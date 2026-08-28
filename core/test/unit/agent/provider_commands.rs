use super::*;

#[test]
fn providers_cmd_lists_every_registered_provider() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers array");
    let names: std::collections::HashSet<_> = arr
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
        .collect();
    for &expected in llm::available_providers().iter() {
        assert!(
            names.contains(expected),
            "providers_cmd missing {expected}: got {names:?}"
        );
    }
    assert_eq!(
        v.get("count").and_then(|c| c.as_u64()).unwrap_or(0),
        llm::available_providers().len() as u64
    );
}

#[test]
fn providers_cmd_marks_active_provider() {
    let active = crate::config::get().agent.provider.clone();
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    let active_entries: Vec<_> = arr
        .iter()
        .filter(|e| e.get("active") == Some(&serde_json::Value::Bool(true)))
        .collect();
    if active.is_empty() {
        // Fresh-install default: no provider configured, so no entry
        // is marked active. The CLI output is the source of truth
        // here — it reports `active: ""` and zero active entries.
        assert_eq!(
            active_entries.len(),
            0,
            "no entry should be active when provider is unconfigured"
        );
        assert_eq!(
            v.get("active").and_then(|a| a.as_str()),
            Some(""),
            "active field should be the empty string"
        );
    } else {
        assert_eq!(active_entries.len(), 1, "exactly one active provider");
        assert_eq!(
            active_entries[0].get("name").and_then(|n| n.as_str()),
            Some(active.as_str())
        );
    }
}

#[test]
fn providers_cmd_filters_by_names_flag() {
    let v = providers_cmd(&["--names".into(), "openai,anthropic".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 2);
    let names: Vec<_> = arr
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"anthropic"));
}

#[test]
fn providers_cmd_filter_drops_unknown_names_silently() {
    let v =
        providers_cmd(&["--names".into(), "openai,does-not-exist".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    // "does-not-exist" is not in REGISTERED, so it gets dropped.
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("name").and_then(|n| n.as_str()), Some("openai"));
}

#[test]
fn providers_cmd_local_providers_have_no_canonical_env_or_credential() {
    let v =
        providers_cmd(&["--names".into(), "ollama,mock,llama_local".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 3);
    for entry in arr {
        assert_eq!(
            entry.get("env"),
            Some(&serde_json::Value::Null),
            "{:?} should have no canonical env",
            entry.get("name")
        );
        assert_eq!(
            entry.get("credential"),
            Some(&serde_json::Value::Null),
            "{:?} should have no canonical credential",
            entry.get("name")
        );
        assert_eq!(
            entry.get("key_required"),
            Some(&serde_json::Value::Bool(false)),
            "{:?} should not require a key",
            entry.get("name")
        );
    }
}

#[test]
fn providers_cmd_cloud_providers_advertise_canonical_env_and_credential() {
    let v = providers_cmd(&[
        "--names".into(),
        "openai,anthropic,gemini,xai,deepseek,openrouter".into(),
    ])
    .expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 6);
    let by_name: std::collections::HashMap<_, _> = arr
        .iter()
        .map(|e| {
            (
                e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                e.clone(),
            )
        })
        .collect();
    assert_eq!(
        by_name["openai"].get("env").and_then(|e| e.as_str()),
        Some("OPENAI_API_KEY")
    );
    assert_eq!(
        by_name["anthropic"].get("env").and_then(|e| e.as_str()),
        Some("ANTHROPIC_API_KEY")
    );
    assert_eq!(
        by_name["gemini"].get("env").and_then(|e| e.as_str()),
        Some("GEMINI_API_KEY")
    );
    assert_eq!(
        by_name["openrouter"]
            .get("credential")
            .and_then(|e| e.as_str()),
        Some("openrouter")
    );
    for n in [
        "openai",
        "anthropic",
        "gemini",
        "xai",
        "deepseek",
        "openrouter",
    ] {
        assert_eq!(
            by_name[n].get("key_required"),
            Some(&serde_json::Value::Bool(true)),
            "{n} should require a key"
        );
    }
}

#[test]
fn providers_cmd_default_base_url_per_alias() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    let by_name: std::collections::HashMap<_, _> = arr
        .iter()
        .map(|e| {
            (
                e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                e.clone(),
            )
        })
        .collect();
    assert_eq!(
        by_name["openai"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(
        by_name["xai"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.x.ai/v1")
    );
    assert_eq!(
        by_name["ollama"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("http://localhost:11434/v1")
    );
    assert_eq!(
        by_name["anthropic"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.anthropic.com/v1")
    );
    assert_eq!(
        by_name["gemini"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://generativelanguage.googleapis.com/v1beta")
    );
}

#[test]
fn providers_cmd_env_present_reflects_environment() {
    // Pick an env name extremely unlikely to be set in CI to assert
    // the negative path. We can't safely set/unset OPENAI_API_KEY in
    // a process-shared test, so we just check the contract.
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    for entry in arr {
        let env = entry.get("env").and_then(|e| e.as_str());
        let env_present = entry
            .get("env_present")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if env.is_none() {
            assert!(
                !env_present,
                "providers without canonical env must report env_present=false"
            );
        }
    }
}

#[test]
fn providers_cmd_probe_credentials_default_off() {
    let v = providers_cmd(&[]).expect("providers ok");
    assert_eq!(
        v.get("probe_credentials"),
        Some(&serde_json::Value::Bool(false))
    );
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    for entry in arr {
        assert_eq!(
            entry.get("credential_present"),
            Some(&serde_json::Value::Bool(false)),
            "credential_present must be false when --probe-credentials is off (no false positives)"
        );
    }
}

#[test]
fn providers_cmd_probe_credentials_flag_flips_marker() {
    let v = providers_cmd(&["--probe-credentials".into()]).expect("providers ok");
    assert_eq!(
        v.get("probe_credentials"),
        Some(&serde_json::Value::Bool(true))
    );
    // We don't assert credential_present truthiness because the
    // test environment is unpredictable; just that the probe ran.
}

#[test]
fn providers_cmd_count_matches_providers_array_len() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr_len = v
        .get("providers")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    assert_eq!(count as usize, arr_len);
}

#[test]
fn providers_cmd_filter_count_matches_filtered_array() {
    let v = providers_cmd(&["--names".into(), "openai".into()]).expect("providers ok");
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    assert_eq!(count, 1);
}

#[test]
fn providers_cmd_surfaces_bedrock_with_aws_access_key_env() {
    // Bedrock uses three env vars (access key, secret, optional
    // session token); we surface AWS_ACCESS_KEY_ID as the canonical
    // env, matching AWS SDK convention. credential is None
    // because Bedrock's three-name credential model doesn't fit
    // the single-name `*_credential` field.
    let v = providers_cmd(&["--names".into(), "bedrock".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert_eq!(entry.get("name").and_then(|n| n.as_str()), Some("bedrock"));
    assert_eq!(
        entry.get("env").and_then(|e| e.as_str()),
        Some("AWS_ACCESS_KEY_ID")
    );
    assert_eq!(entry.get("credential"), Some(&serde_json::Value::Null));
    assert_eq!(
        entry.get("key_required"),
        Some(&serde_json::Value::Bool(true))
    );
    let url = entry
        .get("default_base_url")
        .and_then(|u| u.as_str())
        .unwrap_or("");
    assert!(
        url.contains("bedrock-runtime") && url.contains("{region}"),
        "expected region-templated default_base_url, got {url}"
    );
}

#[test]
fn provider_build_status_surfaces_unresolved_pool_without_secret_values() {
    const LEGACY_ENV: &str = "__COS_TEST_PROVIDER_STATUS_LEGACY__";
    const MISSING_POOL_ENV: &str = "__COS_TEST_PROVIDER_STATUS_POOL_MISSING__";
    const LEGACY_VALUE: &str = "legacy-status-secret-must-not-leak";
    std::env::set_var(LEGACY_ENV, LEGACY_VALUE);
    std::env::remove_var(MISSING_POOL_ENV);
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "openai".into();
    cfg.model = "gpt-test".into();
    cfg.api_key_env = Some(LEGACY_ENV.into());
    cfg.api_key_envs = vec![MISSING_POOL_ENV.into()];

    let (configured, error) = provider_build_status("openai", "gpt-test", &cfg);
    std::env::remove_var(LEGACY_ENV);
    assert!(!configured);
    assert_eq!(error["kind"], "credential_pool");
    assert_eq!(error["provider"], "openai");
    assert_eq!(error["environment_variables"], json!([MISSING_POOL_ENV]));
    assert!(error["details"]
        .as_str()
        .is_some_and(|details| details.contains(MISSING_POOL_ENV)));
    assert!(!error.to_string().contains(LEGACY_VALUE));
}

// ---- provider-doctor ----

#[test]
fn provider_doctor_static_only_includes_doctor_section() {
    // Default invocation: no --probe-network.
    let v = provider_doctor_cmd(&[]).expect("doctor ok");
    // Inherits the providers_cmd shape.
    assert!(v.get("providers").and_then(|p| p.as_array()).is_some());
    // Doctor section present.
    let doctor = v.get("doctor").expect("doctor section");
    assert_eq!(
        doctor.get("probe_network"),
        Some(&serde_json::Value::Bool(false))
    );
    let probe = doctor.get("active_probe").expect("active_probe");
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(probe.get("reason").and_then(|r| r.as_str()).is_some());
}

#[test]
fn provider_doctor_default_timeout_is_30s() {
    let v = provider_doctor_cmd(&[]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
        Some(30)
    );
}

#[test]
fn provider_doctor_custom_timeout_parses() {
    let v = provider_doctor_cmd(&["--timeout".into(), "5".into()]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
        Some(5)
    );
}

#[test]
fn provider_doctor_zero_timeout_rejected() {
    let err = provider_doctor_cmd(&["--timeout".into(), "0".into()]).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn provider_doctor_non_numeric_timeout_rejected() {
    let err = provider_doctor_cmd(&["--timeout".into(), "soon".into()]).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn provider_doctor_skips_probe_for_unconfigured_provider() {
    // The default test config now has provider="" (not configured).
    // Verify --probe-network is gracefully skipped without spinning a
    // tokio runtime or hitting the network.
    let v = provider_doctor_cmd(&["--probe-network".into()]).expect("doctor ok");
    let probe = v
        .get("doctor")
        .and_then(|d| d.get("active_probe"))
        .expect("probe");
    assert_eq!(
        v.get("doctor")
            .and_then(|d| d.get("active"))
            .and_then(|a| a.as_str()),
        Some("")
    );
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    assert!(
        reason.contains("no text-model provider configured"),
        "expected unconfigured-skip reason, got {reason:?}"
    );
}

#[test]
fn provider_doctor_filter_excluding_active_marks_out_of_scope() {
    // Active provider is "" in test config (fresh-install default).
    // Filtering to "openai" leaves the active filter out-of-scope.
    // The probe-skip reason can be either "filter excluded the active
    // provider" or "no LLM configured" depending on which check fires
    // first; both are honest UX.
    let v = provider_doctor_cmd(&["--probe-network".into(), "--names".into(), "openai".into()])
        .expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("active_in_scope"),
        Some(&serde_json::Value::Bool(false))
    );
    let probe = doctor.get("active_probe").unwrap();
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    assert!(
        reason.contains("filtered out")
            || reason.contains("--names")
            || reason.contains("no text-model provider configured"),
        "expected filter or unconfigured reason, got {reason:?}"
    );
}

#[test]
fn provider_doctor_surfaces_effective_timeout_min_of_two() {
    // probe_timeout 9999 + provider request_timeout (default = some
    // smaller value from CosConfig) → effective is the smaller one.
    let v = provider_doctor_cmd(&["--timeout".into(), "9999".into()]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    let probe_t = doctor
        .get("probe_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    let provider_t = doctor
        .get("provider_request_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    let effective = doctor
        .get("effective_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    assert_eq!(probe_t, 9999);
    assert_eq!(effective, std::cmp::min(probe_t, provider_t));
}

#[test]
fn llm_error_kind_classification_is_complete() {
    // Pin the tag for every LlmError variant — adding a new variant
    // without updating the doctor classifier should fail this test.
    assert_eq!(
        llm_error_kind(&llm::LlmError::NotConfigured("x".into())),
        "not_configured"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::InvalidRequest("x".into())),
        "invalid_request"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::Provider {
            status: 500,
            message: "x".into(),
        }),
        "provider"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::RateLimited { retry_after_ms: 0 }),
        "rate_limited"
    );
    assert_eq!(llm_error_kind(&llm::LlmError::Auth), "auth");
    assert_eq!(llm_error_kind(&llm::LlmError::Parse("x".into())), "parse");
    assert_eq!(llm_error_kind(&llm::LlmError::Stream("x".into())), "stream");
    assert_eq!(
        llm_error_kind(&llm::LlmError::Internal("x".into())),
        "internal"
    );
}
