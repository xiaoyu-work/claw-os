//! Hand-written FFI for the onnxruntime-genai entry point.
//!
//! Mirrors [`super::super::ort::ffi`] — scaffold-only, single-symbol
//! resolution. The probed symbol is `OgaShutdown` because:
//!
//!   1. It's marked `OGA_API_CALL` in `ort_genai_c.h` — same calling
//!      convention conditional as ort's `ORT_API_CALL` — and is exported
//!      from every release as a documented teardown function.
//!   2. We do NOT call it during `load()`. Resolving it serves only as
//!      a "library is intact and exposes the GenAI surface" smoke test.
//!      The real wire-in (model creation, generator stepping, sampling)
//!      lands when a user imports their first GenAI-format model.
//!   3. Unlike `OgaResultGetError` it has no required pointer
//!      arguments, so the eventual call (when wire-in arrives) is safe
//!      with no setup cost.
//!
//! Calling convention: `extern "system"` on every fn pointer. On
//! Windows-x86 this resolves to stdcall (matching `OGA_API_CALL`); on
//! Windows-x86_64 and all non-Windows targets it resolves to the C ABI.

use libloading::Library;

/// Function-pointer table resolved from `libonnxruntime-genai`.
///
/// `OrtGenaiSyms` is `Send + Sync` because the field is a bare function
/// pointer (no captured state). The owning
/// [`super::runtime::OrtGenaiRuntime`] keeps the [`Library`] alive for
/// as long as any pointer is reachable.
#[allow(non_snake_case)] // Field names mirror ort_genai_c.h exactly.
pub struct OrtGenaiSyms {
    /// `OGA_EXPORT void OGA_API_CALL OgaShutdown();`
    /// Stable export probed at scaffold time to confirm the loaded
    /// library exposes the GenAI surface. Not invoked during load.
    pub OgaShutdown: unsafe extern "system" fn(),
}

/// SAFETY: bare fn pointers, no interior mutability.
unsafe impl Send for OrtGenaiSyms {}
unsafe impl Sync for OrtGenaiSyms {}

impl OrtGenaiSyms {
    /// Resolve every symbol against `lib`.
    ///
    /// SAFETY: `lib` must point at a real `libonnxruntime-genai`
    /// produced by an upstream onnxruntime-genai build. The C ABI
    /// signature of the resolved symbol must match the declaration
    /// above. Both invariants are honored by the official prebuilt
    /// releases that `cos engine` consumes.
    pub unsafe fn resolve(lib: &Library) -> Result<Self, libloading::Error> {
        let shutdown: libloading::Symbol<unsafe extern "system" fn()> =
            lib.get(b"OgaShutdown\0")?;
        Ok(Self {
            OgaShutdown: *shutdown,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syms_struct_is_compact() {
        let expected = std::mem::size_of::<usize>(); // one fn ptr
        assert_eq!(std::mem::size_of::<OrtGenaiSyms>(), expected);
    }
}
