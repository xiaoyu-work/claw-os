use super::*;
use crate::caps::{Scope, Verb};

fn context<'a>(event_id: &'a str) -> ReferenceContext<'a> {
    ReferenceContext {
        owner_uid: 1000,
        session_id: "session-a",
        task_id: "task-a",
        extension_id: "observer",
        manifest_digest: "a",
        capability_generation: "b",
        event_id,
        deadline: MonotonicDeadlineNs::after(std::time::Duration::from_secs(1)).unwrap(),
    }
}

fn policy(index: usize, tool: &str, policy_id: &str) -> ExtensionActionPolicy {
    ExtensionActionPolicy {
        requested_index: index,
        tool: tool.to_string(),
        policy_id: policy_id.to_string(),
    }
}

fn binding(reference: CapabilityReference, cap: Cap) -> ActionReferenceBinding {
    ActionReferenceBinding {
        reference,
        action_id: "action-a".to_string(),
        tool: "now".to_string(),
        policy_id: "builtin.now/v1".to_string(),
        input_digest: crate::crypto::sha256_hex(b"{}"),
        capability: cap,
        operation_digest: crate::crypto::sha256_hex(b"operation"),
    }
}

#[test]
fn references_are_opaque_single_use_and_event_scoped() {
    let store = Arc::new(CapabilityReferenceStore::new(1));
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    let lease = store
        .issue_event(
            &context("event-a"),
            std::slice::from_ref(&cap),
            &[policy(0, "now", "builtin.now/v1")],
        )
        .unwrap();
    assert_eq!(lease.references().len(), 1);
    assert_eq!(lease.references()[0].handle.len(), 64);
    let reference = lease.references()[0].clone();
    lease
        .consume_all(&[binding(reference.clone(), cap.clone())])
        .unwrap();

    let replay = store
        .issue_event(
            &context("event-a"),
            std::slice::from_ref(&cap),
            &[policy(0, "now", "builtin.now/v1")],
        )
        .unwrap();
    assert!(replay
        .consume_all(&[binding(reference, cap)])
        .unwrap_err()
        .contains("invalid or expired"));
}

#[test]
fn cross_session_and_cross_event_references_fail_and_purge_the_lease() {
    let store = Arc::new(CapabilityReferenceStore::new(1));
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    let lease = store
        .issue_event(
            &context("event-a"),
            std::slice::from_ref(&cap),
            &[policy(0, "now", "builtin.now/v1")],
        )
        .unwrap();
    let reference = lease.references()[0].clone();
    let mut wrong = context("event-b");
    wrong.session_id = "session-b";
    let wrong_lease = IssuedReferenceLease {
        store: Arc::clone(&store),
        context: wrong.owned(),
        references: vec![reference.clone()],
        keys: vec![crate::crypto::sha256_bytes(reference.handle.as_bytes())],
        resolved: false,
    };
    assert!(wrong_lease.consume_all(&[binding(reference, cap)]).is_err());
    assert_eq!(store.len(), 0);
}

#[test]
fn invalid_second_action_executes_no_prefix_and_replays_nothing() {
    let store = Arc::new(CapabilityReferenceStore::new(2));
    let first = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    let second = Cap::new(Verb::UI_NOTIFY, Scope::Wild);
    let lease = store
        .issue_event(
            &context("event-a"),
            &[first.clone(), second.clone()],
            &[
                policy(0, "now", "builtin.now/v1"),
                policy(1, "safe_notify", "fixture.notify/v1"),
            ],
        )
        .unwrap();
    let bindings = vec![
        binding(lease.references()[0].clone(), first),
        ActionReferenceBinding {
            reference: lease.references()[1].clone(),
            action_id: "action-b".to_string(),
            tool: "substituted".to_string(),
            policy_id: "fixture.notify/v1".to_string(),
            input_digest: crate::crypto::sha256_hex(b"{}"),
            capability: second,
            operation_digest: crate::crypto::sha256_hex(b"operation-b"),
        },
    ];
    assert!(lease.consume_all(&bindings).is_err());
    assert_eq!(store.len(), 0);
}

#[test]
fn dropping_or_expiring_a_lease_purges_every_reference() {
    let store = Arc::new(CapabilityReferenceStore::new(1));
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    {
        let _lease = store
            .issue_event(
                &context("event-a"),
                &[cap],
                &[policy(0, "now", "builtin.now/v1")],
            )
            .unwrap();
        assert_eq!(store.len(), 1);
    }
    assert_eq!(store.len(), 0);

    let mut expired = context("event-b");
    expired.deadline = MonotonicDeadlineNs(1);
    assert!(store
        .issue_event(
            &expired,
            &[Cap::new(Verb::SYS_OBSERVE, Scope::name("time"))],
            &[policy(0, "now", "builtin.now/v1")],
        )
        .is_err());
}

#[test]
fn one_extension_store_cannot_exhaust_another() {
    let flooded = Arc::new(CapabilityReferenceStore::new(1));
    let healthy = Arc::new(CapabilityReferenceStore::new(1));
    let cap = Cap::new(Verb::SYS_OBSERVE, Scope::name("time"));
    let _held = flooded
        .issue_event(
            &context("flood"),
            std::slice::from_ref(&cap),
            &[policy(0, "now", "builtin.now/v1")],
        )
        .unwrap();
    assert!(flooded
        .issue_event(
            &context("overflow"),
            std::slice::from_ref(&cap),
            &[policy(0, "now", "builtin.now/v1")],
        )
        .is_err());
    assert!(healthy
        .issue_event(
            &context("healthy"),
            &[cap],
            &[policy(0, "now", "builtin.now/v1")],
        )
        .is_ok());
}

#[test]
fn four_action_references_share_one_deadline_and_consume_all_at_once() {
    let store = Arc::new(CapabilityReferenceStore::new(4));
    let mut near = context("four-actions");
    near.deadline = MonotonicDeadlineNs::after(std::time::Duration::from_secs(2)).unwrap();
    let capabilities = (0..4)
        .map(|index| Cap::new(Verb::SYS_OBSERVE, Scope::name(format!("time-{index}"))))
        .collect::<Vec<_>>();
    let policies = (0..4)
        .map(|index| {
            policy(
                index,
                &format!("safe_{index}"),
                &format!("fixture.safe/{index}"),
            )
        })
        .collect::<Vec<_>>();
    let lease = store.issue_event(&near, &capabilities, &policies).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let bindings = lease
        .references()
        .iter()
        .enumerate()
        .map(|(index, reference)| ActionReferenceBinding {
            reference: reference.clone(),
            action_id: format!("action-{index}"),
            tool: format!("safe_{index}"),
            policy_id: format!("fixture.safe/{index}"),
            input_digest: crate::crypto::sha256_hex(format!("input-{index}").as_bytes()),
            capability: capabilities[index].clone(),
            operation_digest: crate::crypto::sha256_hex(format!("operation-{index}").as_bytes()),
        })
        .collect::<Vec<_>>();
    lease.consume_all(&bindings).unwrap();
    assert_eq!(store.len(), 0);
}
