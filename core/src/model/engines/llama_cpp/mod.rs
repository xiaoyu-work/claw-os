//! llama.cpp inference engine — runtime-loaded.
//!
//! As of P2.3, `libllama` is no longer compile-time linked. The cos
//! binary always builds; an engine is *available* iff the engine
//! package manager has installed an active version on disk:
//!
//! ```text
//! <engines_dir>/llama-cpp/<version>/lib/llama.dll        (Windows)
//!                                  /libllama.so          (Linux)
//!                                  /libllama.dylib       (macOS)
//! ```
//!
//! The user controls availability with `cos engine update llama-cpp`,
//! `cos engine activate <ver>`, etc. — see `core/src/engine_pkg/`.
//!
//! Three failure modes are distinguished so `cos agent status` can give
//! actionable hints:
//!
//!   - [`EngineError::NotInstalled`]: no active version, or the active
//!     version's library file is missing.
//!   - [`EngineError::LibraryLoadFailed`]: the file exists but the OS
//!     loader rejected it (corrupt, ABI mismatch, missing sister DLL,
//!     missing C runtime, ...).
//!   - [`EngineError::InvalidModelPath`]: GGUF file argument is bad.
//!
//! [`is_installed`] reads only the registry + a stat — cheap enough to
//! call from `cos agent status`. Actual loading happens lazily on the
//! first `LlamaEngine::new()` call and is cached process-wide.
//!
//! Concrete inference (tokenize/decode loop) lands in Phase 0.5b once a
//! GGUF model file is available for testing. Until then `generate()`
//! returns the explicit "pending" error.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::EngineError;

pub mod ffi;
pub mod runtime;

/// Static identifier — used by the Provider trait and `engines_linked()`.
/// Note: this is the agent-side / FFI-side stem (`llama_cpp`), not the
/// engine package manager's kebab-case ID (`llama-cpp`). Both are kept
/// stable so existing `cos agent status` and registry consumers don't
/// have to change.
pub const ENGINE_NAME: &str = "llama_cpp";

/// Engine name used by `crate::engine_pkg` (kebab-case, matches GitHub
/// repo path). Internal helper so we don't sprinkle the literal across
/// callers.
pub const PKG_ENGINE_NAME: &str = "llama-cpp";

/// Configuration for instantiating a [`LlamaEngine`].
#[derive(Debug, Clone)]
pub struct LlamaConfig {
    /// Path to the GGUF file. Must exist and be readable.
    pub model_path: PathBuf,
    /// Maximum context window in tokens (0 = use the model's training value).
    pub n_ctx: u32,
    /// Number of threads for CPU inference. 0 = let llama.cpp pick.
    pub n_threads: u32,
    /// Number of layers to offload to GPU. 0 = pure CPU. -1 = offload all.
    pub n_gpu_layers: i32,
    /// Generation length cap, in tokens.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
}

impl Default for LlamaConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::new(),
            n_ctx: 0,
            n_threads: 0,
            n_gpu_layers: 0,
            max_tokens: 512,
            temperature: 0.7,
        }
    }
}

/// Result of a single generation pass.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields read once Phase 0.5b lands the decode loop.
pub struct Generation {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Variants surface once Phase 0.5b lands.
pub enum StopReason {
    /// Model emitted EOS.
    Eos,
    /// Hit `max_tokens`.
    MaxTokens,
    /// Hit a configured stop sequence.
    StopSequence,
    /// Other (cancelled, error converted to stop, etc.).
    Other,
}

/// Cheap availability check. Returns true iff the engine package
/// manager has an active version of `llama-cpp` whose shared library
/// file exists on disk. **Does not load the library.**
///
/// Used by `engines_linked()` and the `llama_local` provider's
/// `is_configured()`. The actual load can still fail (corrupt file,
/// missing sister DLL, ABI mismatch) — that surfaces as
/// `LibraryLoadFailed` from `LlamaEngine::new()`.
pub fn is_installed() -> bool {
    crate::engine_pkg::active_library_path(PKG_ENGINE_NAME, "llama").is_some()
}

/// Validate a config without trying to instantiate the engine. Useful
/// for the provider's `is_configured()` check.
#[allow(dead_code)] // Called from tests; binary path goes through `new()`.
pub fn validate_config(cfg: &LlamaConfig) -> Result<(), EngineError> {
    if cfg.model_path.as_os_str().is_empty() {
        return Err(EngineError::InvalidModelPath(
            "model_path is empty — set agent.model to a GGUF path or 'llama_local:<path>'".into(),
        ));
    }
    if !cfg.model_path.is_file() {
        return Err(EngineError::InvalidModelPath(format!(
            "{} is not a regular file",
            cfg.model_path.display()
        )));
    }
    Ok(())
}

/// Single in-process llama.cpp engine. Phase 0.5 holds the lifecycle
/// plumbing: backend init/free + config validation. Model load + decode
/// loop land in Phase 0.5b once a real GGUF arrives.
///
/// Multiple `LlamaEngine` instances share the same loaded `libllama`
/// (via [`runtime::LlamaRuntime::shared`]) and the same global backend
/// init.
pub struct LlamaEngine {
    cfg: LlamaConfig,
    /// Kept alive so the function-pointer references in `syms` stay
    /// valid for as long as this engine exists.
    runtime: Arc<runtime::LlamaRuntime>,
}

/// `llama_backend_init` is a process-wide global initialiser. We pay
/// the cost exactly once, the first time any `LlamaEngine` is built.
static BACKEND_UP: AtomicBool = AtomicBool::new(false);

fn ensure_backend(rt: &runtime::LlamaRuntime) {
    if BACKEND_UP
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        // SAFETY: `llama_backend_init` is documented as the required
        // first call to the API and is safe to invoke once. The
        // function pointer was just resolved from the loaded library.
        unsafe {
            (rt.syms.llama_backend_init)();
            // Silence the global log unless the user wires their own
            // callback (none of the consumers do today).
            (rt.syms.llama_log_set)(None, std::ptr::null_mut());
        }
    }
}

impl LlamaEngine {
    pub fn new(cfg: LlamaConfig) -> Result<Self, EngineError> {
        validate_config(&cfg)?;
        // Catches non-UTF-8 paths early so future load_from_file calls
        // can't surprise us.
        let _ = cfg
            .model_path
            .to_str()
            .ok_or_else(|| EngineError::InvalidModelPath("non-utf8 model path".into()))?;
        let runtime = runtime::LlamaRuntime::shared()?;
        ensure_backend(&runtime);
        Ok(Self { cfg, runtime })
    }

    pub fn config(&self) -> &LlamaConfig {
        &self.cfg
    }

    /// Path of the loaded `libllama` — surfaced by status output so
    /// users can confirm which version is in use.
    #[allow(dead_code)] // Status integration lands later in this phase.
    pub fn library_path(&self) -> &Path {
        &self.runtime.lib_path
    }

    /// Engine version derived from the **loaded** library path, NOT from
    /// the engine_pkg registry. Matters for audit-trail correctness: the
    /// process-wide `LlamaRuntime` cache may hold the previously-active
    /// version even after `cos engine activate <new>` ran (the user
    /// must restart the daemon for the new version to take effect).
    /// The registry would falsely report the new version; this returns
    /// what's actually executing.
    ///
    /// Returns `None` if the path doesn't match the expected
    /// `<engines_dir>/<engine>/<version>/{lib,bin}/<lib-file>` shape
    /// (e.g. test-injected path).
    pub fn engine_version(&self) -> Option<String> {
        engine_version_from_lib_path(&self.runtime.lib_path)
    }

    pub async fn generate(&self, _prompt: &str) -> Result<Generation, EngineError> {
        Err(EngineError::InferenceFailed(
            "llama_cpp.generate(): wiring complete but tokenize/decode loop pending. \
             Will land in Phase 0.5b once a GGUF model file is available for testing."
                .into(),
        ))
    }
}

/// Extract `<version>` from `<engines_dir>/llama-cpp/<version>/{lib,bin}/<file>`.
/// Pulled out so unit tests can pin the layout-parsing logic.
pub(crate) fn engine_version_from_lib_path(lib_path: &Path) -> Option<String> {
    // .../<engine>/<version>/<lib-or-bin>/<file>
    //                ^^^^^^^^^                   parent.parent.file_name
    let version_dir = lib_path.parent()?.parent()?;
    version_dir.file_name()?.to_str().map(str::to_string)
}

// We never call llama_backend_free() — llama.cpp's docs say it's
// optional, and dropping a single short-lived engine should not tear
// down a global the rest of the runtime may share.

/// Best-effort: render a chat history into a single prompt string. The
/// model-specific chat template is applied later by llama.cpp's
/// `llama_chat_apply_template` once generation lands; for now we use a
/// simple delimited format good enough for raw-completion models.
pub fn render_messages_as_prompt(
    system: Option<&str>,
    messages: &[crate::agent::llm::Message],
) -> String {
    let mut out = String::new();
    if let Some(s) = system {
        out.push_str("<|system|>\n");
        out.push_str(s);
        out.push('\n');
    }
    for msg in messages {
        let role = match msg.role {
            crate::agent::llm::Role::System => "system",
            crate::agent::llm::Role::User => "user",
            crate::agent::llm::Role::Assistant => "assistant",
            crate::agent::llm::Role::Tool => "tool",
        };
        out.push_str("<|");
        out.push_str(role);
        out.push_str("|>\n");
        for block in &msg.content {
            if let crate::agent::llm::ContentBlock::Text { text } = block {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out.push_str("<|assistant|>\n");
    out
}

/// Cheap path check for the provider's `is_configured()` — does NOT load
/// the model. Returns true if the path exists and is a regular file.
pub fn model_path_is_usable(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{ContentBlock, Message, Role};

    #[test]
    fn engine_name_is_stable() {
        assert_eq!(ENGINE_NAME, "llama_cpp");
        assert_eq!(PKG_ENGINE_NAME, "llama-cpp");
    }

    #[test]
    fn validate_config_rejects_empty_path() {
        let cfg = LlamaConfig::default();
        let err = validate_config(&cfg).unwrap_err();
        assert!(matches!(err, EngineError::InvalidModelPath(_)));
    }

    #[test]
    fn validate_config_rejects_missing_file() {
        let mut cfg = LlamaConfig::default();
        cfg.model_path = PathBuf::from("/this/path/should/not/exist.gguf");
        let err = validate_config(&cfg).unwrap_err();
        assert!(matches!(err, EngineError::InvalidModelPath(_)));
    }

    #[test]
    fn validate_config_accepts_existing_file() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-llama-fake-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"fake gguf bytes").unwrap();
        let mut cfg = LlamaConfig::default();
        cfg.model_path = tmp.clone();
        assert!(validate_config(&cfg).is_ok());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn model_path_is_usable_handles_empty_and_missing() {
        assert!(!model_path_is_usable(Path::new("")));
        assert!(!model_path_is_usable(Path::new("/nonexistent/model.gguf")));
    }

    #[test]
    fn render_messages_includes_all_roles() {
        let msgs = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "hello".into(),
                }],
            },
        ];
        let p = render_messages_as_prompt(Some("you are helpful"), &msgs);
        assert!(p.contains("<|system|>"));
        assert!(p.contains("you are helpful"));
        assert!(p.contains("<|user|>"));
        assert!(p.contains("hi"));
        assert!(p.contains("<|assistant|>"));
        assert!(p.contains("hello"));
        assert!(p.trim_end().ends_with("<|assistant|>"));
    }

    #[test]
    fn render_messages_skips_non_text_blocks() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "look".into(),
                },
                ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "...".into(),
                },
            ],
        }];
        let p = render_messages_as_prompt(None, &msgs);
        assert!(p.contains("look"));
        assert!(!p.contains("image/png"));
    }

    /// With no engine installed and no test override, `is_installed()`
    /// returns false. Uses an empty temp engines dir so we don't see
    /// whatever the host has.
    #[test]
    fn is_installed_false_when_no_active_engine() {
        let tmp = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));
        assert!(!is_installed());
        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// With an active version recorded but the dll file missing,
    /// `is_installed()` is still false — we never claim availability
    /// based purely on JSON.
    #[test]
    fn is_installed_false_when_active_dll_missing() {
        let tmp = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        // Hand-craft engines.json with active=v0 but no actual file.
        let engines_dir = tmp.path();
        std::fs::create_dir_all(engines_dir.join("llama-cpp/v0/lib")).unwrap();
        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "v0",
                    "previous": "",
                    "installed": [{"version": "v0", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            engines_dir.join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        assert!(!is_installed(), "no dll on disk -> not installed");

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// With both the registry entry and the dll file present (any file
    /// — we don't try to load), `is_installed()` returns true.
    #[test]
    fn is_installed_true_when_active_dll_present() {
        let tmp = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        let engines_dir = tmp.path();
        let lib_dir = engines_dir.join("llama-cpp/v0/lib");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let lib_name = if cfg!(target_os = "windows") {
            "llama.dll"
        } else if cfg!(target_os = "macos") {
            "libllama.dylib"
        } else {
            "libllama.so"
        };
        std::fs::write(lib_dir.join(lib_name), b"placeholder").unwrap();

        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "v0",
                    "previous": "",
                    "installed": [{"version": "v0", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            engines_dir.join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        assert!(is_installed(), "dll on disk -> installed");

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// Picks up the bin/-rooted layout (Windows zip ships flat under
    /// `bin/`). The helper falls through `lib/` first, then `bin/`.
    #[test]
    fn is_installed_finds_bin_layout() {
        let tmp = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        let engines_dir = tmp.path();
        let bin_dir = engines_dir.join("llama-cpp/v0/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let lib_name = if cfg!(target_os = "windows") {
            "llama.dll"
        } else if cfg!(target_os = "macos") {
            "libllama.dylib"
        } else {
            "libllama.so"
        };
        std::fs::write(bin_dir.join(lib_name), b"placeholder").unwrap();

        let json = serde_json::json!({
            "version": 1,
            "engines": {
                "llama-cpp": {
                    "active": "v0",
                    "previous": "",
                    "installed": [{"version": "v0", "installed_at": "2026-01-01T00:00:00Z", "bytes": 0, "source": "", "sha256": ""}],
                    "pinned": false,
                    "channel": "release",
                    "accelerator": "",
                    "source": ""
                }
            }
        });
        std::fs::write(
            engines_dir.join("engines.json"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        assert!(is_installed(), "dll under bin/ should still count");

        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// Constructing without an installed engine surfaces NotInstalled
    /// — the cleaner of the two failure modes (vs LibraryLoadFailed,
    /// which is for "installed but broken").
    #[test]
    fn engine_construction_returns_not_installed_when_uninstalled() {
        let tmp = tempfile::tempdir().unwrap();
        crate::engine_pkg::paths::set_engines_dir_override(Some(tmp.path().to_path_buf()));

        // Provide a real GGUF placeholder so validate_config passes.
        let gguf = std::env::temp_dir().join(format!(
            "cos-llama-not-installed-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&gguf, b"placeholder").unwrap();

        let mut cfg = LlamaConfig::default();
        cfg.model_path = gguf.clone();

        // Skip the test if the host process happens to have already
        // cached a real runtime in the OnceLock — this can occur if
        // an earlier integration test loaded a real llama-cpp engine.
        // Use the test override to ensure we get a deterministic
        // resolution path: clear it (no override), and rely on the
        // empty engines_dir to drive `NotInstalled`.
        runtime::set_test_override(None);

        match LlamaEngine::new(cfg) {
            Err(EngineError::NotInstalled(_)) => {} // expected
            // If the OnceLock already cached a real runtime we'd reach
            // generate() pending instead — accept that too.
            Err(EngineError::InferenceFailed(_)) => {}
            Ok(_) => {
                let _ = std::fs::remove_file(&gguf);
                crate::engine_pkg::paths::set_engines_dir_override(None);
                panic!("engine should not have constructed without an installed runtime");
            }
            Err(other) => {
                let _ = std::fs::remove_file(&gguf);
                crate::engine_pkg::paths::set_engines_dir_override(None);
                panic!("expected NotInstalled, got {other:?}");
            }
        }

        let _ = std::fs::remove_file(&gguf);
        crate::engine_pkg::paths::set_engines_dir_override(None);
    }

    /// Pinning the parsing of the on-disk layout — engine_version() must
    /// derive `b4001` from `.../llama-cpp/b4001/lib/llama.dll` and from
    /// the bin/ variant. Negative cases must return None rather than a
    /// surprising substring (e.g. `lib`, `bin`, etc.).
    #[test]
    fn engine_version_from_lib_path_handles_layouts() {
        let lib_layout = PathBuf::from("/var/lib/cos/engines/llama-cpp/b4001/lib/libllama.so");
        assert_eq!(
            super::engine_version_from_lib_path(&lib_layout).as_deref(),
            Some("b4001"),
        );

        let bin_layout =
            PathBuf::from("C:/ProgramData/cos/data/engines/llama-cpp/b4001/bin/llama.dll");
        assert_eq!(
            super::engine_version_from_lib_path(&bin_layout).as_deref(),
            Some("b4001"),
        );

        // Nonsense path shorter than the expected depth returns None,
        // not a misleading "lib" or "tmp".
        let too_short = PathBuf::from("/tmp/llama.dll");
        assert!(super::engine_version_from_lib_path(&too_short).is_none());

        let just_a_filename = PathBuf::from("llama.dll");
        assert!(super::engine_version_from_lib_path(&just_a_filename).is_none());
    }
}
