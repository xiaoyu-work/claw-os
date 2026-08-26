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
