use super::*;

fn mock_cfg() -> crate::config::AgentConfig {
    // AgentConfig::default() now leaves provider empty ("not configured").
    // Tests in this module that want to exercise mock-provider behaviour
    // (legacy "default") must opt in explicitly.
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    cfg
}

fn unconfigured_cfg() -> crate::config::AgentConfig {
    crate::config::AgentConfig::default()
}

// The wizard tests below mutate the process-wide COS_CONFIG_PATH
// env var and write to that path. Cargo runs tests in parallel, so
// we serialize them through a static mutex to avoid races.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[test]
fn is_ready_blocks_on_mock_provider() {
    let err = is_ready(&mock_cfg()).unwrap_err();
    assert!(err.contains("agent not configured"));
    assert!(err.contains("mock"));
    assert!(err.contains("cos agent setup"));
}

#[test]
fn truncate_for_log_handles_non_ascii() {
    // Regression: `&s[..max]` panics if `max` lands inside a
    // multi-byte UTF-8 codepoint. Provider error bodies routinely
    // include localised text — Anthropic, Bedrock, and Vertex all
    // surface non-ASCII in their /models error envelopes — so the
    // wizard's "couldn't list models" path must never panic on
    // bytes-vs-chars.
    // ASCII unchanged.
    assert_eq!(truncate_for_log("hello", 100), "hello");
    // Long ASCII gets truncated to exactly `max` chars + ellipsis.
    let long_ascii = "x".repeat(300);
    let out = truncate_for_log(&long_ascii, 10);
    assert!(out.starts_with("xxxxxxxxxx"));
    assert!(out.ends_with('…'));
    // Multi-byte input: every byte budget that lands mid-codepoint
    // must yield a clean string, never a panic, and the result
    // must be valid UTF-8 ending on a char boundary.
    for max in 1..200 {
        let s = "漢字".repeat(50); // 300 bytes, 100 chars
        let out = truncate_for_log(&s, max);
        // Round-tripping confirms validity: invalid UTF-8 would
        // refuse to construct a &str.
        assert!(out.is_char_boundary(0));
        for (i, _) in out.char_indices() {
            assert!(out.is_char_boundary(i));
        }
    }
    // Emoji at the boundary.
    let s = "abc🌍def";
    // max=4 lands inside the emoji (bytes 0..3 = "abc", emoji starts at 3,
    // 4 bytes wide). Truncate must walk back to byte 3.
    let out = truncate_for_log(s, 4);
    assert!(out.contains("abc"));
    assert!(out.ends_with('…'));
}

#[test]
fn is_ready_blocks_on_unconfigured_default() {
    // AgentConfig::default() leaves provider empty so an out-of-the-box
    // install can never accidentally run AI calls against `mock`.
    let err = is_ready(&unconfigured_cfg()).unwrap_err();
    assert!(err.contains("agent not configured"));
    assert!(err.contains("no LLM provider"));
    assert!(err.contains("cos agent setup"));
}

#[test]
fn is_ready_passes_for_llama_local_without_credential() {
    let mut cfg = mock_cfg();
    cfg.provider = "llama_local".into();
    assert!(is_ready(&cfg).is_ok());
}

#[test]
fn is_ready_blocks_real_provider_with_no_credential() {
    let mut cfg = mock_cfg();
    cfg.provider = "anthropic".into();
    cfg.model = "claude-3-5-sonnet".into();
    // Ensure no env var fallback can rescue us in CI:
    cfg.api_key_credential = Some("definitely_not_present".into());
    cfg.api_key_env = Some("__COS_TEST_DEFINITELY_UNSET__".into());
    std::env::remove_var("__COS_TEST_DEFINITELY_UNSET__");
    let err = is_ready(&cfg).unwrap_err();
    assert!(err.contains("no credential found"));
    assert!(err.contains("cos agent setup"));
}

#[test]
fn is_ready_passes_when_env_credential_present() {
    let mut cfg = mock_cfg();
    cfg.provider = "anthropic".into();
    let env_name = "__COS_TEST_API_KEY_PRESENT__";
    cfg.api_key_env = Some(env_name.into());
    std::env::set_var(env_name, "sk-fake");
    assert!(is_ready(&cfg).is_ok());
    std::env::remove_var(env_name);
}

#[test]
fn is_ready_accepts_usable_local_fallback() {
    let mut cfg = mock_cfg();
    cfg.provider = "anthropic".into();
    cfg.model = "primary-model".into();
    cfg.api_key_credential = Some("definitely_not_present".into());
    cfg.api_key_env = Some("__COS_TEST_PRIMARY_UNSET__".into());
    std::env::remove_var("__COS_TEST_PRIMARY_UNSET__");
    cfg.provider_fallbacks = vec![crate::config::ProviderFallbackConfig {
        provider: "llama_local".into(),
        model: "local-default".into(),
        api_key_credential: None,
        api_key_env: None,
        api_key_credentials: Vec::new(),
        api_key_envs: Vec::new(),
        base_url: None,
        extra_headers: std::collections::HashMap::new(),
        request_timeout: None,
        pool_strategy: None,
        pool_cooldown_secs: None,
        ..Default::default()
    }];
    assert!(is_ready(&cfg).is_ok());
}

#[test]
fn unknown_subcommand_is_rejected() {
    let err = run(&["bogus".into()]).unwrap_err();
    assert!(
        err.contains("unknown setup modality/subcommand"),
        "got {err}"
    );
}

#[test]
fn bare_setup_on_non_tty_requires_modality() {
    // Cargo test may inherit a TTY stdin in interactive shells; in
    // that case `run` would block on the modality picker. Skip
    // unless stdin is actually piped (which it is in CI).
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        eprintln!("(skipping: stdin is a TTY in this test run)");
        return;
    }
    let err = run(&[]).unwrap_err();
    assert!(err.contains("requires a modality"), "got {err}");
    // The envelope should also list the valid modalities so callers
    // can self-correct.
    for m in ["llm", "tts", "stt", "imagegen", "embed", "all"] {
        assert!(err.contains(m), "expected `{m}` in envelope; got {err}");
    }
}

#[test]
fn bare_status_defaults_to_all_modalities() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    // No positional modality, just --status: should fan out to all.
    let v = run(&["--status".into()]).expect("status ok");
    let modalities = v
        .get("modalities")
        .and_then(|s| s.as_object())
        .expect("modalities map");
    for k in ["llm", "tts", "stt", "imagegen", "embed"] {
        assert!(
            modalities.contains_key(k),
            "missing modality `{k}` in bare status"
        );
    }

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn unknown_flag_is_rejected() {
    let err = run(&["--bogus".into()]).unwrap_err();
    assert!(err.contains("unknown setup flag"), "got {err}");
}

#[test]
fn help_subcommand_lists_modes() {
    let v = run(&["--help".into()]).expect("help ok");
    let cmd = v.get("command").and_then(|s| s.as_str()).unwrap_or("");
    assert!(
        cmd.starts_with("cos agent setup <MODALITY>"),
        "expected help command to start with `cos agent setup <MODALITY>`; got `{cmd}`"
    );
    let modalities = v.get("modalities").and_then(|s| s.as_object());
    assert!(modalities.is_some(), "expected modalities table in help");
    let m = modalities.unwrap();
    for k in ["llm", "tts", "stt", "imagegen", "embed", "all"] {
        assert!(m.contains_key(k), "expected modality `{k}` in help");
    }
}

#[test]
fn modality_parses_aliases() {
    assert_eq!(Modality::parse("llm"), Some(Modality::Llm));
    assert_eq!(Modality::parse("speech"), Some(Modality::Tts));
    assert_eq!(Modality::parse("asr"), Some(Modality::Stt));
    assert_eq!(Modality::parse("image"), Some(Modality::ImageGen));
    assert_eq!(Modality::parse("embeddings"), Some(Modality::Embed));
    assert_eq!(Modality::parse("all"), Some(Modality::All));
    assert_eq!(Modality::parse("nope"), None);
}

#[test]
fn reset_writes_mock_provider_to_config() {
    let _g = env_lock();
    use std::path::PathBuf;
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path: PathBuf = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);
    // Pre-seed with a non-mock provider.
    std::fs::write(
        &cfg_path,
        r#"{"agent":{"provider":"anthropic","model":"claude-3-5-sonnet"}}"#,
    )
    .unwrap();

    let v = reset_cmd(Modality::Llm).expect("reset ok");
    assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));

    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed["agent"]["provider"].as_str(),
        Some("mock"),
        "expected reset to write provider=mock, got {text}"
    );

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn reset_media_writes_none_provider_to_config() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);
    std::fs::write(
            &cfg_path,
            r#"{"tts":{"provider":"openai","model":"tts-1","api_key_credential":"tts_openai_api_key"}}"#,
        )
        .unwrap();

    let v = reset_cmd(Modality::Tts).expect("reset ok");
    assert_eq!(v.get("ok").and_then(|b| b.as_bool()), Some(true));

    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["tts"]["provider"].as_str(), Some("none"));
    assert!(parsed["tts"]["api_key_credential"].is_null());

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn status_returns_provider_and_ready_flag() {
    let _g = env_lock();
    // Use a tmp config path so we don't depend on the user's real config.
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let v = status_cmd(Modality::Llm).expect("status ok");
    assert!(v.get("ready").and_then(|b| b.as_bool()).is_some());
    assert!(v.get("provider").and_then(|s| s.as_str()).is_some());

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn config_path_defaults_to_user_config_dir() {
    // Regression: agent config used to default to /etc/cos/config.json,
    // which non-root users couldn't write to — saving Azure in
    // cosmic-settings then silently failed. With per-user paths,
    // config_path() must land under COS_USER_CONFIG_DIR.
    let _g = env_lock();
    let tmp =
        std::env::temp_dir().join(format!("cos-setup-user-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp).unwrap();
    let prev_path = std::env::var_os("COS_CONFIG_PATH");
    let prev_user = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::remove_var("COS_CONFIG_PATH");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);

    let p = config_path();
    assert_eq!(p, tmp.join("config.json"));
    // Round-trip: write+read should both succeed under the user dir
    // (no root needed). This is the bug fix in action.
    write_config_atomic(&p, &json!({"agent": {"provider": "azure"}})).expect("write ok");
    let v = read_config_or_empty(&p).expect("read ok");
    assert_eq!(v["agent"]["provider"], "azure");

    match prev_user {
        Some(v) => std::env::set_var("COS_USER_CONFIG_DIR", v),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    if let Some(v) = prev_path {
        std::env::set_var("COS_CONFIG_PATH", v);
    }
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn status_all_reports_every_modality() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let v = status_cmd(Modality::All).expect("status ok");
    let modalities = v
        .get("modalities")
        .and_then(|s| s.as_object())
        .expect("modalities map");
    for k in ["llm", "tts", "stt", "imagegen", "embed"] {
        assert!(
            modalities.contains_key(k),
            "missing modality `{k}` in status"
        );
    }

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

// ---- Azure first-class support --------------------------------------

#[test]
fn resolve_base_url_appends_api_version_when_missing_query() {
    let got = resolve_base_url_args(
        "azure",
        Some("https://acme.openai.azure.com/"),
        Some("2024-12-01-preview"),
    )
    .unwrap();
    assert_eq!(
        got.as_deref(),
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
    );
}

#[test]
fn resolve_base_url_preserves_existing_query() {
    let got = resolve_base_url_args(
        "azure",
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview&foo=bar"),
        Some("ignored-because-base-already-has-query"),
    )
    .unwrap();
    assert_eq!(
        got.as_deref(),
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview&foo=bar")
    );
}

#[test]
fn resolve_base_url_azure_rejects_full_deployment_url() {
    let err = resolve_base_url_args(
        "azure",
        Some("https://acme.openai.azure.com/openai/deployments/gpt-5.4"),
        Some("2024-12-01-preview"),
    )
    .unwrap_err();
    assert!(err.contains("resource root"), "msg was: {err}");
}

#[test]
fn resolve_base_url_azure_rejects_responses_endpoint() {
    let err = resolve_base_url_args(
        "azure",
        Some("https://acme.openai.azure.com/openai/responses"),
        Some("2025-04-01-preview"),
    )
    .unwrap_err();
    assert!(err.contains("resource root"), "msg was: {err}");
}

#[test]
fn resolve_base_url_azure_requires_base_url() {
    let err = resolve_base_url_args("azure", None, Some("2024-12-01-preview")).unwrap_err();
    assert!(err.contains("azure"), "{err}");
    assert!(err.contains("--base-url"), "{err}");
}

#[test]
fn resolve_base_url_non_azure_accepts_no_override() {
    let got = resolve_base_url_args("openai", None, None).unwrap();
    assert!(got.is_none());
}

#[test]
fn resolve_base_url_non_azure_accepts_override() {
    let got = resolve_base_url_args("openai", Some("https://my.proxy/v1"), None).unwrap();
    assert_eq!(got.as_deref(), Some("https://my.proxy/v1"));
}

#[test]
fn split_base_url_parses_api_version_query() {
    let (endpoint, version) = split_base_url_and_api_version(Some(
        "https://acme.openai.azure.com/openai/deployments/dep?api-version=2024-12-01-preview",
    ));
    assert_eq!(
        endpoint.as_deref(),
        Some("https://acme.openai.azure.com/openai/deployments/dep")
    );
    assert_eq!(version.as_deref(), Some("2024-12-01-preview"));
}

#[test]
fn split_base_url_handles_no_query() {
    let (endpoint, version) = split_base_url_and_api_version(Some("https://api.openai.com/v1"));
    assert_eq!(endpoint.as_deref(), Some("https://api.openai.com/v1"));
    assert!(version.is_none());
}

#[test]
fn split_base_url_handles_none() {
    let (endpoint, version) = split_base_url_and_api_version(None);
    assert!(endpoint.is_none());
    assert!(version.is_none());
}

#[test]
fn split_base_url_handles_empty_string() {
    let (endpoint, version) = split_base_url_and_api_version(Some(""));
    assert!(endpoint.is_none());
    assert!(version.is_none());
}

#[test]
fn split_base_url_handles_query_without_api_version() {
    let (endpoint, version) =
        split_base_url_and_api_version(Some("https://api.openai.com/v1?foo=bar"));
    assert_eq!(endpoint.as_deref(), Some("https://api.openai.com/v1"));
    assert!(version.is_none());
}

#[test]
fn extra_fields_for_azure_lists_endpoint_and_api_version() {
    let fields = extra_fields_for("azure");
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0]["key"].as_str(), Some("base_url"));
    assert_eq!(fields[0]["required"].as_bool(), Some(true));
    assert_eq!(fields[1]["key"].as_str(), Some("api_version"));
    assert_eq!(fields[1]["required"].as_bool(), Some(false));
}

#[test]
fn extra_fields_for_non_azure_is_empty() {
    assert!(extra_fields_for("openai").is_empty());
    assert!(extra_fields_for("anthropic").is_empty());
    assert!(extra_fields_for("mock").is_empty());
}

#[test]
fn providers_cmd_llm_includes_azure_with_extra_fields() {
    let v = providers_cmd(Modality::Llm).expect("providers ok");
    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers list");
    let azure = providers
        .iter()
        .find(|p| p["name"] == "azure")
        .expect("azure provider entry");
    let extras = azure["extra_fields"]
        .as_array()
        .expect("extra_fields array");
    assert_eq!(extras.len(), 2);
    assert_eq!(extras[0]["key"], "base_url");
    assert_eq!(extras[1]["key"], "api_version");
    // Non-azure provider: extra_fields present but empty.
    let openai = providers
        .iter()
        .find(|p| p["name"] == "openai")
        .expect("openai provider entry");
    assert!(openai["extra_fields"].as_array().unwrap().is_empty());
}

#[test]
fn providers_cmd_llm_hides_mock_and_llama_local() {
    // The GUI catalogue consumed by cosmic-settings and
    // cosmic-initial-setup must not expose `mock` (test-only) or
    // `llama_local` (managed via `cos model load`). Both remain
    // buildable through the registry for power users / tests.
    let v = providers_cmd(Modality::Llm).expect("providers ok");
    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers list");
    let names: Vec<&str> = providers
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(
        !names.iter().any(|n| *n == "mock"),
        "mock leaked into GUI catalogue: {names:?}"
    );
    assert!(
        !names.iter().any(|n| *n == "llama_local"),
        "llama_local leaked into GUI catalogue: {names:?}"
    );
    // Sanity: real providers are still there.
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"anthropic"));
}

#[test]
fn providers_cmd_llm_marks_copilot_as_oauth_device_and_no_one_else() {
    // The catalog is the contract between kernel and UI: only OAuth
    // providers carry the `auth_kind` field. Adding a second OAuth
    // provider should require a deliberate edit to this test.
    let v = providers_cmd(Modality::Llm).expect("providers ok");
    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers list");
    let copilot = providers
        .iter()
        .find(|p| p["name"] == "copilot")
        .expect("copilot entry must be exposed to UIs");
    assert_eq!(
        copilot.get("auth_kind").and_then(|v| v.as_str()),
        Some("oauth_device"),
        "copilot must advertise auth_kind=oauth_device for the UI to render the sign-in branch"
    );
    // Live-fetched: static catalog list must be empty so the UI knows
    // to call `models --provider copilot` after sign-in.
    assert!(
        copilot["models"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "copilot model list must be empty in the static catalog (live-fetched post-sign-in)"
    );
    assert_eq!(
        copilot.get("default_model").and_then(|v| v.as_str()),
        Some("gpt-4o"),
        "copilot default_model is a placeholder until live discovery succeeds"
    );
    // Nobody else should look like an OAuth provider.
    for p in providers {
        if p["name"] == "copilot" {
            continue;
        }
        assert!(
            p.get("auth_kind").is_none(),
            "non-copilot provider `{}` must not advertise auth_kind",
            p["name"]
        );
    }
}

#[test]
fn oauth_subcommands_reject_non_copilot_provider() {
    // The error surface is the UI's safety net: typos in --provider
    // must surface as actionable errors, not crashes.
    let err = oauth_start_cmd(Some("openai")).expect_err("non-copilot provider rejected");
    assert!(err.contains("does not use OAuth device flow"), "{err}");
    let err =
        oauth_poll_cmd(Some("openai"), Some("xxx")).expect_err("non-copilot provider rejected");
    assert!(err.contains("does not use OAuth device flow"), "{err}");
    let err = models_cmd(Some("openai")).expect_err("models only supports copilot today");
    assert!(err.contains("only supported for `copilot`"), "{err}");
}

#[test]
fn oauth_poll_requires_device_code() {
    let err = oauth_poll_cmd(Some("copilot"), None).expect_err("missing device-code rejected");
    assert!(err.contains("--device-code"), "{err}");
    let err = oauth_poll_cmd(Some("copilot"), Some("  ")).expect_err("blank device-code rejected");
    assert!(err.contains("--device-code"), "{err}");
}

#[test]
fn model_names_from_values_trims_and_drops_invalid_entries() {
    let names = model_names_from_values(vec![
        json!({"name": " gpt-4o "}),
        json!({"name": ""}),
        json!({"id": "missing-name"}),
        json!({"name": "claude-sonnet-4.5"}),
    ]);
    assert_eq!(names, vec!["gpt-4o", "claude-sonnet-4.5"]);
}

#[test]
fn require_provider_rejects_empty_and_missing() {
    assert!(require_provider(None, "oauth-start").is_err());
    assert!(require_provider(Some(""), "oauth-start").is_err());
    assert!(require_provider(Some("   "), "oauth-start").is_err());
    assert_eq!(
        require_provider(Some(" copilot "), "oauth-start").unwrap(),
        "copilot"
    );
}

#[test]
fn user_facing_providers_hides_mock_and_llama_local() {
    // Same contract as the GUI catalogue, but for the interactive
    // `cos agent setup llm` wizard's provider picker — it consumes
    // `user_facing_providers()` instead of `available_providers()`
    // so neither test-only `mock` nor `llama_local` (managed via
    // `cos model load`) show up in the numbered list. Power users
    // can still set them via `apply --provider mock ...`.
    let names = user_facing_providers();
    assert!(
        !names.iter().any(|n| *n == "mock"),
        "mock leaked: {names:?}"
    );
    assert!(
        !names.iter().any(|n| *n == "llama_local"),
        "llama_local leaked: {names:?}"
    );
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"anthropic"));
}

#[test]
fn azure_apply_without_base_url_errors() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let err = run(&[
        "llm".into(),
        "apply".into(),
        "--provider".into(),
        "azure".into(),
        "--model".into(),
        "my-deployment".into(),
        "--api-key-env".into(),
        "__AZURE_TEST_KEY__".into(),
    ])
    .unwrap_err();
    assert!(err.contains("azure"), "{err}");
    assert!(err.contains("--base-url"), "{err}");

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn azure_apply_persists_base_url_with_api_version() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let v = run(&[
        "llm".into(),
        "apply".into(),
        "--provider".into(),
        "azure".into(),
        "--model".into(),
        "my-deployment".into(),
        "--base-url".into(),
        "https://acme.openai.azure.com/".into(),
        "--api-version".into(),
        "2024-12-01-preview".into(),
        "--api-key-env".into(),
        "__AZURE_TEST_KEY__".into(),
    ])
    .expect("apply ok");

    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["provider"].as_str(), Some("azure"));
    assert_eq!(
        v["base_url"].as_str(),
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
    );

    let text = std::fs::read_to_string(&cfg_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["agent"]["provider"].as_str(), Some("azure"));
    assert_eq!(parsed["agent"]["model"].as_str(), Some("my-deployment"));
    assert_eq!(
        parsed["agent"]["base_url"].as_str(),
        Some("https://acme.openai.azure.com/?api-version=2024-12-01-preview")
    );

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn azure_apply_rejects_deployment_in_base_url() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let err = run(&[
        "llm".into(),
        "apply".into(),
        "--provider".into(),
        "azure".into(),
        "--model".into(),
        "my-deployment".into(),
        "--base-url".into(),
        "https://acme.openai.azure.com/openai/deployments/my-deployment".into(),
        "--api-version".into(),
        "2024-12-01-preview".into(),
        "--api-key-env".into(),
        "__AZURE_TEST_KEY__".into(),
    ])
    .unwrap_err();
    assert!(err.contains("resource root"), "msg was: {err}");

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn status_media_reports_base_url_and_api_version_split() {
    // status_media reads fresh from disk per call (unlike
    // status_llm which uses the OnceLock-cached config), so we can
    // exercise the new endpoint/api_version fields here without
    // racing the global config. Azure isn't a media provider in
    // the spec catalogue, so simulate the same persisted shape on
    // a TTS block (the parser is provider-agnostic).
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);
    std::fs::write(
            &cfg_path,
            r#"{"tts":{"provider":"openai","model":"tts-1","base_url":"https://acme.example.com/v1?api-version=2024-12-01-preview","api_key_env":"__TTS_TEST_KEY__"}}"#,
        )
        .unwrap();

    let v = status_cmd(Modality::Tts).expect("status ok");
    assert_eq!(v["provider"].as_str(), Some("openai"));
    assert_eq!(
        v["base_url"].as_str(),
        Some("https://acme.example.com/v1?api-version=2024-12-01-preview")
    );
    assert_eq!(v["endpoint"].as_str(), Some("https://acme.example.com/v1"));
    assert_eq!(v["api_version"].as_str(), Some("2024-12-01-preview"));

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn status_media_reason_is_envelope_shape_when_not_ready() {
    // Regression: status_media used to emit `reason` as a plain
    // string (e.g. `"`tts` not configured (provider=none)"`). The
    // cosmic-settings UI deserialises `reason` into `ErrorEnvelope
    // { error, details, fix }`, so a bare string made every media
    // settings page fail with "invalid status JSON: invalid type:
    // string, expected struct ErrorEnvelope". Now mirror the
    // envelope shape `status_llm` already emits via `is_ready`.
    //
    // Each modality is explicitly pinned to `provider=none` so we
    // deterministically exercise the "not configured" envelope
    // branch without depending on `config::get()` (a OnceLock that
    // gets seeded by whichever test ran first).
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);
    std::fs::write(
        &cfg_path,
        r#"{
                "tts":{"provider":"none"},
                "stt":{"provider":"none"},
                "imagegen":{"provider":"none"},
                "embed":{"provider":"none"}
            }"#,
    )
    .unwrap();

    for m in [
        Modality::Tts,
        Modality::Stt,
        Modality::ImageGen,
        Modality::Embed,
    ] {
        let v = status_cmd(m).expect("status ok");
        assert_eq!(
            v["ready"],
            json!(false),
            "{}: provider=none should not be ready",
            m.name()
        );
        let reason = v.get("reason").expect("reason key");
        assert!(
            reason.is_object(),
            "{}: reason must be a JSON envelope, got {reason}",
            m.name()
        );
        assert!(
            reason["error"].as_str().is_some_and(|s| !s.is_empty()),
            "{}: envelope.error must be a non-empty string",
            m.name()
        );
        assert!(
            reason["details"].as_str().is_some_and(|s| !s.is_empty()),
            "{}: envelope.details must be a non-empty string",
            m.name()
        );
    }

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn status_embed_auto_prompts_when_local_stack_missing() {
    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    let missing_model_dir = tmp_dir.join("missing-qwen3");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);
    std::fs::write(
        &cfg_path,
        json!({
            "embed": {
                "provider": "auto",
                "model_dir": missing_model_dir.display().to_string()
            }
        })
        .to_string(),
    )
    .unwrap();

    let v = status_cmd(Modality::Embed).expect("status ok");
    assert_eq!(v["ready"], json!(false));
    assert_eq!(v["provider"].as_str(), Some("none"));
    assert_eq!(
        v["reason"]["error"].as_str(),
        Some("embedding not configured")
    );
    assert_eq!(v["reason"]["fix"].as_str(), Some("cos agent setup embed"));
    assert!(
        v["reason"]["details"]
            .as_str()
            .is_some_and(|s| s.contains("model dir does not exist")),
        "unexpected reason: {}",
        v["reason"]
    );

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}

#[test]
fn providers_embed_defaults_away_from_local_when_unavailable() {
    let expected = if local_embed_precheck(&json!({}), None).is_err() {
        "openai"
    } else {
        "local"
    };
    let v = providers_cmd(Modality::Embed).expect("providers ok");
    assert_eq!(v["default_provider"].as_str(), Some(expected));
}

#[test]
fn apply_embed_local_rejects_missing_local_stack() {
    if local_embed_precheck(&json!({}), None).is_ok() {
        eprintln!("(skipping: local embedding stack is installed)");
        return;
    }

    let _g = env_lock();
    let tmp_dir =
        std::env::temp_dir().join(format!("cos-setup-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let cfg_path = tmp_dir.join("config.json");
    std::env::set_var("COS_CONFIG_PATH", &cfg_path);

    let err = run(&[
        "embed".into(),
        "apply".into(),
        "--provider".into(),
        "local".into(),
        "--model".into(),
        "qwen3-embedding-0.6b".into(),
    ])
    .expect_err("local apply should fail when stack is unavailable");
    let envelope: serde_json::Value = serde_json::from_str(&err).expect("json envelope");
    assert_eq!(
        envelope["error"].as_str(),
        Some("local embedding stack unavailable")
    );
    assert_eq!(envelope["fix"].as_str(), Some("cos agent setup embed"));

    std::env::remove_var("COS_CONFIG_PATH");
    std::fs::remove_dir_all(&tmp_dir).ok();
}
