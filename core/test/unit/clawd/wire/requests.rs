use super::*;
use serde_json::json;

#[test]
fn a_task_submission_is_closed_and_bounded() {
    let ok = json!({
        "prompt": "summarise the journal",
        "session_id": "sess-1",
        "max_turns": 4,
    });
    assert!(serde_json::from_value::<TaskSubmit>(ok).is_ok());

    // Identity, source, and presence are authority the daemon derives; a
    // caller cannot smuggle any of them in beside a legitimate field.
    for field in [
        json!({"prompt": "hi", "owner_uid": 0}),
        json!({"prompt": "hi", "source": "local-cli"}),
        json!({"prompt": "hi", "attended": true}),
        json!({"prompt": "hi", "local": true}),
    ] {
        assert!(serde_json::from_value::<TaskSubmit>(field).is_err());
    }

    let wrong_type = json!({"prompt": ["hi"]});
    assert!(serde_json::from_value::<TaskSubmit>(wrong_type).is_err());

    let oversized = json!({"prompt": "x".repeat(PROMPT_BYTES + 1)});
    assert!(serde_json::from_value::<TaskSubmit>(oversized).is_err());
}

#[test]
fn a_control_route_refuses_a_field_it_never_declared() {
    let ok = json!({"session": "sess-1", "action": "status"});
    assert!(serde_json::from_value::<AudioControl>(ok).is_ok());

    let extra = json!({"session": "sess-1", "action": "status", "sudo": true});
    assert!(serde_json::from_value::<AudioControl>(extra).is_err());

    let missing = json!({"action": "status"});
    assert!(serde_json::from_value::<AudioControl>(missing).is_err());
}

#[test]
fn optional_fields_round_trip_to_the_shape_handlers_read() {
    let decoded: PowerControl =
        serde_json::from_value(json!({"session": "s", "action": "suspend"})).unwrap();
    let canonical = serde_json::to_value(decoded).unwrap();
    assert_eq!(canonical, json!({"session": "s", "action": "suspend"}));
    assert!(canonical.get("confirm").is_none());

    let with_flag: PowerControl =
        serde_json::from_value(json!({"session": "s", "action": "off", "confirm": true})).unwrap();
    assert_eq!(
        serde_json::to_value(with_flag).unwrap(),
        json!({"session": "s", "action": "off", "confirm": true})
    );
}

#[test]
fn an_explicit_null_optional_decodes_to_an_absent_field() {
    // Clearing transient authority carries neither half of the opaque
    // authorization/action binding.
    let decoded: AppSessionSetTransient = serde_json::from_value(json!({
        "session_id": "app-1",
        "handle": "h1",
        "authorization": null,
        "action_digest": null,
    }))
    .unwrap();
    let canonical = serde_json::to_value(decoded).unwrap();
    assert!(canonical.get("authorization").is_none());
    assert!(canonical.get("action_digest").is_none());
    assert!(serde_json::from_value::<AppSessionSetTransient>(json!({
        "session_id": "app-1",
        "handle": "h1",
        "call": {"tool": "legacy", "args": {}},
    }))
    .is_err());
}

#[test]
fn a_structured_field_keeps_its_shape_for_the_owning_authority() {
    let scope = json!({"kind": "path", "path": "/home/user"});
    let decoded: PermissionRequest = serde_json::from_value(json!({
        "verb": "fs.read",
        "scope": scope,
        "reason": "read the report",
    }))
    .unwrap();
    let canonical = serde_json::to_value(decoded).unwrap();
    assert_eq!(canonical["scope"], scope);
}

#[test]
fn a_rollback_body_requires_every_field_the_route_acts_on() {
    let ok = json!({
        "session": "sess-1",
        "mutation_session": "sess-1",
        "mutation_seq": 3,
        "unit": "ssh.service",
        "active": true,
    });
    assert!(serde_json::from_value::<ServiceRestore>(ok).is_ok());

    let missing_seq = json!({
        "session": "sess-1",
        "mutation_session": "sess-1",
        "unit": "ssh.service",
        "active": true,
    });
    assert!(serde_json::from_value::<ServiceRestore>(missing_seq).is_err());

    let unit_is_not_a_name = json!({
        "session": "sess-1",
        "mutation_session": "sess-1",
        "mutation_seq": 3,
        "unit": "ssh.service; rm -rf /",
        "active": true,
    });
    assert!(serde_json::from_value::<ServiceRestore>(unit_is_not_a_name).is_err());
}

#[test]
fn a_long_poll_body_caps_the_wait_the_caller_asks_for() {
    let ok = json!({"id": "task-1", "timeout_ms": 60_000});
    assert!(serde_json::from_value::<TaskWait>(ok).is_ok());

    // Without this bound the route computes `Instant::now() +
    // Duration::from_millis(u64::MAX)` and pins the connection.
    let absurd = json!({"id": "task-1", "timeout_ms": u64::MAX});
    assert!(serde_json::from_value::<TaskWait>(absurd).is_err());
}

#[test]
fn approval_status_ids_are_a_bounded_list() {
    let ok = json!({"ids": ["req-1", "req-2"]});
    assert!(serde_json::from_value::<PermissionStatus>(ok).is_ok());

    let flood = json!({"ids": vec!["req"; 65]});
    assert!(serde_json::from_value::<PermissionStatus>(flood).is_err());

    let not_a_list = json!({"ids": "req-1"});
    assert!(serde_json::from_value::<PermissionStatus>(not_a_list).is_err());
}
