use super::*;

#[test]
fn every_variant_has_a_stable_kind() {
    let events = [
        JournalEvent::SessionStarted {
            owner_uid: 1000,
            origin: Origin::System,
            delegation: None,
            parent: None,
        },
        JournalEvent::MutationCommitted {
            operation: OperationId::generate(),
            duration_ms: 1,
        },
    ];
    for event in events {
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(
            encoded.get("kind").and_then(|value| value.as_str()),
            Some(event.kind()),
            "the stored tag must equal the ACL name"
        );
    }
}

#[test]
fn labels_reject_values_that_were_never_bounded() {
    // A label produced by this crate is always a token...
    assert_eq!(
        Label::new("system.package.install").as_str(),
        "system.package.install"
    );
    assert_eq!(Label::new("a value with spaces").as_str(), "<unloggable>");
    // ...and one arriving from disk must have been.
    let bad = serde_json::from_str::<Label>("\"has spaces\"");
    assert!(bad.is_err(), "an unbounded label must not decode");
}

#[test]
fn references_accept_scopes_and_reject_control_characters() {
    assert_eq!(
        Reference::new("path:/var/lib/cos").as_str(),
        "path:/var/lib/cos"
    );
    assert_eq!(
        Reference::new("a scope with spaces").as_str(),
        "<unloggable>"
    );
    assert!(serde_json::from_str::<Reference>("\"a\\nb\"").is_err());
}

#[test]
fn digests_must_be_sha256_hex() {
    let digest = Digest::of(b"hello");
    assert_eq!(digest.as_str().len(), 64);
    assert!(Digest::parse("nope").is_none());
    assert!(serde_json::from_str::<Digest>("\"abc\"").is_err());
}

#[test]
fn content_refs_reject_unknown_fields() {
    let json = r#"{"store":"session-turns","digest":"aa","bytes":1,"body":"secret"}"#;
    assert!(
        serde_json::from_str::<ContentRef>(json).is_err(),
        "a decorated reference must not decode into the schema"
    );
}

#[test]
fn no_variant_carries_free_form_json() {
    // The schema is closed by construction: a caller cannot hand the
    // journal an object where a bounded field is expected.
    let json = r#"{"kind":"tool_started","turn":1,"tool":{"nested":"payload"},
                   "tool_use_id":"x","known":true}"#;
    assert!(serde_json::from_str::<JournalEvent>(json).is_err());
}

#[test]
fn mutation_events_name_their_operation() {
    let operation = OperationId::generate();
    let started = JournalEvent::MutationStarted {
        operation: operation.clone(),
        route: Label::new("system.service.control"),
        idempotency: crate::audit_policy::text_digest("key"),
        grant: None,
        session_mutation: Some(4),
    };
    let committed = JournalEvent::MutationCommitted {
        operation: operation.clone(),
        duration_ms: 12,
    };
    assert_eq!(started.operation(), Some(&operation));
    assert!(started.opens_mutation());
    assert!(!started.resolves_mutation());
    assert!(committed.resolves_mutation());
    assert!(committed.is_closure());
    // Flags say the outcome is unknown; they must not retire a bracket.
    let orphaned = JournalEvent::MutationOrphaned {
        operation: operation.clone(),
        route: Label::new("system.package.install"),
        detected_by: RecoverySource::DaemonStart,
        opened_in_epoch: 1,
    };
    assert!(orphaned.flags_mutation());
    assert!(!orphaned.resolves_mutation());
    assert!(orphaned.is_closure());
    let resolved = JournalEvent::MutationResolved {
        operation,
        outcome: Resolution::Abandoned,
        decided_by: Reference::new("uid:0"),
    };
    assert!(resolved.resolves_mutation());
}

#[test]
fn events_round_trip_through_their_stored_form() {
    let event = JournalEvent::ToolFinished {
        turn: 3,
        tool: Label::new("cos_recall"),
        tool_use_id: Label::new("tu-9"),
        known: true,
        success: false,
        latency_ms: 40,
        bytes_returned: 128,
        error: Some(crate::audit_policy::text_digest("provider said no")),
    };
    let line = serde_json::to_string(&event).unwrap();
    let back: JournalEvent = serde_json::from_str(&line).unwrap();
    assert_eq!(event, back);
}
