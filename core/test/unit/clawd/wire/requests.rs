use super::*;
use serde_json::json;

#[test]
fn system_review_prepare_cannot_carry_approval_or_owner_authority() {
    let request = json!({
        "source": "/home/user/downloads/app",
        "expected_package": {
            "kind": "app", "id": "example", "content_digest": "sha256:example",
            "tier": "user"
        }
    });
    assert!(serde_json::from_value::<SystemReviewPrepare>(request.clone()).is_ok());
    for (field, value) in [
        ("owner_uid", json!(0)),
        ("approved", json!(true)),
        ("permissions", json!(["*"])),
        ("session", json!("forged")),
        ("contract_digest", json!("not-a-user-decision")),
    ] {
        let mut forged = request.clone();
        forged[field] = value;
        assert!(serde_json::from_value::<SystemReviewPrepare>(forged).is_err());
    }
    let mut oversized = request;
    oversized["source"] = json!("x".repeat(PATH_BYTES + 1));
    assert!(serde_json::from_value::<SystemReviewPrepare>(oversized).is_err());
}

#[test]
fn system_review_decision_is_not_a_capability_grant() {
    let request = json!({
        "id": "rv-0123456789abcdef0123456789abcdef",
        "owner_uid": 1000,
        "decision": "approve",
    });
    assert!(serde_json::from_value::<SystemReviewDecide>(request.clone()).is_ok());
    for (field, value) in [
        ("duration", json!("forever")),
        ("caps", json!(["*"])),
        ("granted", json!(true)),
        ("source", json!("/another/package")),
    ] {
        let mut forged = request.clone();
        forged[field] = value;
        assert!(serde_json::from_value::<SystemReviewDecide>(forged).is_err());
    }
}

#[test]
fn system_review_cancellation_cannot_be_changed_into_approval() {
    let id = "rv-0123456789abcdef0123456789abcdef";
    assert!(serde_json::from_value::<SystemReviewId>(json!({"id": id})).is_ok());
    for (field, value) in [
        ("decision", json!("approve")),
        ("owner_uid", json!(0)),
        ("approved", json!(true)),
    ] {
        let mut forged = json!({"id": id});
        forged[field] = value;
        assert!(serde_json::from_value::<SystemReviewId>(forged).is_err());
    }
}

#[test]
fn summary_ai_wire_is_bounded_and_cannot_carry_provider_or_owner_authority() {
    let base = json!({
        "session": "synthetic-session", "app_id": "summarize",
        "origin": "external-content", "prompt": "external text",
        "system": "summarize data", "max_units": 4000, "tools": [],
    });
    let limit = crate::clawd::wire::bounded::FILE_TEXT_MAX_BYTES;
    assert_eq!(limit, 1_000_000);
    for field in ["prompt", "system"] {
        let mut body = base.clone();
        body[field] = json!("\u{00e9}".repeat(limit / 2));
        assert!(serde_json::from_value::<AiChat>(body.clone()).is_ok());
        body[field] = json!(format!("{}x", "\u{00e9}".repeat(limit / 2)));
        assert!(serde_json::from_value::<AiChat>(body).is_err());
    }
    for (field, value) in [
        ("model", json!("caller-model")),
        ("provider", json!("caller-provider")),
        ("owner_uid", json!(0)),
        ("safety", json!("minimal")),
        ("max_units", json!(-1)),
        ("max_units", json!("4000")),
    ] {
        let mut body = base.clone();
        body[field] = value;
        assert!(serde_json::from_value::<AiChat>(body).is_err());
    }
}

#[test]
fn requested_model_wire_is_optional_bounded_and_cannot_select_a_provider() {
    let request = json!({"prompt": "hello", "model": "provider/model-v2"});
    let parsed = serde_json::from_value::<TaskSubmit>(request.clone());
    assert!(parsed.is_ok(), "{parsed:?}");
    let mut oversized = request.clone();
    oversized["model"] = json!("m".repeat(257));
    assert!(serde_json::from_value::<TaskSubmit>(oversized).is_err());
    let mut forged = request;
    forged["provider"] = json!("other-provider");
    assert!(serde_json::from_value::<TaskSubmit>(forged).is_err());
}

#[test]
fn task_workspace_wire_is_optional_bounded_and_carries_no_authority() {
    let parsed: TaskSubmit = serde_json::from_value(json!({
        "prompt": "hello",
        "workspace": "projects/claw",
    }))
    .unwrap();
    assert_eq!(parsed.workspace.unwrap().as_str(), "projects/claw");
    assert!(serde_json::from_value::<TaskSubmit>(json!({
        "prompt": "hello",
        "workspace": "x".repeat(4097),
    }))
    .is_err());
    assert!(serde_json::from_value::<TaskWorkspaceResolve>(json!({
        "path": "project",
        "caps": ["fs.write:**"],
    }))
    .is_err());
}

#[test]
fn durable_queue_predecessor_is_a_bounded_task_identity_only() {
    let parsed: TaskSubmit = serde_json::from_value(json!({
        "prompt": "follow up",
        "session_id": "session-1",
        "after_task_id": "task-previous",
    }))
    .unwrap();
    assert_eq!(
        parsed.after_task_id.unwrap().as_str(),
        "task-previous"
    );
    assert!(serde_json::from_value::<TaskSubmit>(json!({
        "prompt": "follow up",
        "after_task_id": "../foreign",
    }))
    .is_err());
}

#[test]
fn activity_policy_wire_is_closed_bounded_and_cannot_supply_authority() {
    let valid = json!({"id":"00000000-0000-4000-8000-000000000001","policy":{"rules":[
        {"verb":"fs.delete","mode":"deny","scopes":[]}
    ]}});
    assert!(serde_json::from_value::<ActivityCapabilityPolicySet>(valid.clone()).is_ok());
    for field in ["owner_uid", "caps", "grant", "enabled", "revision"] {
        let mut forged = valid.clone();
        forged[field] = json!(0);
        assert!(serde_json::from_value::<ActivityCapabilityPolicySet>(forged).is_err());
        let mut forged = valid.clone();
        forged["policy"][field] = json!(0);
        assert!(serde_json::from_value::<ActivityCapabilityPolicySet>(forged).is_err());
    }
    for rule in [
        json!({"verb":"unknown.action","mode":"deny","scopes":[]}),
        json!({"verb":"fs.read","mode":"normal","scopes":[{"kind":"wild"}]}),
        json!({"verb":"fs.read","mode":"normal","scopes":[{"kind":"host","value":"example.test"}]}),
        json!({"verb":"fs.delete","mode":"deny","scopes":[{"kind":"path","value":"/workspace"}]}),
    ] {
        let mut malformed = valid.clone();
        malformed["policy"]["rules"] = json!([rule]);
        assert!(serde_json::from_value::<ActivityCapabilityPolicySet>(malformed).is_err());
    }
    let mut huge = valid;
    huge["policy"]["rules"] = json!(vec![
        json!({"verb":"fs.delete","mode":"deny","scopes":[]});
        65
    ]);
    assert!(serde_json::from_value::<ActivityCapabilityPolicySet>(huge).is_err());
    assert!(serde_json::from_value::<ActivityCapabilityPolicyEnabled>(
        json!({"id":"id","enabled":false})
    )
    .is_err());
}

#[test]
fn activity_scheduling_wire_is_closed_and_only_selects_pending_admission_class() {
    let valid = json!({
        "id":"00000000-0000-4000-8000-000000000001",
        "expected_revision":2,
        "priority":"foreground",
    });
    assert!(serde_json::from_value::<ActivitySchedulingPolicySet>(valid.clone()).is_ok());
    for field in [
        "owner_uid",
        "caps",
        "grant",
        "consent",
        "budget",
        "preempt",
        "cancel_running",
        "completed",
    ] {
        let mut forged = valid.clone();
        forged[field] = json!(true);
        assert!(
            serde_json::from_value::<ActivitySchedulingPolicySet>(forged).is_err(),
            "{field}"
        );
    }
    let mut invalid = valid;
    invalid["priority"] = json!("urgent");
    assert!(serde_json::from_value::<ActivitySchedulingPolicySet>(invalid).is_err());
}

fn file_replace_body() -> serde_json::Value {
    json!({
        "session":"session-1",
        "path":"/home/owner/document",
        "expected":{
            "sha256":format!("sha256:{}", "a".repeat(64)), "size":65536,
            "device":1, "inode":2, "mode":33152,
            "modified_ns":-1, "changed_ns":1
        },
        "content_base64":""
    })
}

#[test]
fn file_replace_wire_is_closed_and_requires_an_explicit_nullable_precondition() {
    let body = file_replace_body();
    assert!(serde_json::from_value::<FileReplace>(body.clone()).is_ok());
    let mut absent = body.clone();
    absent["expected"] = serde_json::Value::Null;
    let canonical =
        serde_json::to_value(serde_json::from_value::<FileReplace>(absent.clone()).unwrap())
            .unwrap();
    assert!(canonical.get("expected").unwrap().is_null());
    absent.as_object_mut().unwrap().remove("expected");
    assert!(serde_json::from_value::<FileReplace>(absent).is_err());
    for field in [
        "owner_uid",
        "grant",
        "directory",
        "force",
        "follow_symlinks",
    ] {
        let mut forged = body.clone();
        forged[field] = json!(true);
        assert!(
            serde_json::from_value::<FileReplace>(forged).is_err(),
            "{field}"
        );
    }
    for field in ["uid", "gid", "owner_uid", "grant", "link_count"] {
        let mut forged = body.clone();
        forged["expected"][field] = json!(0);
        assert!(
            serde_json::from_value::<FileReplace>(forged).is_err(),
            "{field}"
        );
    }
    for key in [
        "sha256",
        "size",
        "device",
        "inode",
        "mode",
        "modified_ns",
        "changed_ns",
    ] {
        let mut missing = body.clone();
        missing["expected"].as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<FileReplace>(missing).is_err(),
            "{key}"
        );
    }
}

#[test]
fn file_replace_wire_enforces_hash_integer_path_and_content_bounds() {
    let body = file_replace_body();
    for hash in [
        "sha256:ABC".to_string(),
        format!("sha256:{}", "A".repeat(64)),
        "a".repeat(64),
        format!("sha256:{}", "f".repeat(65)),
    ] {
        let mut bad = body.clone();
        bad["expected"]["sha256"] = json!(hash);
        assert!(serde_json::from_value::<FileReplace>(bad).is_err());
    }
    for (field, value) in [
        ("size", json!(65537)),
        ("size", json!(-1)),
        ("size", json!(true)),
        ("device", json!(-1)),
        ("inode", json!("3")),
        ("mode", json!(u64::from(u32::MAX) + 1)),
        ("modified_ns", json!(u64::MAX)),
        ("changed_ns", json!(1.5)),
    ] {
        let mut bad = body.clone();
        bad["expected"][field] = value;
        assert!(
            serde_json::from_value::<FileReplace>(bad).is_err(),
            "{field}"
        );
    }
    for (field, size) in [("path", 4097), ("content_base64", 90001), ("session", 129)] {
        let mut bad = body.clone();
        bad[field] = json!("x".repeat(size));
        assert!(
            serde_json::from_value::<FileReplace>(bad).is_err(),
            "{field}"
        );
    }
    let mut exact = body;
    exact["path"] = json!("x".repeat(4096));
    exact["content_base64"] = json!("x".repeat(90000));
    assert!(serde_json::from_value::<FileReplace>(exact).is_ok());
}

#[test]
fn file_replace_relay_payload_retains_inner_route_validation_and_structured_bounds() {
    use crate::clawd::routes::Command;

    let mut body = file_replace_body();
    body["content_base64"] = json!("A".repeat(87384));
    assert!(Structured::parse(body.clone()).is_err());
    let relay = json!({"session_id":"session-1","handle":"opaque","command":"system.file.replace","params":body});
    let decoded: AppSessionRelay = serde_json::from_value(relay.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), relay);
    let mut forged = relay.clone();
    forged["params"]["unexpected"] = json!("extra");
    let forged: AppSessionRelay = serde_json::from_value(forged).unwrap();
    assert!((Command::SystemFileReplace.route().decode)(
        serde_json::to_value(forged.params.unwrap()).unwrap()
    )
    .is_err());
    let mut unrelated = relay;
    unrelated["params"] = json!({"large_text":"x".repeat(65537)});
    assert!(Structured::parse(unrelated["params"].clone()).is_err());
    let unrelated: AppSessionRelay = serde_json::from_value(unrelated).unwrap();
    assert!((Command::SystemFileReplace.route().decode)(
        serde_json::to_value(unrelated.params.unwrap()).unwrap()
    )
    .is_err());
}

#[test]
fn receipt_reports_are_bounded_data_and_cannot_claim_authority_or_os_verification() {
    let report = json!({
        "id":"00000000-0000-4000-8000-000000000002",
        "app_id":"demo","operation":"get","package_digest":format!("sha256:{}", "a".repeat(64)),
        "outcome":"indeterminate","result":null,"error":"No result was available"
    });
    let valid = json!({"id":"00000000-0000-4000-8000-000000000001","report":report});
    assert!(serde_json::from_value::<ActivityReceiptRecord>(valid.clone()).is_ok());
    for field in [
        "source",
        "owner_uid",
        "effects_confirmed",
        "grant",
        "declaration",
    ] {
        let mut forged = valid.clone();
        forged["report"][field] = json!("caller chooses");
        assert!(
            serde_json::from_value::<ActivityReceiptRecord>(forged).is_err(),
            "{field}"
        );
    }
    let mut oversized = valid;
    oversized["report"]["error"] = json!("x".repeat(2049));
    assert!(serde_json::from_value::<ActivityReceiptRecord>(oversized).is_err());
    assert!(serde_json::from_value::<ActivityReceipts>(json!({"id":"id","owner_uid":0})).is_err());
}

#[test]
fn operation_preview_requests_are_bounded_and_cannot_supply_authority() {
    let good =
        json!({"app_id":"fs","operation":"write","args":["/home/user/file","--content","draft"]});
    assert!(serde_json::from_value::<OperationPreview>(good.clone()).is_ok());
    for field in ["owner_uid", "grant", "authorized", "execute"] {
        let mut bad = good.clone();
        bad[field] = json!(true);
        assert!(serde_json::from_value::<OperationPreview>(bad).is_err());
    }
    let mut flood = good.clone();
    flood["args"] = json!(vec!["x"; 65]);
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
    assert!(
        serde_json::from_value::<ActivityObjects>(json!({"id":"activity-id","owner_uid":0}))
            .is_err()
    );
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
    assert!(serde_json::from_value::<ActivityRun>(
        json!({"id": "activity-1", "priority": "foreground"}),
    )
    .is_err());
    assert!(serde_json::from_value::<ActivityContinuityExport>(json!({"id":"activity-1"})).is_ok());
    assert!(serde_json::from_value::<ActivityContinuityExport>(
        json!({"id":"activity-1","owner_uid":1000})
    )
    .is_err());
    assert!(serde_json::from_value::<ActivityContinuityImport>(json!({
        "placement":"local",
        "document":"{}"
    }))
    .is_ok());
    for placement in ["remote", "provider", "device-1"] {
        assert!(serde_json::from_value::<ActivityContinuityImport>(json!({
            "placement":placement,
            "document":"{}"
        }))
        .is_err());
    }
    assert!(serde_json::from_value::<ActivityContinuityImport>(json!({
        "placement":"local",
        "document":"{}",
        "owner_uid":1000
    }))
    .is_err());
    assert!(serde_json::from_value::<ActivityContinuityImport>(json!({
        "placement":"local",
        "document":"x".repeat(crate::activities::MAX_CONTINUITY_DOCUMENT_BYTES + 1)
    }))
    .is_err());
}

#[test]
fn activity_linkage_is_additive_to_legacy_task_requests() {
    let legacy: TaskSubmit = serde_json::from_value(json!({"prompt": "hello"})).unwrap();
    assert!(legacy.activity_id.is_none());
    assert!(serde_json::to_value(legacy)
        .unwrap()
        .get("activity_id")
        .is_none());
    let linked: TaskSubmit = serde_json::from_value(json!({
        "prompt": "prepare the release",
        "activity_id": "00000000-0000-4000-8000-000000000001",
    }))
    .unwrap();
    assert!(linked.activity_id.is_some());
    assert!(serde_json::from_value::<TaskList>(json!({"activity_id": "../foreign"})).is_err());
}

#[test]
fn conversation_wire_is_closed_and_bounded() {
    assert!(serde_json::from_value::<AgentConversationCreate>(json!({})).is_ok());
    assert!(serde_json::from_value::<AgentConversationCreate>(
        json!({"title": "Release planning"})
    )
    .is_ok());
    assert!(
        serde_json::from_value::<AgentConversationCreate>(json!({"title": "x".repeat(513)}))
            .is_err()
    );
    assert!(serde_json::from_value::<AgentConversationCreate>(json!({"owner_uid": 1000})).is_err());

    for value in [
        json!({"id": "ses_0000000000001_000000000001"}),
        json!({"id": "078ed458-0e17-882b-b0aa-ca1088683b25", "limit": 100}),
    ] {
        assert!(serde_json::from_value::<AgentConversationGet>(value).is_ok());
    }
    assert!(serde_json::from_value::<AgentConversationGet>(json!({"id": "../foreign"})).is_err());
    assert!(serde_json::from_value::<AgentConversationList>(
        json!({"archived": true, "limit": 50})
    )
    .is_ok());
    assert!(serde_json::from_value::<AgentConversationList>(json!({"deleted": true})).is_err());
    assert!(serde_json::from_value::<AgentConversationUpdate>(
        json!({"id": "ses_0000000000001_000000000001", "archived": true})
    )
    .is_ok());
    assert!(serde_json::from_value::<AgentConversationUpdate>(
        json!({"id": "ses_0000000000001_000000000001", "grant": "forged"})
    )
    .is_err());
    assert!(serde_json::from_value::<AgentConversationFork>(
        json!({"id": "ses_0000000000001_000000000001", "before_user_turn": 2})
    )
    .is_ok());
    assert!(serde_json::from_value::<AgentConversationFork>(
        json!({"id": "ses_0000000000001_000000000001", "copy_jobs": true})
    )
    .is_err());
    assert!(serde_json::from_value::<AgentConversationRevert>(
        json!({"id": "ses_0000000000001_000000000001", "user_turns": 2})
    )
    .is_ok());
    assert!(serde_json::from_value::<AgentConversationRevert>(
        json!({"id": "ses_0000000000001_000000000001", "delete_jobs": true})
    )
    .is_err());
}

#[test]
fn a_task_submission_is_closed_and_bounded() {
    let ok = json!({
        "prompt": "summarise the journal",
        "session_id": "sess-1",
        "max_turns": 4,
        "attachments": [{
            "name": "screen.png",
            "media_type": "image/png",
            "data": "aGVsbG8=",
        }],
    });
    assert!(serde_json::from_value::<TaskSubmit>(ok).is_ok());

    // Identity, source, and presence are authority the daemon derives; a
    // caller cannot smuggle any of them in beside a legitimate field.
    for field in [
        json!({"prompt": "hi", "owner_uid": 0}),
        json!({"prompt": "hi", "source": "local-cli"}),
        json!({"prompt": "hi", "attended": true}),
        json!({"prompt": "hi", "local": true}),
        json!({"prompt": "hi", "priority": "foreground"}),
        json!({"prompt": "hi", "scheduling_priority": "background"}),
    ] {
        assert!(serde_json::from_value::<TaskSubmit>(field).is_err());
    }

    let wrong_type = json!({"prompt": ["hi"]});
    assert!(serde_json::from_value::<TaskSubmit>(wrong_type).is_err());

    let oversized = json!({"prompt": "x".repeat(PROMPT_BYTES + 1)});
    assert!(serde_json::from_value::<TaskSubmit>(oversized).is_err());
    assert!(serde_json::from_value::<TaskSubmit>(json!({
        "prompt": "hi",
        "attachments": (0..=crate::agent::attachments::MAX_ATTACHMENTS)
            .map(|index| json!({
                "name": format!("{index}.png"),
                "media_type": "image/png",
                "data": "aGVsbG8=",
            }))
            .collect::<Vec<_>>(),
    }))
    .is_err());
    assert!(serde_json::from_value::<TaskSubmit>(json!({
        "prompt": "hi",
        "attachments": [{
            "name": "screen.png",
            "media_type": "image/png",
            "data": "x".repeat(crate::agent::attachments::MAX_BASE64_BYTES + 1),
        }],
    }))
    .is_err());
    assert!(serde_json::from_value::<TaskSubmit>(json!({
        "prompt": "hi",
        "attachments": [{
            "name": "screen.png",
            "media_type": "image/png",
            "data": "aGVsbG8=",
            "bytes": 5,
        }],
    }))
    .is_err());
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
