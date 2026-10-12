use super::*;
use serde_json::json;

#[test]
fn retained_presentation_recovers_totals_and_metrics_without_private_payloads() {
    let mut presentation = TaskPresentation::default();
    let redactor = Redactor::default_set();
    for record in [
        json!({"event":{"kind":"reasoning","id":"rs1","summary":["Checking state"],"encrypted_content":"OPAQUE_PROVIDER_STATE"}}),
        json!({"event":{"kind":"tool_use","id":"tool1","name":"observe","input":{"secret":"PRIVATE_INPUT"}}}),
        json!({"progress":{"kind":"tool_start","id":"tool1","name":"observe"}}),
        json!({"progress":{"kind":"tool_result","id":"tool1","name":"observe","ok":true,"latency_ms":42,"bytes_returned":512,"error_preview":"SUCCESS_BODY"}}),
        json!({"event":{"kind":"done","finish":"tool_use","usage":{"input_tokens":100,"output_tokens":20,"cache_read_tokens":40,"cache_write_tokens":0}}}),
        json!({"event":{"kind":"done","finish":"stop","usage":{"input_tokens":30,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":5}}}),
    ] {
        project_record(&mut presentation, record, &redactor).unwrap();
    }
    assert_eq!(presentation.reasoning, ["Checking state"]);
    let usage = presentation.usage.as_ref().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens
        ),
        (130, 30, 40, 5)
    );
    assert_eq!(presentation.tools[0].latency_ms, Some(42));
    assert_eq!(presentation.tools[0].bytes_returned, Some(512));
    let encoded = serde_json::to_string(&presentation).unwrap();
    for private in ["OPAQUE_PROVIDER_STATE", "PRIVATE_INPUT", "SUCCESS_BODY"] {
        assert!(!encoded.contains(private));
    }
}

#[test]
fn retained_failures_are_redacted_and_summary_limits_are_explicit() {
    let mut presentation = TaskPresentation::default();
    let redactor = Redactor::default_set();
    project_record(
        &mut presentation,
        json!({"progress":{
            "kind":"tool_result", "id":"tool1", "name":"observe", "ok":false,
            "error_preview": format!("Bearer {} {}", "a".repeat(32), "x".repeat(4096)),
        }}),
        &redactor,
    )
    .unwrap();
    let preview = presentation.tools[0].error_preview.as_ref().unwrap();
    assert!(!preview.contains(&"a".repeat(32)));
    assert!(preview.contains("[REDACTED:bearer]"));
    assert!(append_summary(
        &mut presentation,
        vec!["x".repeat(MAX_SUMMARY_BYTES + 1)],
        &redactor
    )
    .is_err());
}

#[test]
fn evidence_projection_excludes_model_claims_and_confidence_and_discloses_coverage() {
    let report = EvidenceReport {
        status: EvidenceStatus::Partial,
        required: true,
        binding_confidence: Some(0.9),
        claim_confidence: Some(0.9),
        interpretation: "Source binding does not establish semantic correctness.".into(),
        warnings: vec!["warning".to_string(); 33],
        ..EvidenceReport::default()
    };
    let presented = EvidencePresentation::from(&report);
    let encoded = serde_json::to_value(&presented).unwrap();
    assert_eq!(presented.status, EvidenceStatus::Partial);
    assert!(presented.truncated);
    assert_eq!(presented.warnings.len(), 32);
    assert!(encoded.get("claim_confidence").is_none());
    assert!(encoded.get("binding_confidence").is_none());
    assert!(encoded.get("claims").is_none());
}
