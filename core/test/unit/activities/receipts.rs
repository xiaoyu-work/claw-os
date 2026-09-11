use super::*;
use serde_json::json;

fn result() -> ResultSummary {
    ResultSummary {
        kind: ResultKind::Text,
        sha256: format!("sha256:{}", "a".repeat(64)),
        bytes: 1,
        preview: "[redacted]\n\tpreview".to_string(),
        preview_truncated: true,
    }
}

fn report() -> ReceiptReport {
    ReceiptReport {
        id: uuid::Uuid::new_v4().to_string(),
        app_id: "sample_app-1".to_string(),
        operation: "notes.read_text".to_string(),
        package_digest: format!("sha256:{}", "b".repeat(64)),
        outcome: ReceiptOutcome::Returned,
        result: Some(result()),
        error: None,
    }
}

fn declaration() -> ReceiptDeclaration {
    ReceiptDeclaration {
        app_version: "1.0.0".to_string(),
        operation_label: "Read".to_string(),
        effects: vec![ReceiptEffect {
            kind: EffectKind::Read,
            label: "App-declared read".to_string(),
            recovery: EffectRecovery::Unknown,
            target_arg: Some("path".to_string()),
        }],
    }
}

#[test]
fn receipt_enums_have_only_the_frozen_wire_spellings() {
    for (outcome, wire) in [
        (ReceiptOutcome::Returned, "returned"),
        (ReceiptOutcome::ReportedError, "reported_error"),
        (ReceiptOutcome::Indeterminate, "indeterminate"),
    ] {
        assert_eq!(serde_json::to_value(outcome).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ReceiptOutcome>(json!(wire)).unwrap(),
            outcome
        );
    }
    for (kind, wire) in [
        (ResultKind::Json, "json"),
        (ResultKind::Text, "text"),
        (ResultKind::Empty, "empty"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<ResultKind>(json!(wire)).unwrap(),
            kind
        );
    }
    assert_eq!(
        serde_json::to_value(ReceiptSource::CallerReported).unwrap(),
        "caller_reported"
    );
    for source in ["os_confirmed", "verified", "user", "model"] {
        assert!(serde_json::from_value::<ReceiptSource>(json!(source)).is_err());
    }
}

#[test]
fn caller_cannot_supply_source_authority_or_manifest_declarations_in_a_report() {
    for field in [
        "source",
        "owner_uid",
        "received_at",
        "grant",
        "confirmed_effects",
        "declaration",
    ] {
        let mut value = serde_json::to_value(report()).unwrap();
        value[field] = json!("forged");
        assert!(
            serde_json::from_value::<ReceiptReport>(value).is_err(),
            "{field}"
        );
    }
    let mut value = serde_json::to_value(result()).unwrap();
    value["original_output"] = json!("secret");
    assert!(serde_json::from_value::<ResultSummary>(value).is_err());
    for field in ["requested_target", "final_target", "confirmed", "grant"] {
        let mut value = serde_json::to_value(&declaration().effects[0]).unwrap();
        value[field] = json!("/not-a-receipt-field");
        assert!(
            serde_json::from_value::<ReceiptEffect>(value).is_err(),
            "{field}"
        );
    }
}

#[test]
fn report_id_is_canonicalized_without_changing_report_text() {
    let original = report();
    for id in [
        original.id.to_uppercase(),
        uuid::Uuid::parse_str(&original.id)
            .unwrap()
            .simple()
            .to_string(),
        uuid::Uuid::parse_str(&original.id)
            .unwrap()
            .urn()
            .to_string(),
    ] {
        let mut value = original.clone();
        value.id = id;
        value.validate().unwrap();
        assert_eq!(value.canonicalized().unwrap(), original);
    }
    for id in [
        "",
        "not-a-uuid",
        " 00000000-0000-4000-8000-000000000001",
        "uuid\0",
    ] {
        let mut value = original.clone();
        value.id = id.to_string();
        assert!(value.validate().is_err());
    }
}

#[test]
fn receipt_identifiers_and_digests_are_strict_and_bounded() {
    let mut value = report();
    value.app_id = "a".repeat(128);
    value.operation = "a".repeat(128);
    value.validate().unwrap();
    for app in [
        "a".repeat(129),
        "App".into(),
        "app.name".into(),
        "_app".into(),
        "a\n".into(),
    ] {
        let mut value = report();
        value.app_id = app;
        assert!(value.validate().is_err());
    }
    for operation in [
        "a".repeat(129),
        "Read".into(),
        "a..b".into(),
        "read-text".into(),
        "../read".into(),
        "read\t".into(),
    ] {
        let mut value = report();
        value.operation = operation;
        assert!(value.validate().is_err());
    }
    for digest in [
        "a".repeat(64),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{} ", "a".repeat(64)),
    ] {
        let mut value = report();
        value.package_digest = digest.clone();
        assert!(value.validate().is_err());
        let mut summary = result();
        summary.sha256 = digest;
        assert!(summary.validate().is_err());
    }
}

#[test]
fn result_summaries_bound_original_size_and_redacted_preview_independently() {
    let mut summary = result();
    summary.bytes = MAX_RESULT_BYTES;
    summary.preview = "\u{e9}".repeat(MAX_REPORT_TEXT_BYTES / 2);
    summary.validate().unwrap();
    summary.preview.push('x');
    assert!(summary.validate().is_err());
    summary = result();
    summary.bytes = MAX_RESULT_BYTES + 1;
    assert!(summary.validate().is_err());
    for control in ['\0', '\u{1b}', '\u{7f}', '\u{85}'] {
        let mut summary = result();
        summary.preview.push(control);
        assert!(summary.validate().is_err());
    }
    for kind in [ResultKind::Json, ResultKind::Text] {
        let mut summary = result();
        summary.kind = kind;
        summary.preview = "  [redacted, not necessarily JSON]\r\n\t  ".into();
        summary.validate().unwrap();
        summary.bytes = 0;
        assert!(summary.validate().is_err());
    }
}

#[test]
fn empty_summary_has_exact_empty_output_invariants() {
    let empty = ResultSummary {
        kind: ResultKind::Empty,
        sha256: EMPTY_SHA256.to_string(),
        bytes: 0,
        preview: String::new(),
        preview_truncated: false,
    };
    empty.validate().unwrap();
    let mut nonzero = empty.clone();
    nonzero.bytes = 1;
    let mut preview = empty.clone();
    preview.preview = "redacted".into();
    let mut truncated = empty.clone();
    truncated.preview_truncated = true;
    let mut digest = empty;
    digest.sha256 = result().sha256;
    for value in [nonzero, preview, truncated, digest] {
        assert!(value.validate().is_err());
    }
}

#[test]
fn outcomes_require_the_exact_result_error_combinations() {
    for outcome in [
        ReceiptOutcome::Returned,
        ReceiptOutcome::ReportedError,
        ReceiptOutcome::Indeterminate,
    ] {
        for has_result in [false, true] {
            for has_error in [false, true] {
                let mut value = report();
                value.outcome = outcome;
                value.result = has_result.then(result);
                value.error = has_error.then(|| "Caller reports an error".to_string());
                let expected = match outcome {
                    ReceiptOutcome::Returned => has_result && !has_error,
                    ReceiptOutcome::ReportedError => has_result && has_error,
                    ReceiptOutcome::Indeterminate => !has_result && has_error,
                };
                assert_eq!(value.validate().is_ok(), expected);
            }
        }
    }
    let mut value = report();
    value.outcome = ReceiptOutcome::ReportedError;
    value.error = Some("\u{e9}".repeat(MAX_REPORT_TEXT_BYTES / 2));
    value.validate().unwrap();
    value.error.as_mut().unwrap().push('x');
    assert!(value.validate().is_err());
    value.error = Some(String::new());
    assert!(value.validate().is_err());
}

#[test]
fn declaration_target_arguments_are_bounded_and_control_free() {
    let mut value = declaration();
    value.effects[0].target_arg = None;
    value.validate().unwrap();
    for target in [
        "a".repeat(MAX_NAME_BYTES),
        "\u{e9}".repeat(MAX_NAME_BYTES / 2),
    ] {
        assert_eq!(target.len(), MAX_NAME_BYTES);
        value.effects[0].target_arg = Some(target);
        value.validate().unwrap();
        value.effects[0].target_arg.as_mut().unwrap().push('x');
        assert!(matches!(value.validate(), Err(ActivityError::Invalid(_))));
    }
    value.effects[0].target_arg = Some(String::new());
    assert!(matches!(value.validate(), Err(ActivityError::Invalid(_))));
    for control in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}'] {
        value.effects[0].target_arg = Some(format!("path{control}"));
        assert!(matches!(value.validate(), Err(ActivityError::Invalid(_))));
    }
}

#[test]
fn manifest_declaration_or_error_is_required_and_bounded_without_target_values() {
    let mut value = declaration();
    value.app_version = "v".repeat(128);
    value.operation_label = "\u{e9}".repeat(256);
    value.effects[0].label = "\u{e9}".repeat(256);
    value.effects = vec![value.effects[0].clone(); 16];
    validate_declaration(Some(&value), None).unwrap();
    value.effects.push(value.effects[0].clone());
    assert!(validate_declaration(Some(&value), None).is_err());
    value.effects.pop();
    value.effects[0].label.push('x');
    assert!(validate_declaration(Some(&value), None).is_err());
    value = declaration();
    value.app_version = "v".repeat(129);
    assert!(validate_declaration(Some(&value), None).is_err());
    value = declaration();
    value.operation_label = "x".repeat(513);
    assert!(validate_declaration(Some(&value), None).is_err());
    assert!(validate_declaration(None, None).is_err());
    assert!(validate_declaration(Some(&declaration()), Some("mismatch")).is_err());
    assert!(validate_declaration(None, Some("")).is_err());
    validate_declaration(None, Some(&"x".repeat(2048))).unwrap();
    assert!(validate_declaration(None, Some(&"x".repeat(2049))).is_err());
}
