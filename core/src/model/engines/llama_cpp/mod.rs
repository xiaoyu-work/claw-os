//! llama.cpp inference engine.
//!
//! Two compilation modes:
//!
//! - **Without the `llama_cpp` cargo feature** — this module compiles as a
//!   stub. `LlamaEngine::new()` returns `EngineError::NotLinked("llama_cpp")`,
//!   `IS_LINKED = false`. The agent provider registry skips the `llama_local`
//!   provider. Lets contributors build cos without llama.cpp on disk.
//!
//! - **With the `llama_cpp` cargo feature** — `build.rs` compiled and linked
//!   the C++ side via CMake. This module exposes safe wrappers over the
//!   minimal FFI in [`ffi`] (backend init/free, opaque types). `new()`
//!   validates config and brings the backend up; concrete model load +
//!   tokenize/decode land in Phase 0.5b once a real GGUF arrives. See the
//!   `real` submodule and [`ffi`] for the rationale.
//!
//! The provider-facing surface ([`LlamaEngine::generate`]) is identical in
//! both modes, so [`crate::agent::llm::providers::llama_local`] can be
//! written once and only its return values change between feature flags.

use std::path::{Path, PathBuf};

use super::EngineError;

#[cfg(feature = "llama_cpp")]
pub mod ffi;

/// Whether the llama.cpp engine is linked into this binary. Read by
/// `engines::engines_linked()` and the provider registry.
#[cfg(feature = "llama_cpp")]
pub const IS_LINKED: bool = true;
#[cfg(not(feature = "llama_cpp"))]
pub const IS_LINKED: bool = false;

/// Static identifier — used by the Provider trait and `engines_linked()`.
#[allow(dead_code)] // Surfaced via stringly typed APIs (Provider::name).
pub const ENGINE_NAME: &str = "llama_cpp";

/// Configuration for instantiating a [`LlamaEngine`].
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields read only inside `cfg(feature = "llama_cpp")`.
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
#[allow(dead_code)] // Fields read only inside `cfg(feature = "llama_cpp")`.
pub struct Generation {
    pub text: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Variants used only inside `cfg(feature = "llama_cpp")`.
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

/// Validate a config without trying to instantiate the engine. Useful for
/// the provider's `is_configured()` check.
#[allow(dead_code)] // Called only inside `cfg(feature = "llama_cpp")` and from tests.
pub fn validate_config(cfg: &LlamaConfig) -> Result<(), EngineError> {
    if cfg.model_path.as_os_str().is_empty() {
        return Err(EngineError::InvalidModelPath(
            "model_path is empty — set agent.model to a GGUF path or 'llama_local:<path>'"
                .into(),
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

// ----------------------------------------------------------------------
// Stub implementation (no `llama_cpp` feature)
// ----------------------------------------------------------------------

#[cfg(not(feature = "llama_cpp"))]
#[derive(Debug)]
#[allow(dead_code)] // Constructed only via `new()` which always errors when feature is off.
pub struct LlamaEngine;

#[cfg(not(feature = "llama_cpp"))]
#[allow(dead_code)] // Construction always fails; methods exist for trait parity.
impl LlamaEngine {
    pub fn new(_cfg: LlamaConfig) -> Result<Self, EngineError> {
        Err(EngineError::NotLinked("llama_cpp"))
    }

    /// Returns the engine config used at construction. Stub never reaches here.
    pub fn config(&self) -> &LlamaConfig {
        unreachable!("stub LlamaEngine cannot be constructed")
    }

    pub async fn generate(&self, _prompt: &str) -> Result<Generation, EngineError> {
        Err(EngineError::NotLinked("llama_cpp"))
    }
}

// ----------------------------------------------------------------------
// Real implementation (with `llama_cpp` feature)
// ----------------------------------------------------------------------

#[cfg(feature = "llama_cpp")]
pub use real::LlamaEngine;

#[cfg(feature = "llama_cpp")]
mod real {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Real llama.cpp engine. Phase 0.5 holds the lifecycle plumbing:
    /// backend init/free and config validation. Model load + decode loop
    /// land in Phase 0.5b once the first GGUF arrives and we can pin
    /// llama_context_params / llama_model_params layouts (or generate
    /// them with bindgen at that point).
    ///
    /// This intentionally does NOT load a model in `new()`. We keep the
    /// `LlamaConfig` so the future implementation has everything it
    /// needs, and we initialise the backend exactly once per process.
    pub struct LlamaEngine {
        cfg: LlamaConfig,
    }

    static BACKEND_UP: AtomicBool = AtomicBool::new(false);

    fn ensure_backend() {
        // SAFETY: llama_backend_init is idempotent. We still gate with an
        // atomic so we don't pay the cost on every construction.
        if BACKEND_UP
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            unsafe {
                ffi::llama_backend_init();
                ffi::llama_log_set(None, std::ptr::null_mut());
            }
        }
    }

    impl LlamaEngine {
        #[allow(dead_code)] // Called from tests; binary path uses real() at runtime.
        pub fn new(cfg: LlamaConfig) -> Result<Self, EngineError> {
            super::validate_config(&cfg)?;
            // Validate the path encodes cleanly on this platform — catches
            // non-UTF-8 paths early so the future load_from_file call
            // can't surprise us.
            let _ = cfg.model_path.to_str().ok_or_else(|| {
                EngineError::InvalidModelPath("non-utf8 model path".into())
            })?;
            ensure_backend();
            Ok(Self { cfg })
        }

        pub fn config(&self) -> &LlamaConfig {
            &self.cfg
        }

        pub async fn generate(&self, _prompt: &str) -> Result<Generation, EngineError> {
            // See `real` module docstring. Returning a clear error here
            // is the deliberate Phase 0.5 boundary.
            Err(EngineError::InferenceFailed(
                "llama_cpp.generate(): wiring complete but tokenize/decode loop pending. \
                 Will land in Phase 0.5b once a GGUF model file is available for testing."
                    .into(),
            ))
        }
    }

    // We never call llama_backend_free() — llama.cpp's docs say it's
    // optional, and dropping a single short-lived engine should not tear
    // down a global the rest of the runtime may share.
}

// ----------------------------------------------------------------------
// Helpers shared by both modes
// ----------------------------------------------------------------------

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
                content: vec![ContentBlock::Text {
                    text: "hi".into(),
                }],
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
        // Always ends with <|assistant|> open tag for completion.
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

    #[cfg(not(feature = "llama_cpp"))]
    #[test]
    fn stub_engine_construction_returns_not_linked() {
        let cfg = LlamaConfig::default();
        let err = LlamaEngine::new(cfg).unwrap_err();
        assert!(
            matches!(err, EngineError::NotLinked("llama_cpp")),
            "expected NotLinked, got {err:?}"
        );
    }

    #[cfg(not(feature = "llama_cpp"))]
    #[test]
    fn engines_linked_excludes_llama_when_feature_off() {
        let linked = super::super::engines_linked();
        assert!(
            !linked.contains(&"llama_cpp"),
            "feature is off but llama_cpp claims to be linked: {linked:?}"
        );
    }

    #[cfg(feature = "llama_cpp")]
    #[test]
    fn engines_linked_includes_llama_when_feature_on() {
        let linked = super::super::engines_linked();
        assert!(linked.contains(&"llama_cpp"));
    }

    /// With the feature on, constructing on a valid (even if not a real
    /// GGUF) path should succeed: `new()` does not load weights yet.
    #[cfg(feature = "llama_cpp")]
    #[test]
    fn real_engine_construction_succeeds_with_valid_path() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-llama-real-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"placeholder").unwrap();
        let mut cfg = LlamaConfig::default();
        cfg.model_path = tmp.clone();
        let engine = LlamaEngine::new(cfg).expect("construction should succeed");
        assert_eq!(engine.config().model_path, tmp);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Even with the feature on, generate() returns the explicit
    /// "pending" error until Phase 0.5b lands the decode loop.
    #[cfg(feature = "llama_cpp")]
    #[tokio::test]
    async fn real_engine_generate_returns_pending_error() {
        let tmp = std::env::temp_dir().join(format!(
            "cos-llama-gen-{}-{}.gguf",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"placeholder").unwrap();
        let mut cfg = LlamaConfig::default();
        cfg.model_path = tmp.clone();
        let engine = LlamaEngine::new(cfg).unwrap();
        let err = engine.generate("hi").await.unwrap_err();
        assert!(matches!(err, EngineError::InferenceFailed(_)));
        let _ = std::fs::remove_file(&tmp);
    }
}
