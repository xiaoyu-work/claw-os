//! Hand-written FFI for `onnxruntime-genai` (header
//! `microsoft/onnxruntime-genai/src/ort_genai_c.h`, v0.12.x).
//!
//! Calling convention is `extern "system"` because the C header
//! declares every exported function with `OGA_API_CALL`, defined as
//! `_stdcall` on `_WIN32` and empty (= cdecl) elsewhere. `extern
//! "system"` resolves to the platform's "winapi" calling convention
//! on Windows-x86 (stdcall), to the C ABI on Windows-x86_64, and to
//! the C ABI on every non-Windows target — which is exactly what
//! `OGA_API_CALL` macro-expands to.
//!
//! ## Lifetime invariants
//!
//! Every safe wrapper in [`super::safe`] **must** retain an
//! `Arc<super::runtime::OrtGenaiRuntime>`. The runtime owns the
//! `Library` whose [`Drop`] would unmap the DLL — once that happens
//! every fn pointer in [`OrtGenaiSyms`] becomes a dangling call into
//! unmapped memory. The wrappers, *not* this struct, are responsible
//! for keeping the library alive across calls.
//!
//! ## Thread safety
//!
//! The upstream header is explicit: **the API is not thread-safe.**
//! Concurrent calls into the same model / tokenizer / generator from
//! multiple threads are undefined behaviour. The safe wrappers in
//! [`super::safe`] therefore serialize all access through a `Mutex`
//! at the embedder boundary; this FFI struct itself makes no Send/Sync
//! claim about the resources it dispatches against.

use std::ffi::c_void;
use std::os::raw::c_char;

/// Opaque types matching the typedefs in `ort_genai_c.h`. We never
/// dereference these from Rust — only pass them back to the C API.
pub mod sys {
    use super::c_void;

    #[repr(C)]
    pub struct OgaResult {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaModel {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaTokenizer {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaSequences {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaGeneratorParams {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaGenerator {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
    #[repr(C)]
    pub struct OgaTensor {
        _u: [u8; 0],
        _p: std::marker::PhantomData<*mut c_void>,
    }
}

/// Mirror of `enum OgaElementType` in `ort_genai_c.h`. Only the few
/// values we actually inspect are spelled out — the rest are caught
/// by the wildcard arm in [`super::safe::OgaTensor::data_f32`].
#[repr(i32)]
#[allow(non_camel_case_types, dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OgaElementType {
    Undefined = 0,
    Float32 = 1,
    Uint8 = 2,
    Int8 = 3,
    Uint16 = 4,
    Int16 = 5,
    Int32 = 6,
    Int64 = 7,
    String = 8,
    Bool = 9,
    Float16 = 10,
    Float64 = 11,
    Uint32 = 12,
    Uint64 = 13,
    Complex64 = 14,
    Complex128 = 15,
    Bfloat16 = 16,
}

/// Function-pointer table resolved from `libonnxruntime-genai`.
///
/// The symbol surface covers everything the embedder path calls plus
/// the pre-existing `OgaShutdown` smoke-test export. Adding a new
/// pointer here requires three things:
///
///   1. A `pub field: unsafe extern "system" fn(...)` declaration with
///      the exact signature from `ort_genai_c.h`.
///   2. A `lib.get(b"FieldName\0")?` resolution in [`Self::resolve`].
///   3. (For wire-up) A method on the corresponding safe wrapper that
///      dispatches through this struct.
///
/// Field names mirror the C symbols verbatim (PascalCase + underscore
/// where the header uses one). Snake-case lint is suppressed at the
/// struct level.
#[allow(non_snake_case)]
pub struct OrtGenaiSyms {
    // --- Lifetime / error helpers ---
    pub OgaShutdown: unsafe extern "system" fn(),
    pub OgaResultGetError: unsafe extern "system" fn(*const sys::OgaResult) -> *const c_char,
    pub OgaDestroyResult: unsafe extern "system" fn(*mut sys::OgaResult),
    pub OgaDestroyString: unsafe extern "system" fn(*const c_char),

    // --- Model ---
    pub OgaCreateModel:
        unsafe extern "system" fn(*const c_char, *mut *mut sys::OgaModel) -> *mut sys::OgaResult,
    pub OgaDestroyModel: unsafe extern "system" fn(*mut sys::OgaModel),

    // --- Tokenizer ---
    pub OgaCreateTokenizer: unsafe extern "system" fn(
        *const sys::OgaModel,
        *mut *mut sys::OgaTokenizer,
    ) -> *mut sys::OgaResult,
    pub OgaDestroyTokenizer: unsafe extern "system" fn(*mut sys::OgaTokenizer),
    pub OgaTokenizerEncode: unsafe extern "system" fn(
        *const sys::OgaTokenizer,
        *const c_char,
        *mut sys::OgaSequences,
    ) -> *mut sys::OgaResult,

    // --- Sequences ---
    pub OgaCreateSequences:
        unsafe extern "system" fn(*mut *mut sys::OgaSequences) -> *mut sys::OgaResult,
    pub OgaDestroySequences: unsafe extern "system" fn(*mut sys::OgaSequences),
    pub OgaSequencesGetSequenceCount:
        unsafe extern "system" fn(*const sys::OgaSequences, usize) -> usize,
    pub OgaSequencesGetSequenceData:
        unsafe extern "system" fn(*const sys::OgaSequences, usize) -> *const i32,

    // --- Generator params ---
    pub OgaCreateGeneratorParams: unsafe extern "system" fn(
        *const sys::OgaModel,
        *mut *mut sys::OgaGeneratorParams,
    ) -> *mut sys::OgaResult,
    pub OgaDestroyGeneratorParams: unsafe extern "system" fn(*mut sys::OgaGeneratorParams),
    pub OgaGeneratorParamsSetSearchNumber: unsafe extern "system" fn(
        *mut sys::OgaGeneratorParams,
        *const c_char,
        f64,
    ) -> *mut sys::OgaResult,

    // --- Generator ---
    pub OgaCreateGenerator: unsafe extern "system" fn(
        *const sys::OgaModel,
        *const sys::OgaGeneratorParams,
        *mut *mut sys::OgaGenerator,
    ) -> *mut sys::OgaResult,
    pub OgaDestroyGenerator: unsafe extern "system" fn(*mut sys::OgaGenerator),
    pub OgaGenerator_AppendTokens:
        unsafe extern "system" fn(*mut sys::OgaGenerator, *const i32, usize) -> *mut sys::OgaResult,
    pub OgaGenerator_GetOutput: unsafe extern "system" fn(
        *const sys::OgaGenerator,
        *const c_char,
        *mut *mut sys::OgaTensor,
    ) -> *mut sys::OgaResult,

    // --- Tensor ---
    pub OgaDestroyTensor: unsafe extern "system" fn(*mut sys::OgaTensor),
    pub OgaTensorGetType:
        unsafe extern "system" fn(*mut sys::OgaTensor, *mut OgaElementType) -> *mut sys::OgaResult,
    pub OgaTensorGetShapeRank:
        unsafe extern "system" fn(*mut sys::OgaTensor, *mut usize) -> *mut sys::OgaResult,
    pub OgaTensorGetShape:
        unsafe extern "system" fn(*mut sys::OgaTensor, *mut i64, usize) -> *mut sys::OgaResult,
    pub OgaTensorGetData:
        unsafe extern "system" fn(*mut sys::OgaTensor, *mut *mut c_void) -> *mut sys::OgaResult,
}

/// SAFETY: bare fn pointers, no captured state. The owning
/// [`super::runtime::OrtGenaiRuntime`] keeps the [`libloading::Library`]
/// alive for as long as any pointer in this struct is dereferenceable.
unsafe impl Send for OrtGenaiSyms {}
unsafe impl Sync for OrtGenaiSyms {}

impl OrtGenaiSyms {
    /// Resolve every symbol against `lib`.
    ///
    /// # Safety
    ///
    /// `lib` must point at a real `libonnxruntime-genai`
    /// produced by an upstream onnxruntime-genai build. Each symbol
    /// must match the C signature declared above. Both invariants are
    /// honored by the official prebuilt releases that `cos engine`
    /// consumes.
    pub unsafe fn resolve(lib: &libloading::Library) -> Result<Self, libloading::Error> {
        macro_rules! sym {
            ($lib:expr, $name:literal, $ty:ty) => {{
                // SAFETY: `$lib` is alive for the duration of `resolve`,
                // and the caller asserts the symbol's signature matches
                // the macro-supplied function-pointer type.
                let s: libloading::Symbol<$ty> = $lib.get(concat!($name, "\0").as_bytes())?;
                *s
            }};
        }

        Ok(Self {
            OgaShutdown: sym!(lib, "OgaShutdown", unsafe extern "system" fn()),

            OgaResultGetError: sym!(
                lib,
                "OgaResultGetError",
                unsafe extern "system" fn(*const sys::OgaResult) -> *const c_char
            ),
            OgaDestroyResult: sym!(
                lib,
                "OgaDestroyResult",
                unsafe extern "system" fn(*mut sys::OgaResult)
            ),
            OgaDestroyString: sym!(
                lib,
                "OgaDestroyString",
                unsafe extern "system" fn(*const c_char)
            ),

            OgaCreateModel: sym!(
                lib,
                "OgaCreateModel",
                unsafe extern "system" fn(
                    *const c_char,
                    *mut *mut sys::OgaModel,
                ) -> *mut sys::OgaResult
            ),
            OgaDestroyModel: sym!(
                lib,
                "OgaDestroyModel",
                unsafe extern "system" fn(*mut sys::OgaModel)
            ),

            OgaCreateTokenizer: sym!(
                lib,
                "OgaCreateTokenizer",
                unsafe extern "system" fn(
                    *const sys::OgaModel,
                    *mut *mut sys::OgaTokenizer,
                ) -> *mut sys::OgaResult
            ),
            OgaDestroyTokenizer: sym!(
                lib,
                "OgaDestroyTokenizer",
                unsafe extern "system" fn(*mut sys::OgaTokenizer)
            ),
            OgaTokenizerEncode: sym!(
                lib,
                "OgaTokenizerEncode",
                unsafe extern "system" fn(
                    *const sys::OgaTokenizer,
                    *const c_char,
                    *mut sys::OgaSequences,
                ) -> *mut sys::OgaResult
            ),

            OgaCreateSequences: sym!(
                lib,
                "OgaCreateSequences",
                unsafe extern "system" fn(*mut *mut sys::OgaSequences) -> *mut sys::OgaResult
            ),
            OgaDestroySequences: sym!(
                lib,
                "OgaDestroySequences",
                unsafe extern "system" fn(*mut sys::OgaSequences)
            ),
            OgaSequencesGetSequenceCount: sym!(
                lib,
                "OgaSequencesGetSequenceCount",
                unsafe extern "system" fn(*const sys::OgaSequences, usize) -> usize
            ),
            OgaSequencesGetSequenceData: sym!(
                lib,
                "OgaSequencesGetSequenceData",
                unsafe extern "system" fn(*const sys::OgaSequences, usize) -> *const i32
            ),

            OgaCreateGeneratorParams: sym!(
                lib,
                "OgaCreateGeneratorParams",
                unsafe extern "system" fn(
                    *const sys::OgaModel,
                    *mut *mut sys::OgaGeneratorParams,
                ) -> *mut sys::OgaResult
            ),
            OgaDestroyGeneratorParams: sym!(
                lib,
                "OgaDestroyGeneratorParams",
                unsafe extern "system" fn(*mut sys::OgaGeneratorParams)
            ),
            OgaGeneratorParamsSetSearchNumber: sym!(
                lib,
                "OgaGeneratorParamsSetSearchNumber",
                unsafe extern "system" fn(
                    *mut sys::OgaGeneratorParams,
                    *const c_char,
                    f64,
                ) -> *mut sys::OgaResult
            ),

            OgaCreateGenerator: sym!(
                lib,
                "OgaCreateGenerator",
                unsafe extern "system" fn(
                    *const sys::OgaModel,
                    *const sys::OgaGeneratorParams,
                    *mut *mut sys::OgaGenerator,
                ) -> *mut sys::OgaResult
            ),
            OgaDestroyGenerator: sym!(
                lib,
                "OgaDestroyGenerator",
                unsafe extern "system" fn(*mut sys::OgaGenerator)
            ),
            OgaGenerator_AppendTokens: sym!(
                lib,
                "OgaGenerator_AppendTokens",
                unsafe extern "system" fn(
                    *mut sys::OgaGenerator,
                    *const i32,
                    usize,
                ) -> *mut sys::OgaResult
            ),
            OgaGenerator_GetOutput: sym!(
                lib,
                "OgaGenerator_GetOutput",
                unsafe extern "system" fn(
                    *const sys::OgaGenerator,
                    *const c_char,
                    *mut *mut sys::OgaTensor,
                ) -> *mut sys::OgaResult
            ),

            OgaDestroyTensor: sym!(
                lib,
                "OgaDestroyTensor",
                unsafe extern "system" fn(*mut sys::OgaTensor)
            ),
            OgaTensorGetType: sym!(
                lib,
                "OgaTensorGetType",
                unsafe extern "system" fn(
                    *mut sys::OgaTensor,
                    *mut OgaElementType,
                ) -> *mut sys::OgaResult
            ),
            OgaTensorGetShapeRank: sym!(
                lib,
                "OgaTensorGetShapeRank",
                unsafe extern "system" fn(*mut sys::OgaTensor, *mut usize) -> *mut sys::OgaResult
            ),
            OgaTensorGetShape: sym!(
                lib,
                "OgaTensorGetShape",
                unsafe extern "system" fn(
                    *mut sys::OgaTensor,
                    *mut i64,
                    usize,
                ) -> *mut sys::OgaResult
            ),
            OgaTensorGetData: sym!(
                lib,
                "OgaTensorGetData",
                unsafe extern "system" fn(
                    *mut sys::OgaTensor,
                    *mut *mut c_void,
                ) -> *mut sys::OgaResult
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct holds 25 fn pointers; assert layout to catch
    /// accidental reordering or duplicate-field bugs in resolve().
    #[test]
    fn syms_struct_size_matches_field_count() {
        const FN_COUNT: usize = 25;
        let expected = std::mem::size_of::<usize>() * FN_COUNT;
        assert_eq!(std::mem::size_of::<OrtGenaiSyms>(), expected);
    }

    #[test]
    fn element_type_float32_is_one() {
        assert_eq!(OgaElementType::Float32 as i32, 1);
    }
}
