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
fn authorization_errors_keep_diagnostics_and_distinguish_cancellation() {
    let refused = authorization_failure(Some(127), b"Not authorized");
    assert!(refused.contains("Polkit authentication failed or was refused"));
    assert!(refused.contains("Not authorized"));
    assert!(!refused.contains("cancelled"));
    assert!(authorization_failure(Some(126), b"").contains("was cancelled"));
    assert!(authorization_failure(Some(1), b"request not found").contains("helper failed"));
    assert!(authorization_failure(None, b"").contains("interrupted"));
}

#[tokio::test]
async fn authorization_diagnostics_are_relayed_live_and_bounded() {
    let mut terminal = Vec::new();
    let diagnostic = relay_authorization_diagnostics(&b"Not authorized"[..], &mut terminal)
        .await
        .unwrap();
    assert_eq!(terminal, b"Not authorized");
    assert_eq!(diagnostic, terminal);
    assert!(relay_authorization_diagnostics(&vec![b'x'; 16 * 1_024 + 1][..], tokio::io::sink())
        .await
        .unwrap_err()
        .contains("size limit"));
}
