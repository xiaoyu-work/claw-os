use super::*;
use crate::config::AgentConfig;
use std::collections::HashMap;

#[derive(Default)]
struct FakeCredentialSource {
    stored: HashMap<String, crate::credential::CredentialResult<Option<String>>>,
    environment: HashMap<String, String>,
}

impl CredentialSource for FakeCredentialSource {
    fn load_stored(&self, name: &str) -> crate::credential::CredentialResult<Option<String>> {
        match self.stored.get(name) {
            Some(Ok(value)) => Ok(value.clone()),
            Some(Err(error)) => Err(crate::credential::CredentialError::external(
                error.operation(),
                error.to_string(),
            )),
            None => Ok(None),
        }
    }

    fn load_environment(&self, name: &str) -> Option<String> {
        self.environment.get(name).cloned()
    }
}

#[test]
fn single_key_prefers_stored_credential_over_environment() {
    let source = FakeCredentialSource {
        stored: HashMap::from([("stored".into(), Ok(Some(" stored-key ".into())))]),
        environment: HashMap::from([("API_KEY".into(), "env-key".into())]),
    };

    let resolved = resolve_single_api_key(Some("stored"), Some("API_KEY"), &source).unwrap();

    assert_eq!(resolved.as_deref(), Some("stored-key"));
}

#[test]
fn blank_stored_credential_falls_through_to_environment() {
    let source = FakeCredentialSource {
        stored: HashMap::from([("stored".into(), Ok(Some("  ".into())))]),
        environment: HashMap::from([("API_KEY".into(), " env-key ".into())]),
    };

    let resolved = resolve_single_api_key(Some("stored"), Some("API_KEY"), &source).unwrap();

    assert_eq!(resolved.as_deref(), Some("env-key"));
}

#[test]
fn stored_credential_failure_remains_typed() {
    let source = FakeCredentialSource {
        stored: HashMap::from([(
            "broken".into(),
            Err(crate::credential::CredentialError::external(
                "test.credential",
                "corrupt record",
            )),
        )]),
        environment: HashMap::from([("API_KEY".into(), "must-not-rescue".into())]),
    };

    let error = resolve_single_api_key(Some("broken"), Some("API_KEY"), &source).unwrap_err();

    assert!(matches!(
        error,
        LlmError::CredentialStore { credential, .. } if credential == "broken"
    ));
}

#[test]
fn declared_pool_is_authoritative_and_preserves_source_order() {
    let source = FakeCredentialSource {
        stored: HashMap::from([
            ("first".into(), Ok(Some("one".into()))),
            ("legacy".into(), Ok(Some("must-not-rescue".into()))),
        ]),
        environment: HashMap::from([("SECOND".into(), "two".into())]),
    };
    let mut cfg = AgentConfig::default();
    cfg.api_key_credential = Some("legacy".into());
    cfg.api_key_credentials = vec!["missing".into(), "first".into()];
    cfg.api_key_envs = vec!["SECOND".into()];
    cfg.pool_strategy = "round-robin".into();

    let resolved = resolve_api_credentials(
        "provider:test",
        ApiCredentialConfig::from_agent_config(&cfg),
        &source,
    )
    .unwrap();
    assert!(resolved.api_key.is_none());
    let pool = resolved.pool.expect("declared pool");
    let first = pool.acquire().unwrap();
    assert_eq!(first.value(), "one");
    pool.report_success(&first);
    let second = pool.acquire().unwrap();
    assert_eq!(second.value(), "two");
}

#[test]
fn unresolved_declared_pool_fails_without_legacy_fallback() {
    let source = FakeCredentialSource {
        stored: HashMap::from([("legacy".into(), Ok(Some("must-not-rescue".into())))]),
        environment: HashMap::new(),
    };
    let mut cfg = AgentConfig::default();
    cfg.api_key_credential = Some("legacy".into());
    cfg.api_key_credentials = vec!["missing".into()];

    let error = resolve_api_credentials(
        "provider:test",
        ApiCredentialConfig::from_agent_config(&cfg),
        &source,
    )
    .unwrap_err();

    assert!(matches!(error, LlmError::NotConfigured(message) if message.contains("missing")));
}

#[test]
fn aws_resolution_uses_stored_then_configured_then_default_environment() {
    let source = FakeCredentialSource {
        stored: HashMap::from([("stored".into(), Ok(Some("stored-value".into())))]),
        environment: HashMap::from([
            ("CONFIGURED".into(), "configured-value".into()),
            ("DEFAULT".into(), "default-value".into()),
        ]),
    };

    assert_eq!(
        resolve_aws_value(Some("stored"), Some("CONFIGURED"), "DEFAULT", &source)
            .unwrap()
            .as_deref(),
        Some("stored-value")
    );
    assert_eq!(
        resolve_aws_value(Some("missing"), Some("CONFIGURED"), "DEFAULT", &source)
            .unwrap()
            .as_deref(),
        Some("configured-value")
    );
    assert_eq!(
        resolve_aws_value(Some("missing"), None, "DEFAULT", &source)
            .unwrap()
            .as_deref(),
        Some("default-value")
    );
}

#[test]
fn aws_stored_credential_failure_is_not_rescued_by_environment() {
    let source = FakeCredentialSource {
        stored: HashMap::from([(
            "broken".into(),
            Err(crate::credential::CredentialError::external(
                "test.credential",
                "corrupt record",
            )),
        )]),
        environment: HashMap::from([("DEFAULT".into(), "must-not-rescue".into())]),
    };

    let error = resolve_aws_value(Some("broken"), None, "DEFAULT", &source).unwrap_err();
    assert!(matches!(
        &error,
        LlmError::CredentialStore { credential, .. } if credential == "broken"
    ));
    assert!(!error.to_string().contains("must-not-rescue"));
}

#[test]
fn provider_constructors_and_chain_have_no_ambient_infrastructure_reads() {
    let provider_sources = [
        (
            "openai_compat",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/agent/llm/providers/openai_compat.rs"
            )),
        ),
        (
            "anthropic",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/agent/llm/providers/anthropic.rs"
            )),
        ),
        (
            "gemini",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/agent/llm/providers/gemini.rs"
            )),
        ),
        (
            "bedrock",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/agent/llm/providers/bedrock.rs"
            )),
        ),
        (
            "copilot_auth",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/agent/llm/providers/copilot_auth.rs"
            )),
        ),
    ];
    for (name, source) in provider_sources {
        for forbidden in [
            "crate::credential::",
            "std::env::var",
            "reqwest::Client::builder",
            "reqwest::Client::new",
            "agent_audit_log_path",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} contains ambient infrastructure read: {forbidden}"
            );
        }
    }

    let openai = provider_sources[0].1;
    for injected_auth_call in [
        "ensure_copilot_token_with_transport",
        "refresh_rejected_copilot_token_with_transport",
        "wire_api_for_model_with_transport",
    ] {
        assert!(
            openai.contains(injected_auth_call),
            "Copilot live auth bypasses injected transport: {injected_auth_call}"
        );
    }

    let chain = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/agent/llm/provider_chain.rs"
    ));
    for forbidden in [
        "std::env",
        "agent_audit_log_path",
        "log_chained_event",
        "crate::config",
    ] {
        assert!(
            !chain.contains(forbidden),
            "provider chain contains ambient dependency: {forbidden}"
        );
    }
}
