use super::*;
use crate::agent::llm::Message;

fn req(model: &str, text: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![Message::user_text(text)],
        system: Some("you are a local model".into()),
        tools: vec![],
        tool_choice: Default::default(),
        max_tokens: Some(64),
        temperature: Some(0.5),
        top_p: None,
        stop_sequences: vec![],
        extra: serde_json::Value::Null,
    }
}

#[test]
fn extract_model_path_strips_prefix() {
    assert_eq!(
        extract_model_path("llama_local:/tmp/m.gguf"),
        PathBuf::from("/tmp/m.gguf")
    );
    assert_eq!(
        extract_model_path("/abs/m.gguf"),
        PathBuf::from("/abs/m.gguf")
    );
    assert_eq!(
        extract_model_path("model.gguf"),
        PathBuf::from("model.gguf")
    );
}

#[test]
fn name_is_stable() {
    let p = LlamaLocalProvider::new("/tmp/x.gguf", &AgentConfig::default());
    assert_eq!(p.name(), "llama_local");
}

#[test]
fn supported_models_echoes_spec() {
    let p = LlamaLocalProvider::new("llama_local:/tmp/x.gguf", &AgentConfig::default());
    let m = p.supported_models();
    assert_eq!(m, vec!["llama_local:/tmp/x.gguf".to_string()]);
}

#[test]
fn is_configured_false_when_path_missing() {
    let p =
        LlamaLocalProvider::new("/this/path/should/not/exist.gguf", &AgentConfig::default());
    assert!(!p.is_configured());
}

/// `is_configured()` ANDs file presence with engine-installed-on-disk.
/// Without an installed engine, even a real model file should not
/// flip it on. This test pins the engines_dir to an empty temp
/// directory so the host's actual install (if any) doesn't leak in.
#[test]
fn is_configured_requires_installed_engine() {
    let tmp_engines = tempfile::tempdir().unwrap();
    crate::engine_pkg::paths::set_engines_dir_override(Some(tmp_engines.path().to_path_buf()));

    let tmp_gguf = std::env::temp_dir().join(format!(
        "cos-llama-prov-{}-{}.gguf",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&tmp_gguf, b"placeholder").unwrap();
    let spec = tmp_gguf.to_string_lossy().to_string();
    let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());

    assert!(
        !p.is_configured(),
        "no engine installed -> not configured even with model file"
    );

    let _ = std::fs::remove_file(&tmp_gguf);
    crate::engine_pkg::paths::set_engines_dir_override(None);
}

/// With both an engine *and* a model file installed, the provider
/// reports configured. This is the "happy path" Phase 0.5 status
/// will display once a user has run `cos engine update llama-cpp`.
#[test]
fn is_configured_true_when_engine_and_model_present() {
    let tmp_engines = tempfile::tempdir().unwrap();
    crate::engine_pkg::paths::set_engines_dir_override(Some(tmp_engines.path().to_path_buf()));

    // Stand up a fake "installed engine".
    let lib_dir = tmp_engines.path().join("llama-cpp/v0/lib");
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
        tmp_engines.path().join("engines.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();

    // And a fake GGUF.
    let gguf = std::env::temp_dir().join(format!(
        "cos-llama-prov-happy-{}-{}.gguf",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&gguf, b"placeholder").unwrap();
    let spec = gguf.to_string_lossy().to_string();
    let p = LlamaLocalProvider::new(&spec, &AgentConfig::default());

    assert!(p.is_configured());

    let _ = std::fs::remove_file(&gguf);
    crate::engine_pkg::paths::set_engines_dir_override(None);
}

/// With no engine installed, `chat()` surfaces `NotConfigured` so
/// the user gets a clear pointer to `cos engine update`.
#[tokio::test]
async fn chat_without_engine_returns_not_configured() {
    let tmp_engines = tempfile::tempdir().unwrap();
    // The provider runs `is_installed()` from a tokio worker via
    // `spawn_blocking`. The thread-local override lives on the
    // current thread only, so the worker would see the host's real
    // engines dir. Bypass by calling chat() — its initial path
    // through `ensure_engine().await` runs on the current task
    // until it hits `spawn_blocking`. The `is_installed()` check
    // happens BEFORE `spawn_blocking`, so the override applies.
    crate::engine_pkg::paths::set_engines_dir_override(Some(tmp_engines.path().to_path_buf()));

    let p = LlamaLocalProvider::new("/tmp/anything.gguf", &AgentConfig::default());
    let err = p.chat(req("/tmp/anything.gguf", "hi")).await.unwrap_err();
    match err {
        LlmError::NotConfigured(msg) => {
            assert!(
                msg.contains("llama-cpp") || msg.contains("cos engine"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected NotConfigured, got {other:?}"),
    }

    crate::engine_pkg::paths::set_engines_dir_override(None);
}
