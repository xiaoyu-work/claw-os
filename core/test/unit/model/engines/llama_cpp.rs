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
