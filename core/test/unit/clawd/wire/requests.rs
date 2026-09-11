use super::*;
use serde_json::json;

#[test]
fn receipt_reports_are_bounded_data_and_cannot_claim_authority_or_os_verification() {
    let report = json!({
        "id":"00000000-0000-4000-8000-000000000002",
        "app_id":"demo","operation":"get","package_digest":format!("sha256:{}", "a".repeat(64)),
        "outcome":"indeterminate","result":null,"error":"No result was available"
    });
    let valid = json!({"id":"00000000-0000-4000-8000-000000000001","report":report});
    assert!(serde_json::from_value::<ActivityReceiptRecord>(valid.clone()).is_ok());
    for field in ["source","owner_uid","effects_confirmed","grant","declaration"] {
        let mut forged = valid.clone();
        forged["report"][field] = json!("caller chooses");
        assert!(serde_json::from_value::<ActivityReceiptRecord>(forged).is_err(), "{field}");
    }
    let mut oversized = valid;
    oversized["report"]["error"] = json!("x".repeat(2049));
    assert!(serde_json::from_value::<ActivityReceiptRecord>(oversized).is_err());
    assert!(serde_json::from_value::<ActivityReceipts>(json!({"id":"id","owner_uid":0})).is_err());
}

#[test]
fn operation_preview_requests_are_bounded_and_cannot_supply_authority() {
    let good = json!({"app_id":"fs","operation":"write","args":["/home/user/file","--content","draft"]});
    assert!(serde_json::from_value::<OperationPreview>(good.clone()).is_ok());
    for field in ["owner_uid", "grant", "authorized", "execute"] {
        let mut bad = good.clone();
        bad[field] = json!(true);
        assert!(serde_json::from_value::<OperationPreview>(bad).is_err());
    }
    let mut flood = good.clone();
    flood["args"] = json!(vec!["x";65]);
    assert!(serde_json::from_value::<OperationPreview>(flood).is_err());
    let mut long = good;
    long["args"] = json!(["x".repeat(8193)]);
    assert!(serde_json::from_value::<OperationPreview>(long).is_err());
}

#[test]
fn activity_object_requests_are_bounded_closed_references_not_authority() {
    let valid = json!({
        "id":"00000000-0000-4000-8000-000000000001",
        "label":"Status",
        "object":{"app_id":"kv","object_type":"entry","object_id":"release.status"}
    });
    assert!(serde_json::from_value::<ActivityObjectAttach>(valid.clone()).is_ok());
    for field in ["owner_uid", "grant", "operation"] {
        let mut forged = valid.clone();
        forged["object"][field] = json!("unexpected");
        assert!(serde_json::from_value::<ActivityObjectAttach>(forged).is_err());
    }
    for id in ["".to_string(), "x".repeat(1025), "line\nbreak".to_string()] {
        let mut invalid = valid.clone();
        invalid["object"]["object_id"] = json!(id);
        assert!(serde_json::from_value::<ActivityObjectAttach>(invalid).is_err());
    }
    let mut bad_app = valid;
    bad_app["object"]["app_id"] = json!("../other");
    assert!(serde_json::from_value::<ActivityObjectAttach>(bad_app).is_err());
    assert!(serde_json::from_value::<ActivityObjects>(
        json!({"id":"activity-id","owner_uid":0})
    ).is_err());
}

#[test]
fn activity_requests_are_closed_bounded_and_never_choose_an_owner() {
    let create = json!({
        "title": "Release v2",
        "goal": "Publish on Friday",
        "resources": [{"label": "Draft", "reference": "/home/user/release.md"}],
    });
    assert!(serde_json::from_value::<ActivityCreate>(create.clone()).is_ok());
    let mut forged = create.clone();
    forged["owner_uid"] = json!(0);
    assert!(serde_json::from_value::<ActivityCreate>(forged).is_err());
    let mut oversized = create.clone();
    oversized["goal"] = json!("x".repeat(16385));
    assert!(serde_json::from_value::<ActivityCreate>(oversized).is_err());
    let mut flood = create.clone();
    flood["resources"] = json!(vec![json!({"label": "x", "reference": "x"}); 33]);
    assert!(serde_json::from_value::<ActivityCreate>(flood).is_err());
    let mut hidden_authority = create;
    hidden_authority["resources"][0]["capabilities"] = json!(["fs.write:*"]);
    assert!(serde_json::from_value::<ActivityCreate>(hidden_authority).is_err());
    assert!(serde_json::from_value::<ActivityTransition>(
        json!({"id": "activity-1", "state": "automatically_verified"}),
    )
    .is_err());
    assert!(serde_json::from_value::<ActivityRun>(
        json!({"id": "activity-1", "grant": "unexpected"}),
    )
    .is_err());
}

#[test]
fn activity_linkage_is_additive_to_legacy_task_requests() {
    let legacy: TaskSubmit = serde_json::from_value(json!({"prompt": "hello"})).unwrap();
    assert!(legacy.activity_id.is_none());
    assert!(serde_json::to_value(legacy).unwrap().get("activity_id").is_none());
    let linked: TaskSubmit = serde_json::from_value(json!({
        "prompt": "prepare the release",
        "activity_id": "00000000-0000-4000-8000-000000000001",
    }))
    .unwrap();
    assert!(linked.activity_id.is_some());
    assert!(serde_json::from_value::<TaskList>(json!({"activity_id": "../foreign"})).is_err());
}

#[test]
fn a_task_submission_is_closed_and_bounded() {
    let ok = json!({
        "prompt": "summarise the journal",
        "session_id": "sess-1",
        "max_turns": 4,
    });
    assert!(serde_json::from_value::<TaskSubmit>(ok).is_ok());

    // `owner_uid` is authority the daemon derives; a caller cannot
    // smuggle it in beside a legitimate field.
    let smuggled = json!({"prompt": "hi", "owner_uid": 0});
    assert!(serde_json::from_value::<TaskSubmit>(smuggled).is_err());

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
    // `app_session.set_transient` sends `"call": null` to clear the
    // transient capability, and the handler treats absent and null the
    // same way.
    let decoded: AppSessionSetTransient = serde_json::from_value(json!({
        "session_id": "app-1",
        "handle": "h1",
        "call": null,
    }))
    .unwrap();
    let canonical = serde_json::to_value(decoded).unwrap();
    assert!(canonical.get("call").is_none());
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
