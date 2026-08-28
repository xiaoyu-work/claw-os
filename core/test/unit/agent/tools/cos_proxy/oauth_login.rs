use super::*;

#[test]
fn schema_constrains_provider_and_timeout() {
    let schema = CosOauthLoginTool::new().input_schema();
    assert_eq!(
        schema.pointer("/required/0").and_then(Value::as_str),
        Some("provider")
    );
    assert_eq!(
        schema
            .pointer("/properties/provider/enum")
            .and_then(Value::as_array)
            .unwrap(),
        &vec![json!("google"), json!("microsoft")]
    );
    assert_eq!(
        schema
            .pointer("/properties/timeout_seconds/minimum")
            .and_then(Value::as_u64),
        Some(30)
    );
    assert_eq!(
        schema
            .pointer("/properties/timeout_seconds/maximum")
            .and_then(Value::as_u64),
        Some(900)
    );
    assert!(schema.pointer("/properties/namespace").is_none());
}

#[tokio::test]
async fn rejects_unsupported_provider_before_authorization() {
    let result = CosOauthLoginTool::new()
        .exec(json!({"provider": "gmail"}))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("supported: google, microsoft"));
}

#[tokio::test]
async fn rejects_invalid_timeout_before_authorization() {
    let result = CosOauthLoginTool::new()
        .exec(json!({"provider": "google", "timeout_seconds": 10}))
        .await;
    assert!(result.is_error);
    assert!(result.content.contains("between 30 and 900"));
}

fn fake_authorized_runner(
    args: Vec<String>,
    _authorization: crate::credential::AgentOauthAuthorization,
) -> Result<Value, String> {
    assert!(
        crate::agent::tools::exposure::current().is_none(),
        "spawn_blocking must not be expected to inherit Tokio task locals"
    );
    Ok(json!({"authorized": true, "provider": args[0]}))
}

#[tokio::test]
async fn direct_cli_and_web_capture_authorization_before_spawn_blocking() {
    for source in [
        crate::session::SessionSource::LocalCli,
        crate::session::SessionSource::LocalWeb,
    ] {
        let context = crate::agent::tools::exposure::ToolExposureContext::isolated(
            crate::agent::tools::guardrails::Guardrails::permissive(),
        )
        .with_identity("oauth-session", 1000, source)
        .with_presence(true, true);
        let result = crate::agent::tools::exposure::scope(
            context,
            execute_with(json!({"provider": "google"}), fake_authorized_runner),
        )
        .await;
        assert!(!result.is_error, "{source:?}: {}", result.content);
        assert!(result.content.contains("\"authorized\":true"));
    }
}
