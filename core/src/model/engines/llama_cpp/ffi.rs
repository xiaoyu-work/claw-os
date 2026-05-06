//! Hand-written FFI to a minimal, version-stable subset of `llama.h`.
//!
//! Compiled and linked only when the `llama_cpp` cargo feature is on
//! (driven by `core/build.rs`). We deliberately keep the surface tiny:
//!
//!   - opaque struct declarations (`llama_model`, `llama_context`,
//!     `llama_sampler`, `llama_vocab`)
//!   - global lifecycle: [`llama_backend_init`], [`llama_backend_free`],
//!     [`llama_log_set`]
//!
//! The richer surface — `llama_model_params`, `llama_context_params`,
//! `llama_decode`, `llama_sampler_chain_*` — is intentionally NOT mirrored
//! here because those structs contain nested enums / function-pointer
//! members whose layout drifts between llama.cpp versions. Mirroring them
//! by hand is a footgun. The pragmatic plan:
//!
//!   1. Phase 0.5  (now): scaffold + link + verify backend init/free.
//!      Generation calls return [`InferenceFailed`] with a clear pointer
//!      to Phase 0.5b. — done.
//!   2. Phase 0.5b: when the user supplies the first GGUF, switch this
//!      module to bindgen-generated bindings (gated by feature) OR pin
//!      to a specific llama.cpp commit and add the structs by hand.
//!
//! Until then we still get genuine value: build.rs is exercised on every
//! compile-with-feature, the engine's lifecycle plumbing is real, and the
//! provider registration hook is in place so flipping on real generation
//! is a contained change.
//!
//! [`InferenceFailed`]: super::EngineError::InferenceFailed

use std::os::raw::{c_char, c_void};

/// Opaque — defined in `llama.h` (`struct llama_model;`).
#[repr(C)]
pub struct llama_model {
    _private: [u8; 0],
}

/// Opaque — defined in `llama.h` (`struct llama_context;`).
#[repr(C)]
pub struct llama_context {
    _private: [u8; 0],
}

/// Opaque — defined in `llama.h` (`struct llama_sampler;`).
#[repr(C)]
pub struct llama_sampler {
    _private: [u8; 0],
}

/// Opaque — defined in `llama.h` (`struct llama_vocab;`).
#[repr(C)]
pub struct llama_vocab {
    _private: [u8; 0],
}

/// Token type — `typedef int32_t llama_token;` in `llama.h`. Stable for
/// many years.
pub type llama_token = i32;

/// Log callback signature: `void (*)(enum ggml_log_level, const char *, void *)`.
/// We treat the level as `i32` and the user-data as opaque.
pub type llama_log_callback =
    Option<unsafe extern "C" fn(level: i32, text: *const c_char, user_data: *mut c_void)>;

extern "C" {
    /// Initialize the llama.cpp backend. Idempotent on most platforms.
    /// Must be called before any other API. Linked from `libllama`.
    pub fn llama_backend_init();

    /// Tear down the llama.cpp backend. Idempotent.
    pub fn llama_backend_free();

    /// Replace the global log callback. Pass `None` to silence.
    pub fn llama_log_set(log_callback: llama_log_callback, user_data: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check: opaque types are zero-sized so callers can't
    /// accidentally try to construct one.
    #[test]
    fn opaque_types_are_zst() {
        assert_eq!(std::mem::size_of::<llama_model>(), 0);
        assert_eq!(std::mem::size_of::<llama_context>(), 0);
        assert_eq!(std::mem::size_of::<llama_sampler>(), 0);
        assert_eq!(std::mem::size_of::<llama_vocab>(), 0);
    }

    /// Smoke: bring the backend up and straight back down. Verifies the
    /// CMake-driven static link in `build.rs` actually produced a working
    /// `libllama` whose globals can be initialised.
    #[test]
    fn backend_lifecycle_round_trip() {
        unsafe {
            llama_backend_init();
            // Silence any logging the backend might emit during teardown.
            llama_log_set(None, std::ptr::null_mut());
            llama_backend_free();
        }
    }
}
