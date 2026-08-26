use super::*;
use crate::agent::llm::Role;
use serde_json::json;

fn trajectory(id: &str, name: &str, result: &str, is_error: bool) -> Vec<Message> {
    vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: json!({}),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                is_error,
                content: result.to_string(),
            }],
        },
    ]
}

#[test]
fn valid_citation_binds_result_hash() {
    let messages = trajectory("call_1", "cos_sysinfo", "{\"load\":2}", false);
    let report = verify_answer(
        "why is my computer slow?",
        "Load is elevated. [evidence:call_1 confidence=0.92]",
        &messages,
    );
    assert_eq!(report.status, EvidenceStatus::Verified);
    assert_eq!(report.verified_claims, 1);
    assert_eq!(report.sources[0].result_sha256.len(), 64);
    assert_eq!(report.binding_confidence, Some(1.0));
    assert_eq!(report.claim_confidence, Some(0.92));
}

#[test]
fn unknown_call_id_is_invalid() {
    let messages = trajectory("call_1", "cos_sysinfo", "{}", false);
    let report = verify_answer(
        "why is my computer slow?",
        "Load is elevated. [evidence:invented confidence=0.9]",
        &messages,
    );
    assert_eq!(report.status, EvidenceStatus::Invalid);
    assert!(!report.claims[0].verified);
}

#[test]
fn live_request_without_tools_is_missing() {
    let report = verify_answer("当前系统为什么很慢", "It is overloaded.", &[]);
    assert_eq!(report.status, EvidenceStatus::Missing);
    assert_eq!(report.binding_confidence, Some(0.0));
    assert_eq!(report.claim_confidence, None);
}

#[test]
fn ordinary_answer_without_tools_needs_no_evidence() {
    let report = verify_answer("Explain what a CPU is", "A CPU executes instructions.", &[]);
    assert_eq!(report.status, EvidenceStatus::NotRequired);
    assert!(!report.required);
}

#[test]
fn error_result_caps_confidence() {
    let messages = trajectory("call_1", "cos_sysinfo", "permission denied", true);
    let report = verify_answer(
        "why is my computer slow?",
        "The probe failed. [evidence:call_1 confidence=0.95]",
        &messages,
    );
    assert_eq!(report.status, EvidenceStatus::Verified);
    assert_eq!(report.binding_confidence, Some(1.0));
    assert_eq!(report.claim_confidence, Some(0.4));
}

#[test]
fn presentation_strips_internal_evidence_markers() {
    assert_eq!(
        strip_markers(
            "Link is idle. [evidence:call_1 confidence=0.95]\n\
             DNS is unresolved. [evidence:call_2 confidence=0.40]"
        ),
        "Link is idle.\nDNS is unresolved."
    );
}

#[test]
fn unrelated_tool_use_does_not_force_evidence() {
    let messages = trajectory("call_1", "write_file", "ok", false);
    let report = verify_answer("create a config file", "Done.", &messages);
    assert_eq!(report.status, EvidenceStatus::NotRequired);
    assert!(!report.required);
    assert_eq!(report.sources.len(), 1);
}
