use super::*;

fn report(result: Result<Option<String>, String>) -> ReceiptReport {
    capture(
        uuid::Uuid::new_v4().to_string(),
        "demo".into(),
        "get".into(),
        format!("sha256:{}", "a".repeat(64)),
        result,
    )
}

#[test]
fn receipt_captures_returned_bytes_without_claiming_effects() {
    let output = r#"{"value":"ready"}"#;
    let captured = report(Ok(Some(output.into())));
    captured.validate().unwrap();
    assert_eq!(captured.outcome, ReceiptOutcome::Returned);
    let result = captured.result.unwrap();
    assert_eq!(result.kind, ResultKind::Json);
    assert_eq!(result.bytes, output.len() as u64);
    assert_eq!(
        result.sha256,
        format!("sha256:{}", crate::crypto::sha256_hex(output.as_bytes()))
    );
    assert!(!result.preview_truncated);
}

#[test]
fn receipt_distinguishes_app_error_empty_output_and_unavailable_result() {
    let error = report(Ok(Some(r#"{"error":"not found"}"#.into())));
    error.validate().unwrap();
    assert_eq!(error.outcome, ReceiptOutcome::ReportedError);
    assert_eq!(error.error.as_deref(), Some("not found"));
    let empty = report(Ok(None));
    empty.validate().unwrap();
    assert_eq!(empty.result.unwrap().kind, ResultKind::Empty);
    let uncertain = report(Err("connection lost after dispatch".into()));
    uncertain.validate().unwrap();
    assert_eq!(uncertain.outcome, ReceiptOutcome::Indeterminate);
    assert!(uncertain.result.is_none());
    let text = report(Ok(Some("plain output".into())));
    text.validate().unwrap();
    assert_eq!(text.result.unwrap().kind, ResultKind::Text);
}

#[test]
fn receipt_previews_are_bounded_redacted_and_control_safe() {
    let token = format!("ghp_{}", "a".repeat(36));
    let raw = format!("token {token}\u{1b}[31m {}", "界".repeat(1000));
    let captured = report(Ok(Some(raw)));
    captured.validate().unwrap();
    let summary = captured.result.unwrap();
    assert!(summary.preview.len() <= 2048);
    assert!(summary.preview_truncated);
    assert!(!summary.preview.contains(&token));
    assert!(!summary.preview.contains('\u{1b}'));
    let oversized = report(Ok(Some("x".repeat(MAX_RESULT_BYTES + 1))));
    oversized.validate().unwrap();
    assert_eq!(oversized.outcome, ReceiptOutcome::Indeterminate);
    assert!(oversized.result.is_none());
}
