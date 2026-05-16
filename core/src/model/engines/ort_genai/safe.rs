//! RAII wrappers for the onnxruntime-genai C API.
//!
//! ## Lifetime invariants
//!
//! Every wrapper holds an [`Arc<OrtGenaiRuntime>`] directly (NOT an
//! `Arc` to a parent wrapper). The runtime owns the
//! [`libloading::Library`] whose drop unmaps the DLL — keeping that
//! alive is the only safety contract this module enforces. Parent /
//! child object lifetimes (e.g. tokenizer must outlive its model)
//! are enforced **by the caller** via field declaration order on the
//! struct that owns them. The [`Qwen3GenaiEmbedder::Inner`] struct
//! demonstrates the canonical pattern.
//!
//! ## Thread-safety
//!
//! Every wrapper is `Send` but **not** `Sync`. The upstream C header
//! is explicit that "the API is not thread-safe", so a single
//! `OgaModel` / `OgaTokenizer` / `OgaGenerator` instance must not be
//! invoked concurrently. Sharing & passing instances *across* threads
//! sequentially (the typical async-runtime pattern) is fine. The
//! safe API hands out no `&Self` references that escape the embedder
//! Mutex, so the lack of `Sync` is a hard wall.
//!
//! ## OgaResult ownership
//!
//! `OgaResultGetError(*const OgaResult) -> *const c_char` returns a
//! pointer borrowed *from* the result. Callers must copy the message
//! before calling `OgaDestroyResult`. The [`check_result`] helper
//! does this dance once for every fallible FFI call.

use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;

use super::ffi::{sys, OgaElementType};
use super::runtime::OrtGenaiRuntime;

/// Unified error type for every safe wrapper. Callers translate this
/// into their domain error — the `From` impl in
/// [`super::super::tasks::qwen3_genai`] maps it to `EmbedError`.
#[derive(Debug, thiserror::Error)]
pub enum OrtGenaiError {
    /// The C API returned a non-null `OgaResult*` whose error message
    /// (already copied) is captured here.
    #[error("ort-genai: {0}")]
    Runtime(String),

    /// A pointer-to-string that the C API returned was not valid UTF-8.
    #[error("ort-genai returned non-UTF-8 string")]
    InvalidUtf8,

    /// Path contained a NUL byte (cannot be passed to `OgaCreateModel`).
    #[error("path contains NUL byte: {0}")]
    PathWithNul(String),

    /// Input text contained a NUL byte (cannot be passed to encode).
    #[error("input contains a NUL byte")]
    InputWithNul,

    /// Tensor element type did not match the dtype the caller asked for.
    #[error("tensor element type mismatch: expected {expected:?}, got {actual:?}")]
    TensorTypeMismatch {
        expected: OgaElementType,
        actual: OgaElementType,
    },

    /// Tensor shape was malformed (e.g. rank zero, oversized dim).
    #[error("tensor shape invalid: {0}")]
    TensorShapeMismatch(String),

    /// Tokenizer produced no tokens for the input.
    #[error("tokenizer produced zero tokens")]
    EmptyTokens,
}

/// Take ownership of an `OgaResult*`: copy the error string out, then
/// destroy the result. `Ok(())` if the pointer is null (success).
///
/// # Safety
///
/// `result` must either be null or a pointer freshly returned from a
/// `Oga*` C function. After this call returns, the pointer is owned
/// (and freed) by this helper — callers must not use it again.
unsafe fn check_result(
    rt: &OrtGenaiRuntime,
    result: *mut sys::OgaResult,
) -> Result<(), OrtGenaiError> {
    if result.is_null() {
        return Ok(());
    }
    let err_ptr = (rt.syms.OgaResultGetError)(result);
    let msg = if err_ptr.is_null() {
        String::from("(no error message)")
    } else {
        match CStr::from_ptr(err_ptr).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => String::from("(non-UTF-8 error message)"),
        }
    };
    (rt.syms.OgaDestroyResult)(result);
    Err(OrtGenaiError::Runtime(msg))
}

// =====================================================================
// OgaModel
// =====================================================================

pub struct OgaModel {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaModel,
}

// SAFETY: `ptr` is an opaque handle whose state is mutated only when
// FFI is dispatched via &self/&mut self methods. The runtime is
// Send+Sync. Concurrent access from multiple threads is the caller's
// responsibility — the C API is not thread-safe, so do not.
unsafe impl Send for OgaModel {}

impl OgaModel {
    /// Load a model directory (an Olive-exported `genai` bundle).
    pub fn load(rt: Arc<OrtGenaiRuntime>, dir: &Path) -> Result<Self, OrtGenaiError> {
        let path_str = dir
            .to_str()
            .ok_or_else(|| OrtGenaiError::PathWithNul(dir.display().to_string()))?;
        let c_path =
            CString::new(path_str).map_err(|_| OrtGenaiError::PathWithNul(path_str.to_string()))?;
        let mut out: *mut sys::OgaModel = std::ptr::null_mut();
        // SAFETY: `c_path` lives until end of call; `out` is a stack slot.
        unsafe {
            let r = (rt.syms.OgaCreateModel)(c_path.as_ptr(), &mut out);
            check_result(&rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaCreateModel returned null without an error result".into(),
            ));
        }
        Ok(Self { rt, ptr: out })
    }

    pub(crate) fn as_ptr(&self) -> *const sys::OgaModel {
        self.ptr as *const _
    }

    pub(crate) fn runtime(&self) -> &Arc<OrtGenaiRuntime> {
        &self.rt
    }
}

impl Drop for OgaModel {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: `ptr` was returned by OgaCreateModel and we have
            // exclusive ownership at drop.
            unsafe { (self.rt.syms.OgaDestroyModel)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// =====================================================================
// OgaTokenizer
// =====================================================================

pub struct OgaTokenizer {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaTokenizer,
}

unsafe impl Send for OgaTokenizer {}

impl OgaTokenizer {
    /// Construct a tokenizer for `model`. The returned tokenizer
    /// borrows from `model` internally; the caller must guarantee
    /// `model` outlives every subsequent `encode()` call (the
    /// embedder pattern of holding both behind the same `Mutex`
    /// and dropping the tokenizer first via field-declaration
    /// order satisfies this).
    ///
    /// We deliberately do NOT encode the borrow with a `'m`
    /// lifetime parameter even though that would be the strictest
    /// Rustic option — every container that stores `(OgaModel,
    /// OgaTokenizer)` together (see `model::tasks::qwen3_genai::
    /// Inner`) would otherwise become self-referential and require
    /// `ouroboros` or unsafe `'static` casts.
    pub fn new(model: &OgaModel) -> Result<Self, OrtGenaiError> {
        let rt = model.runtime().clone();
        let mut out: *mut sys::OgaTokenizer = std::ptr::null_mut();
        // SAFETY: model.ptr is a live OgaModel; `out` is a stack slot.
        unsafe {
            let r = (rt.syms.OgaCreateTokenizer)(model.as_ptr(), &mut out);
            check_result(&rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaCreateTokenizer returned null without an error result".into(),
            ));
        }
        Ok(Self { rt, ptr: out })
    }

    /// Encode a string into a fresh `OgaSequences` (always a single
    /// sequence at index 0).
    pub fn encode(&self, text: &str) -> Result<OgaSequences, OrtGenaiError> {
        let c_text = CString::new(text).map_err(|_| OrtGenaiError::InputWithNul)?;
        let seqs = OgaSequences::new(self.rt.clone())?;
        // SAFETY: self.ptr live; c_text alive; seqs.ptr live.
        unsafe {
            let r =
                (self.rt.syms.OgaTokenizerEncode)(self.ptr as *const _, c_text.as_ptr(), seqs.ptr);
            check_result(&self.rt, r)?;
        }
        Ok(seqs)
    }
}

impl Drop for OgaTokenizer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.rt.syms.OgaDestroyTokenizer)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// =====================================================================
// OgaSequences
// =====================================================================

pub struct OgaSequences {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaSequences,
}

unsafe impl Send for OgaSequences {}

impl OgaSequences {
    fn new(rt: Arc<OrtGenaiRuntime>) -> Result<Self, OrtGenaiError> {
        let mut out: *mut sys::OgaSequences = std::ptr::null_mut();
        // SAFETY: out is a stack slot.
        unsafe {
            let r = (rt.syms.OgaCreateSequences)(&mut out);
            check_result(&rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaCreateSequences returned null without an error result".into(),
            ));
        }
        Ok(Self { rt, ptr: out })
    }

    /// Token ids of sequence index 0. Returned slice is borrowed from
    /// the underlying C buffer — valid for the lifetime of `&self`.
    pub fn first_sequence(&self) -> &[i32] {
        // SAFETY: self.ptr is a valid OgaSequences*; index 0 is the
        // sequence written by OgaTokenizerEncode (always present).
        // The returned pointer is owned by the OgaSequences object.
        unsafe {
            let len = (self.rt.syms.OgaSequencesGetSequenceCount)(self.ptr as *const _, 0);
            if len == 0 {
                return &[];
            }
            let data = (self.rt.syms.OgaSequencesGetSequenceData)(self.ptr as *const _, 0);
            if data.is_null() {
                return &[];
            }
            std::slice::from_raw_parts(data, len)
        }
    }
}

impl Drop for OgaSequences {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.rt.syms.OgaDestroySequences)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// =====================================================================
// OgaGeneratorParams
// =====================================================================

pub struct OgaGeneratorParams {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaGeneratorParams,
}

unsafe impl Send for OgaGeneratorParams {}

impl OgaGeneratorParams {
    pub fn new(model: &OgaModel) -> Result<Self, OrtGenaiError> {
        let rt = model.runtime().clone();
        let mut out: *mut sys::OgaGeneratorParams = std::ptr::null_mut();
        // SAFETY: model.ptr live; out is a stack slot.
        unsafe {
            let r = (rt.syms.OgaCreateGeneratorParams)(model.as_ptr(), &mut out);
            check_result(&rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaCreateGeneratorParams returned null without an error result".into(),
            ));
        }
        Ok(Self { rt, ptr: out })
    }

    pub fn set_search_number(&mut self, name: &str, value: f64) -> Result<(), OrtGenaiError> {
        let c_name = CString::new(name).map_err(|_| OrtGenaiError::InputWithNul)?;
        // SAFETY: self.ptr live; c_name alive.
        unsafe {
            let r =
                (self.rt.syms.OgaGeneratorParamsSetSearchNumber)(self.ptr, c_name.as_ptr(), value);
            check_result(&self.rt, r)?;
        }
        Ok(())
    }

    pub(crate) fn as_ptr(&self) -> *const sys::OgaGeneratorParams {
        self.ptr as *const _
    }
}

impl Drop for OgaGeneratorParams {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.rt.syms.OgaDestroyGeneratorParams)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// =====================================================================
// OgaGenerator
// =====================================================================

pub struct OgaGenerator {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaGenerator,
}

unsafe impl Send for OgaGenerator {}

impl OgaGenerator {
    /// Caller must guarantee `model` outlives this generator. (We
    /// don't carry a `'m` lifetime parameter for the same self-
    /// referential reasons documented on [`OgaTokenizer::new`].)
    pub fn new(
        model: &OgaModel,
        params: &OgaGeneratorParams,
    ) -> Result<Self, OrtGenaiError> {
        let rt = model.runtime().clone();
        let mut out: *mut sys::OgaGenerator = std::ptr::null_mut();
        // SAFETY: model.ptr / params.ptr live; out is a stack slot.
        unsafe {
            let r = (rt.syms.OgaCreateGenerator)(model.as_ptr(), params.as_ptr(), &mut out);
            check_result(&rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaCreateGenerator returned null without an error result".into(),
            ));
        }
        Ok(Self { rt, ptr: out })
    }

    /// Append tokens, which triggers a forward pass and materializes
    /// every named output (including `hidden_states`). Empty inputs
    /// are rejected to avoid an undefined-output state.
    pub fn append_tokens(&mut self, ids: &[i32]) -> Result<(), OrtGenaiError> {
        if ids.is_empty() {
            return Err(OrtGenaiError::EmptyTokens);
        }
        // SAFETY: self.ptr live; `ids` outlives the call.
        unsafe {
            let r = (self.rt.syms.OgaGenerator_AppendTokens)(self.ptr, ids.as_ptr(), ids.len());
            check_result(&self.rt, r)?;
        }
        Ok(())
    }

    /// Fetch a named output tensor (e.g. `"hidden_states"`).
    ///
    /// The returned [`OgaTensor`] borrows from `self` — the FFI
    /// tensor reads directly out of the generator's working buffer,
    /// which the next `append_tokens` call would reallocate. Binding
    /// the tensor's lifetime to `&self` makes that aliasing rule a
    /// compile error instead of a use-after-free at runtime.
    pub fn get_output<'g>(&'g self, name: &str) -> Result<OgaTensor<'g>, OrtGenaiError> {
        let c_name = CString::new(name).map_err(|_| OrtGenaiError::InputWithNul)?;
        let mut out: *mut sys::OgaTensor = std::ptr::null_mut();
        // SAFETY: self.ptr live; c_name alive; out is a stack slot.
        unsafe {
            let r = (self.rt.syms.OgaGenerator_GetOutput)(
                self.ptr as *const _,
                c_name.as_ptr(),
                &mut out,
            );
            check_result(&self.rt, r)?;
        }
        if out.is_null() {
            return Err(OrtGenaiError::Runtime(format!(
                "OgaGenerator_GetOutput({name}) returned null"
            )));
        }
        Ok(OgaTensor {
            rt: self.rt.clone(),
            ptr: out,
            _gen: PhantomData,
        })
    }
}

impl Drop for OgaGenerator {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.rt.syms.OgaDestroyGenerator)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

// =====================================================================
// OgaTensor
// =====================================================================

pub struct OgaTensor<'g> {
    rt: Arc<OrtGenaiRuntime>,
    ptr: *mut sys::OgaTensor,
    /// Tensors are non-owning views into the generator's output
    /// buffer. Without this `PhantomData` the borrow checker would
    /// happily let `gen.append_tokens(...)` run while a previous
    /// `OgaTensor` is still alive — the next forward pass then
    /// invalidates the slice returned by `data_f32`. Binding to the
    /// generator's lifetime forces the tensor to drop first.
    _gen: PhantomData<&'g OgaGenerator>,
}

unsafe impl<'g> Send for OgaTensor<'g> {}

impl<'g> OgaTensor<'g> {
    pub fn dtype(&self) -> Result<OgaElementType, OrtGenaiError> {
        let mut t = OgaElementType::Undefined;
        // SAFETY: self.ptr live; &mut t is a stack slot.
        unsafe {
            let r = (self.rt.syms.OgaTensorGetType)(self.ptr, &mut t);
            check_result(&self.rt, r)?;
        }
        Ok(t)
    }

    pub fn shape(&self) -> Result<Vec<i64>, OrtGenaiError> {
        let mut rank: usize = 0;
        // SAFETY: self.ptr live; &mut rank is a stack slot.
        unsafe {
            let r = (self.rt.syms.OgaTensorGetShapeRank)(self.ptr, &mut rank);
            check_result(&self.rt, r)?;
        }
        if rank == 0 {
            return Err(OrtGenaiError::TensorShapeMismatch(
                "tensor rank is 0".into(),
            ));
        }
        if rank > 16 {
            return Err(OrtGenaiError::TensorShapeMismatch(format!(
                "tensor rank {rank} exceeds sanity limit (16)"
            )));
        }
        let mut dims = vec![0i64; rank];
        // SAFETY: dims has `rank` slots; self.ptr live.
        unsafe {
            let r = (self.rt.syms.OgaTensorGetShape)(self.ptr, dims.as_mut_ptr(), rank);
            check_result(&self.rt, r)?;
        }
        Ok(dims)
    }

    /// Returns a slice over the f32 payload. The slice is valid for
    /// `&self`; do not retain across calls that release the tensor.
    pub fn data_f32(&self) -> Result<&[f32], OrtGenaiError> {
        let dt = self.dtype()?;
        if dt != OgaElementType::Float32 {
            return Err(OrtGenaiError::TensorTypeMismatch {
                expected: OgaElementType::Float32,
                actual: dt,
            });
        }
        let shape = self.shape()?;
        let mut total: usize = 1;
        for d in &shape {
            if *d <= 0 {
                return Err(OrtGenaiError::TensorShapeMismatch(format!(
                    "non-positive dim in shape: {shape:?}"
                )));
            }
            total = total
                .checked_mul(*d as usize)
                .ok_or_else(|| OrtGenaiError::TensorShapeMismatch("shape overflow".into()))?;
        }
        let mut data: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: self.ptr live; &mut data is a stack slot.
        unsafe {
            let r = (self.rt.syms.OgaTensorGetData)(self.ptr, &mut data);
            check_result(&self.rt, r)?;
        }
        if data.is_null() {
            return Err(OrtGenaiError::Runtime(
                "OgaTensorGetData returned null pointer".into(),
            ));
        }
        // SAFETY: data points at `total` consecutive f32 in the tensor
        // backing buffer, owned by the tensor for `&self`'s lifetime.
        Ok(unsafe { std::slice::from_raw_parts(data as *const f32, total) })
    }
}

impl<'g> Drop for OgaTensor<'g> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { (self.rt.syms.OgaDestroyTensor)(self.ptr) }
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_in_path_detected() {
        let bad = "C:\\tmp\\with\0nul";
        match CString::new(bad) {
            Err(_) => {}
            Ok(_) => panic!("expected NulError for path containing NUL"),
        }
    }

    #[test]
    fn nul_in_input_detected() {
        let bad = "hello\0world";
        match CString::new(bad) {
            Err(_) => {}
            Ok(_) => panic!("expected NulError for input containing NUL"),
        }
    }

    #[test]
    fn error_display_smoke_test() {
        let e = OrtGenaiError::Runtime("boom".into());
        assert!(format!("{e}").contains("boom"));
        let e = OrtGenaiError::TensorTypeMismatch {
            expected: OgaElementType::Float32,
            actual: OgaElementType::Int64,
        };
        let s = format!("{e}");
        assert!(s.contains("Float32"));
        assert!(s.contains("Int64"));
    }
}
