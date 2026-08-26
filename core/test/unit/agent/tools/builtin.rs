use super::*;

#[tokio::test]
async fn echo_returns_text() {
    let r = Echo.exec(json!({"text": "hello"})).await;
    assert!(!r.is_error);
    assert_eq!(r.content, "hello");
}

#[tokio::test]
async fn echo_missing_field() {
    let r = Echo.exec(json!({})).await;
    assert!(r.is_error);
    assert!(r.content.contains("text"));
}

#[tokio::test]
async fn now_returns_rfc3339() {
    let r = Now.exec(json!({})).await;
    assert!(!r.is_error);
    // Year 20XX or 21XX, RFC 3339-ish: we just assert it parses.
    let parsed = chrono::DateTime::parse_from_rfc3339(&r.content);
    assert!(parsed.is_ok(), "expected RFC3339, got: {}", r.content);
}
