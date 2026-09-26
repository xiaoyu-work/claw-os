use super::*;

#[test]
fn approval_readback_accepts_the_exact_already_consumed_decision() {
    let state = json!({"statuses": [{"id": "ap-123", "status": "consumed"}]});
    assert!(approval_decision_confirmed(&state, "ap-123", ReviewDecision::ApproveOnce));
    assert!(!approval_decision_confirmed(&state, "ap-other", ReviewDecision::ApproveOnce));
    assert!(!approval_decision_confirmed(&state, "ap-123", ReviewDecision::Deny));
    for status in ["pending", "resolving", "unknown", "denied"] {
        let state = json!({"statuses": [{"id": "ap-123", "status": status}]});
        assert!(!approval_decision_confirmed(&state, "ap-123", ReviewDecision::ApproveOnce));
    }
}

#[test]
fn approval_parser_projects_catalog_text_and_human_scope() {
    let value = json!({
        "id": "approval-1",
        "verb": "fs.meta",
        "scope": {"kind": "path", "value": "/**"},
        "reason": "Inspect file details: path:/**",
        "session": "session-1",
        "requested_at": 42,
        "requester": "Claw Agent",
        "risk": "low"
    });

    assert_eq!(
        parse_approval(&value, "pending").unwrap(),
        ApprovalRequest {
            id: "approval-1".into(),
            verb: "fs.meta".into(),
            scope: json!({"kind": "path", "value": "/**"}),
            label: "Inspect file details".into(),
            description: "See file names, sizes, and timestamps without reading the contents."
                .into(),
            target: "All files and folders on this computer".into(),
            reason: "Inspect file details: path:/**".into(),
            status: "pending".into(),
            session: "session-1".into(),
            requested_at: 42,
            requester: Some("Claw Agent".into()),
            risk: Some("low".into()),
            decided_at: None,
            duration: None,
            note: None,
        }
    );
}
