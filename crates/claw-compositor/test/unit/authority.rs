use super::*;

#[test]
fn origin_class_never_inherits_another_origins_rights() {
    let state = GrantState {
        read: true,
        write: true,
        panel: true,
        ..GrantState::default()
    };
    assert_eq!(
        state.permissions(Origin::Login, true),
        OS_SESSION | PANEL | CONTEXT
    );
    assert_eq!(state.permissions(Origin::Creator(1), true), CONTEXT);
    assert_eq!(
        state.permissions(Origin::Instance(1), true),
        READ | WRITE | PANEL
    );
    let locker = Origin::from_native(4, 0).unwrap();
    assert_eq!(state.permissions(locker, true), SESSION_LOCK);
    assert_eq!(state.permissions(locker, false), 0);
    for kind in [0, 4, u32::MAX] {
        assert!(Origin::from_native(kind, 1).is_err());
    }
    assert!(Origin::from_native(1, 1).is_err());
    assert!(Origin::from_native(2, 0).is_err());
    assert!(Origin::from_native(3, 0).is_err());
}

#[test]
fn selection_read_write_and_panel_rights_are_independent() {
    for read in [false, true] {
        for write in [false, true] {
            for panel in [false, true] {
                let state = GrantState {
                    read,
                    write,
                    panel,
                    ..GrantState::default()
                };
                let rights = state.permissions(Origin::Instance(1), true);
                assert_eq!(rights & READ != 0, read);
                assert_eq!(rights & WRITE != 0, write);
                assert_eq!(rights & PANEL != 0, panel);
                assert_eq!(rights & (CONTEXT | OS_SESSION), 0);
            }
        }
    }
}

#[test]
fn inactive_or_retired_instances_cannot_keep_protocol_rights() {
    let mut state = GrantState {
        read: true,
        write: true,
        panel: true,
        ..GrantState::default()
    };
    assert_eq!(state.permissions(Origin::Instance(1), false), 0);
    assert!(state.consume_context(false).is_err());
    assert!(!state.context_used);
    state.retired = true;
    for origin in [Origin::Login, Origin::Creator(1), Origin::Instance(1), Origin::Locker] {
        assert_eq!(state.permissions(origin, true), 0);
    }
    assert!(state.add_client(true).is_err());
}

#[test]
fn root_creator_context_is_single_use_even_after_listener_retirement() {
    let mut state = GrantState::default();
    assert!(state.add_listener(true).is_err());
    state.consume_context(true).unwrap();
    state.add_listener(true).unwrap();
    assert!(state.add_listener(true).is_err());
    state.listener = false;
    assert!(state.consume_context(true).is_err());
    assert_eq!(state.permissions(Origin::Creator(1), true), 0);
}

#[test]
fn retirement_stays_nonempty_until_clients_and_listener_are_removed() {
    let mut state = GrantState::default();
    state.consume_context(true).unwrap();
    state.add_listener(true).unwrap();
    state.add_client(true).unwrap();
    state.retired = true;
    assert!(!state.empty());
    state.remove_client().unwrap();
    assert!(!state.empty());
    assert!(state.remove_client().is_err());
    state.listener = false;
    assert!(state.empty());
}

#[test]
fn expired_or_overlong_leases_are_rejected_without_overflow() {
    assert!(validate_expiry(100, 100, MAX_LEASE_MS).is_err());
    assert!(validate_expiry(99, 100, MAX_LEASE_MS).is_err());
    assert!(validate_expiry(100 + MAX_LEASE_MS, 100, MAX_LEASE_MS).is_ok());
    assert!(validate_expiry(101 + MAX_LEASE_MS, 100, MAX_LEASE_MS).is_err());
    assert!(validate_expiry(u64::MAX, u64::MAX - 1, MAX_LEASE_MS).is_ok());
    let mut sequence_value = u64::MAX;
    assert!(sequence(&mut sequence_value).is_err());
    assert_eq!(sequence_value, u64::MAX);
}

#[test]
fn native_selection_handoff_rechecks_display_and_request_expiry() {
    let selection = PendingSelection {
        payload: Arc::new(Payload {
            entries: vec![claw_display_control::selection::Entry {
                mime: "text/plain".into(),
                data: b"owner selection".to_vec(),
            }],
        }),
        expires_ms: 1000,
    };
    assert!(selection.validate(true, 999).is_ok());
    assert!(selection.validate(false, 999).is_err());
    assert!(selection.validate(true, 1000).is_err());
    assert!(selection.validate(true, 1001).is_err());
}
