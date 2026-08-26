use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

fn req(name: &str, input: serde_json::Value) -> ApprovalRequest {
    ApprovalRequest {
        tool_name: name.to_string(),
        tool_input: input,
        reason: "test".to_string(),
    }
}

#[tokio::test]
async fn non_dangerous_tool_passes_through() {
    let gate = ApprovalGate::new(ApprovalConfig::new());
    let out = gate.evaluate("cos_echo", &json!({}), "n/a").await;
    assert!(matches!(out, ApprovalOutcome::Approved { .. }));
}

#[tokio::test]
async fn auto_deny_takes_precedence_over_auto_approve() {
    let cfg = ApprovalConfig::new()
        .auto_approve("dangerous_tool")
        .auto_deny("dangerous_tool");
    let gate = ApprovalGate::new(cfg);
    let out = gate.evaluate("dangerous_tool", &json!({}), "test").await;
    assert!(matches!(out, ApprovalOutcome::Denied { .. }));
}

#[tokio::test]
async fn auto_approve_short_circuits() {
    let cfg = ApprovalConfig::new()
        .auto_approve("safe_tool")
        .dangerous("safe_tool");
    let gate = ApprovalGate::new(cfg);
    let out = gate.evaluate("safe_tool", &json!({}), "test").await;
    match out {
        ApprovalOutcome::Approved { note } => {
            assert!(note.unwrap().contains("auto-approved"));
        }
        other => panic!("expected approved, got {other:?}"),
    }
}

#[tokio::test]
async fn dangerous_no_approver_defers() {
    let cfg = ApprovalConfig::new().dangerous("cos_proc");
    let gate = ApprovalGate::new(cfg);
    let out = gate.evaluate("cos_proc", &json!({}), "kill cmd").await;
    assert!(matches!(out, ApprovalOutcome::Deferred { .. }));
}

#[tokio::test]
async fn dangerous_with_approver_uses_approver() {
    let cfg = ApprovalConfig::new().dangerous("cos_proc");
    let gate = ApprovalGate::new(cfg).with_approver(Arc::new(StaticApprover {
        outcome: ApprovalOutcome::Approved {
            note: Some("ok".to_string()),
        },
    }));
    let out = gate.evaluate("cos_proc", &json!({}), "test").await;
    match out {
        ApprovalOutcome::Approved { note } => assert_eq!(note.as_deref(), Some("ok")),
        other => panic!("expected approved, got {other:?}"),
    }
}

#[tokio::test]
async fn deferring_approver_emits_prompt() {
    let cfg = ApprovalConfig::new().dangerous("cos_proc");
    let gate = ApprovalGate::new(cfg).with_approver(Arc::new(DeferringApprover));
    let out = gate.evaluate("cos_proc", &json!({}), "kill cmd").await;
    match out {
        ApprovalOutcome::Deferred { prompt } => {
            let p = prompt.unwrap();
            assert!(p.contains("cos_proc"));
            assert!(p.contains("kill cmd"));
        }
        other => panic!("expected deferred, got {other:?}"),
    }
}

#[tokio::test]
async fn approver_sees_full_request() {
    struct Capture(AtomicUsize, std::sync::Mutex<Option<ApprovalRequest>>);
    #[async_trait]
    impl Approver for Capture {
        async fn approve(&self, request: &ApprovalRequest) -> ApprovalOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            *self.1.lock().unwrap() = Some(request.clone());
            ApprovalOutcome::Denied {
                reason: Some("captured".to_string()),
            }
        }
    }
    let capture = Arc::new(Capture(AtomicUsize::new(0), std::sync::Mutex::new(None)));
    let cfg = ApprovalConfig::new().dangerous("cos_credential");
    let gate = ApprovalGate::new(cfg).with_approver(capture.clone());
    let _ = gate
        .evaluate("cos_credential", &json!({"action": "set"}), "writes")
        .await;
    assert_eq!(capture.0.load(Ordering::SeqCst), 1);
    let captured = capture.1.lock().unwrap().clone().unwrap();
    assert_eq!(captured.tool_name, "cos_credential");
    assert_eq!(captured.tool_input["action"], "set");
    assert_eq!(captured.reason, "writes");
}

#[tokio::test]
async fn approver_not_invoked_for_non_dangerous() {
    struct ShouldNotBeCalled;
    #[async_trait]
    impl Approver for ShouldNotBeCalled {
        async fn approve(&self, _: &ApprovalRequest) -> ApprovalOutcome {
            panic!("approver should not be called for non-dangerous tool");
        }
    }
    let gate =
        ApprovalGate::new(ApprovalConfig::new()).with_approver(Arc::new(ShouldNotBeCalled));
    let out = gate.evaluate("safe", &json!({}), "n/a").await;
    assert!(matches!(out, ApprovalOutcome::Approved { .. }));
}

#[test]
fn would_short_circuit_when_not_dangerous() {
    let gate = ApprovalGate::new(ApprovalConfig::new());
    assert!(gate.would_short_circuit("anything"));
}

#[test]
fn would_short_circuit_for_auto_deny() {
    let gate = ApprovalGate::new(ApprovalConfig::new().auto_deny("x"));
    assert!(gate.would_short_circuit("x"));
}

#[test]
fn would_short_circuit_for_auto_approve() {
    let gate = ApprovalGate::new(ApprovalConfig::new().auto_approve("x"));
    assert!(gate.would_short_circuit("x"));
}

#[test]
fn would_not_short_circuit_for_dangerous() {
    let gate = ApprovalGate::new(ApprovalConfig::new().dangerous("x"));
    assert!(!gate.would_short_circuit("x"));
}

#[test]
fn config_builder_chains() {
    let cfg = ApprovalConfig::new()
        .auto_approve("a")
        .auto_deny("b")
        .dangerous("c");
    assert!(cfg.auto_approve.contains("a"));
    assert!(cfg.auto_deny.contains("b"));
    assert!(cfg.dangerous.contains("c"));
}

#[test]
fn outcome_serialisation_uses_decision_tag() {
    let approved = ApprovalOutcome::Approved {
        note: Some("ok".to_string()),
    };
    let v = serde_json::to_value(&approved).unwrap();
    assert_eq!(v["decision"], "approved");
    assert_eq!(v["note"], "ok");
}

#[test]
fn outcome_round_trip_for_all_variants() {
    for o in [
        ApprovalOutcome::Approved { note: None },
        ApprovalOutcome::Approved {
            note: Some("ok".to_string()),
        },
        ApprovalOutcome::Denied { reason: None },
        ApprovalOutcome::Denied {
            reason: Some("nope".to_string()),
        },
        ApprovalOutcome::Deferred { prompt: None },
        ApprovalOutcome::Deferred {
            prompt: Some("ask?".to_string()),
        },
    ] {
        let s = serde_json::to_string(&o).unwrap();
        let parsed: ApprovalOutcome = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, o);
    }
}

#[tokio::test]
async fn gate_clone_shares_state() {
    let cfg = ApprovalConfig::new().dangerous("cos_proc");
    let gate = ApprovalGate::new(cfg).with_approver(Arc::new(DeferringApprover));
    let cloned = gate.clone();
    let out_orig = gate.evaluate("cos_proc", &json!({}), "test").await;
    let out_clone = cloned.evaluate("cos_proc", &json!({}), "test").await;
    assert_eq!(out_orig, out_clone);
}
