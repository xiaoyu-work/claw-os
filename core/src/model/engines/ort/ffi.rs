//! Hand-written FFI for the ONNX Runtime entry point.
//!
//! As of P2.3 the cos binary no longer statically links `libonnxruntime`.
//! The engine package manager installs a prebuilt onnxruntime release
//! into `<engines_dir>/ort/<version>/{bin,lib}/` and we resolve the
//! shared library at runtime via `libloading`.
//!
//! **Scaffold scope:** only [`OrtSyms`] is bound here, holding the single
//! exported entry point `OrtGetApiBase`. The actual `OrtApiBase` / `OrtApi`
//! vtables are deliberately treated as opaque pointers — we don't bind
//! them at scaffold time because:
//!
//!   1. We never call any vtable function during `load()`. The scaffold
//!      proves the library is intact by resolving the entry point
//!      symbol; everything else lives behind the API version negotiation
//!      that wire-in will perform via `OrtApiBase::GetApi(N)`.
//!   2. Mirroring `OrtApiBase` (and especially `OrtApi`) by hand for the
//!      purpose of *not calling them* costs us future maintenance —
//!      every onnxruntime release that touches the vtable layout would
//!      be a potential ABI footgun.
//!   3. When wire-in lands, the binding will use `extern "system"` for
//!      every fn pointer mirroring an `ORT_API_CALL` marker so
//!      Windows-x86 stdcall is correct (Windows-x86_64 unifies stdcall
//!      with cdecl, so the difference matters only for legacy targets).
//!
//! For the entry point itself, `OrtGetApiBase` is also marked
//! `ORT_API_CALL` in `onnxruntime_c_api.h`, so its function-pointer type
//! uses `extern "system"`.

use std::os::raw::c_void;

use libloading::Library;

/// Function-pointer table resolved from `libonnxruntime`. Currently
/// holds exactly one symbol — the stable entry point that future
/// wire-in code will call to obtain an `OrtApiBase*`.
///
/// `OrtSyms` is `Send + Sync` because the field is a bare function
/// pointer (no captured state). The owning [`super::runtime::OrtRuntime`]
/// keeps the [`Library`] alive for as long as any pointer is reachable.
#[allow(non_snake_case)] // Field names mirror onnxruntime_c_api.h exactly.
pub struct OrtSyms {
    /// `const OrtApiBase* ORT_API_CALL OrtGetApiBase() NO_EXCEPTION;`
    /// Treated as opaque (`*const c_void`) at scaffold time — wire-in
    /// will replace `c_void` with a properly-bound `OrtApiBase` struct.
    pub OrtGetApiBase: unsafe extern "system" fn() -> *const c_void,
}

/// SAFETY: `OrtSyms` is a plain table of function pointers. There is
/// no interior mutability and no captured `!Send`/`!Sync` state.
unsafe impl Send for OrtSyms {}
unsafe impl Sync for OrtSyms {}

impl OrtSyms {
    /// Resolve every symbol against `lib`. Returns the first
    /// libloading error if any is missing.
    ///
    /// # Safety
    ///
    /// `lib` must point at a real `libonnxruntime` produced by
    /// an upstream onnxruntime build. The C ABI signature of the
    /// resolved symbol must match the declaration above. Both
    /// invariants are honored by the official onnxruntime prebuilt
    /// releases that `cos engine` consumes.
    pub unsafe fn resolve(lib: &Library) -> Result<Self, libloading::Error> {
        let get_api_base: libloading::Symbol<unsafe extern "system" fn() -> *const c_void> =
            lib.get(b"OrtGetApiBase\0")?;
        Ok(Self {
            OrtGetApiBase: *get_api_base,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Function pointers are pointer-sized — sanity check that
    /// `OrtSyms` packs as a single fn pointer at scaffold stage.
    #[test]
    fn syms_struct_is_compact() {
        let expected = std::mem::size_of::<usize>(); // one fn ptr
        assert_eq!(std::mem::size_of::<OrtSyms>(), expected);
    }
}
