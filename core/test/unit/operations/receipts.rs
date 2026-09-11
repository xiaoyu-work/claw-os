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
fn session_capture_preserves_mcp_error_flags_and_transport_uncertainty() {
    for (result, outcome, error) in [
        (
            Ok(("exact\noutput".to_string(), false)),
            ReceiptOutcome::Returned,
            None,
        ),
        (
            Ok(("handler refused".into(), true)),
            ReceiptOutcome::ReportedError,
            Some("handler refused"),
        ),
        (
            Err("connection lost after dispatch".to_string()),
            ReceiptOutcome::Indeterminate,
            Some("connection lost after dispatch"),
        ),
    ] {
        let returned = result.as_ref().ok().map(|(text, _)| text.clone());
        let captured = capture_session(
            "demo",
            "demo.read-file",
            &format!("sha256:{}", "a".repeat(64)),
            result,
        );
        captured.validate().unwrap();
        assert_eq!(captured.operation, "session:demo.read-file");
        assert_eq!(captured.outcome, outcome);
        assert_eq!(captured.error.as_deref(), error);
        if let Some(text) = returned {
            let summary = captured.result.unwrap();
            assert_eq!(summary.bytes, text.len() as u64);
            assert_eq!(
                summary.sha256,
                format!("sha256:{}", crate::crypto::sha256_hex(text.as_bytes())),
            );
        } else {
            assert!(captured.result.is_none());
        }
    }
    let oversized = capture_session(
        "demo",
        "demo.read",
        &format!("sha256:{}", "a".repeat(64)),
        Ok(("x".repeat(MAX_RESULT_BYTES + 1), true)),
    );
    oversized.validate().unwrap();
    assert_eq!(oversized.outcome, ReceiptOutcome::Indeterminate);
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

fn file_plan() -> serde_json::Value {
    serde_json::json!({
        "schema":1,"kind":"file_change_plan",
        "plan_id":"00000000-0000-4000-8000-000000000001",
        "path":"/home/user/config.txt","state":"draft",
        "before_exists":true,"before_sha256":format!("sha256:{}", "a".repeat(64)),
        "after_sha256":format!("sha256:{}", "b".repeat(64)),
        "before_bytes":4,"after_bytes":4,"would_change":true,
        "review":format!("sha256:{}", "c".repeat(64)),
        "reference":"app://fs/change-plan?id=%2Fhome%2Fuser%2Fconfig.txt&revision=00000000-0000-4000-8000-000000000001",
        "diff":"--- before\n+++ proposed\n@@ -1 +1 @@\n-old\n+new\n",
        "diff_truncated":false,"created_at":"2026-09-10T00:00:00Z",
        "expires_at":"2026-09-10T01:00:00Z","warnings":["App-reported, not authority"]
    })
}

#[test]
fn typed_file_plan_receipts_render_real_diff_as_untrusted_text() {
    let raw = file_plan().to_string();
    let capture = |raw| {
        capture(
            uuid::Uuid::new_v4().to_string(),
            "fs".into(),
            "plan_show".into(),
            format!("sha256:{}", "a".repeat(64)),
            Ok(Some(raw)),
        )
    };
    let captured = capture(raw.clone());
    captured.validate().unwrap();
    assert_eq!(captured.outcome, ReceiptOutcome::Returned);
    let summary = captured.result.unwrap();
    assert_eq!(summary.kind, ResultKind::Json);
    assert_eq!(
        summary.sha256,
        format!("sha256:{}", crate::crypto::sha256_hex(raw.as_bytes()))
    );
    assert!(summary.preview.contains("-old\n+new\n"));
    assert!(summary
        .preview
        .contains("not authorization or OS-confirmed effects"));
    let mut truncated = file_plan();
    truncated["diff_truncated"] = serde_json::json!(true);
    assert!(
        capture(truncated.to_string())
            .result
            .unwrap()
            .preview_truncated
    );
}

#[test]
fn malformed_typed_file_plan_is_not_interpreted_as_a_valid_proposal() {
    let mut invalid = file_plan();
    invalid["authority"] = serde_json::json!("os_verified");
    let captured = report(Ok(Some(invalid.to_string())));
    captured.validate().unwrap();
    assert_eq!(captured.outcome, ReceiptOutcome::Indeterminate);
    assert!(captured.result.is_none());
    assert!(captured.error.unwrap().contains("no plan was trusted"));
    let legacy = report(Ok(Some(
        r#"{"kind":"file_change_plan","legacy":"ordinary App data"}"#.into(),
    )));
    legacy.validate().unwrap();
    assert_eq!(legacy.outcome, ReceiptOutcome::Returned);
    assert!(legacy.result.unwrap().preview.contains("ordinary App data"));
}
