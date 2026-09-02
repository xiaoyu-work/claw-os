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
        expires_at_ms: now_ms() + 1000,
    }
}

#[test]
fn references_are_opaque_single_use_and_event_scoped() {
    let store = CapabilityReferenceStore::default();
    let cap = Cap::new(Verb::FS_READ, Scope::path("/workspace/file"));
    let issued = store.issue(&context("event-a"), std::slice::from_ref(&cap)).unwrap();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].handle.len(), 64);
    assert_eq!(
        store.consume(&context("event-a"), &issued[0]).unwrap(),
        cap
    );
    assert!(store
        .consume(&context("event-a"), &issued[0])
        .unwrap_err()
        .contains("invalid or expired"));
}

#[test]
fn guessed_cross_session_and_cross_event_references_fail_identically() {
    let store = CapabilityReferenceStore::default();
    let cap = Cap::unscoped(Verb::UI_NOTIFY);
    let issued = store.issue(&context("event-a"), &[cap]).unwrap();

    let mut wrong_session = context("event-a");
    wrong_session.session_id = "session-b";
    assert_eq!(
        store.consume(&wrong_session, &issued[0]).unwrap_err(),
        "capability reference is invalid or expired"
    );

    let guessed = CapabilityReference {
        requested_index: 0,
        handle: "f".repeat(64),
    };
    assert_eq!(
        store.consume(&context("event-a"), &guessed).unwrap_err(),
        "capability reference is invalid or expired"
    );

    let second = store
        .issue(
            &context("event-a"),
            &[Cap::unscoped(Verb::UI_NOTIFY)],
        )
        .unwrap();
    assert_eq!(
        store
            .consume(&context("event-b"), &second[0])
            .unwrap_err(),
        "capability reference is invalid or expired"
    );
}

#[test]
fn expired_references_are_rejected() {
    let store = CapabilityReferenceStore::default();
    let mut expired = context("event-a");
    expired.expires_at_ms = now_ms();
    assert!(store
        .issue(&expired, &[Cap::unscoped(Verb::UI_NOTIFY)])
        .is_err());
}
