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
