//! Hand-written FFI to a minimal, version-stable subset of `llama.h`.
//!
//! As of P2.3 the cos binary no longer statically links `libllama`. The
//! engine package manager installs a prebuilt llama.cpp release into
//! `<engines_dir>/llama-cpp/<version>/{bin,lib}/` and we resolve the
//! shared library at runtime via `libloading`. This module therefore
//! provides:
//!
//!   - opaque struct declarations (`llama_model`, `llama_context`,
//!     `llama_sampler`, `llama_vocab`)
//!   - typedefs (`llama_token`, `llama_log_callback`)
//!   - [`LlamaSyms`] — a struct of function pointers resolved on first
//!     load. Each field is an `unsafe extern "C" fn(...)`. Callers go
//!     through `LlamaRuntime::syms` to invoke them; there is no static
//!     `extern "C"` block any more.
//!
//! Why such a tiny surface? `llama_model_params`, `llama_context_params`,
//! `llama_decode`, `llama_sampler_chain_*` are all structs whose layout
//! drifts between llama.cpp versions. Mirroring them by hand is a
//! footgun, so the decode loop (Phase 0.5b) will gain those bindings via
//! bindgen against the active engine's headers. Until then we only need
//! global lifecycle (`init` / `free` / `log_set`) plus the opaque types
//! so the rest of the runtime can compile.

use std::os::raw::{c_char, c_void};

use libloading::Library;

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
#[allow(non_camel_case_types)]
pub type llama_token = i32;

/// Log callback signature: `void (*)(enum ggml_log_level, const char *, void *)`.
/// We treat the level as `i32` and the user-data as opaque.
#[allow(non_camel_case_types)]
pub type llama_log_callback =
    Option<unsafe extern "C" fn(level: i32, text: *const c_char, user_data: *mut c_void)>;

/// Function-pointer table resolved from `libllama`. Each entry mirrors
/// what used to be a static `extern "C"` declaration. New symbols are
/// added here as the decode loop lands in Phase 0.5b.
///
/// `LlamaSyms` is `Send + Sync` because every field is a bare function
/// pointer (no captured state). The owning [`super::runtime::LlamaRuntime`]
/// keeps the [`Library`] alive for as long as any pointer is reachable.
#[allow(non_snake_case)] // Field names mirror llama.h exactly for grep-ability.
pub struct LlamaSyms {
    pub llama_backend_init: unsafe extern "C" fn(),
    pub llama_backend_free: unsafe extern "C" fn(),
    pub llama_log_set: unsafe extern "C" fn(callback: llama_log_callback, user_data: *mut c_void),
}

/// SAFETY: `LlamaSyms` is a plain table of function pointers. There is
/// no interior mutability and no captured `!Send`/`!Sync` state.
unsafe impl Send for LlamaSyms {}
unsafe impl Sync for LlamaSyms {}

impl LlamaSyms {
    /// Resolve every symbol against `lib`. Returns the first
    /// libloading error if any is missing — the runtime treats this as
    /// a hard failure rather than degrading silently, since a missing
    /// symbol almost always means a wildly mismatched llama.cpp build.
    ///
    /// # Safety
    ///
    /// `lib` must point at a real `libllama` produced by an
    /// upstream llama.cpp build. The C ABI signatures of the resolved
    /// symbols must match those declared above. Both invariants are
    /// honored by the official llama.cpp prebuilt releases that
    /// `cos engine` consumes.
    pub unsafe fn resolve(lib: &Library) -> Result<Self, libloading::Error> {
        // `Symbol::into_raw` would let us drop the `Symbol` and keep the
        // raw fn pointer; instead we transmute via the deref. As long as
        // the `Library` outlives the function pointers (it does — the
        // owning `LlamaRuntime` keeps it), this is sound.
        let backend_init: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"llama_backend_init\0")?;
        let backend_free: libloading::Symbol<unsafe extern "C" fn()> =
            lib.get(b"llama_backend_free\0")?;
        let log_set: libloading::Symbol<
            unsafe extern "C" fn(callback: llama_log_callback, user_data: *mut c_void),
        > = lib.get(b"llama_log_set\0")?;
        Ok(Self {
            llama_backend_init: *backend_init,
            llama_backend_free: *backend_free,
            llama_log_set: *log_set,
        })
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/model/engines/llama_cpp/ffi.rs"
    ));
}
