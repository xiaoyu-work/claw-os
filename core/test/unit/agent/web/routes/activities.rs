use super::*;

fn continuity_document() -> ActivityContinuityDocument {
    ActivityContinuityDocument::build(
        ActivityContinuityLineage {
            id: "00000000-0000-4000-8000-000000000123".into(),
            revision: 9_007_199_254_740_993,
        },
        PortableActivityIntent {
            title: "Release".into(),
            goal: "Publish the release".into(),
            completion_criteria: "Reviewed and available".into(),
            boundaries: "Ask before publishing".into(),
        },
        vec![PortableActivityReference {
            label: "Status".into(),
            reference: "app://kv/entry?id=release.status&revision=v1".into(),
        }],
        PortableActivityRules {
            execution_limits: Some(PortableExecutionLimits {
                enabled: false,
                max_attempts: 10,
                max_turns_per_attempt: 5,
                expires_at: "2030-01-01T00:00:00.000000000Z".into(),
            }),
            scheduling: Some(PortableSchedulingPreference {
                priority: ActivitySchedulingPriority::Foreground,
            }),
        },
    )
    .unwrap()
}

#[test]
fn activity_url_and_shared_request_contract_are_preserved() {
    let update = with_id::<ActivityUpdate>(
        "activity-1".into(),
        json!({
            "title": "Plan a release",
            "boundaries": "",
            "resources": [{ "label": "Notes", "reference": "/home/user/release.txt" }],
        }),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(update).unwrap(),
        json!({
            "id": "activity-1",
            "title": "Plan a release",
            "boundaries": "",
            "resources": [{ "label": "Notes", "reference": "/home/user/release.txt" }],
        })
    );
    let run = with_id::<ActivityRun>(
        "activity-1".into(),
        json!({ "session_id": "session-1", "prompt": "Continue", "use_memory": false }),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(run).unwrap(),
        json!({
            "id": "activity-1",
            "session_id": "session-1",
            "prompt": "Continue",
            "use_memory": false,
        })
    );
    let attention = with_id::<ActivityGet>("activity-1".into(), json!({ "limit": 12 })).unwrap();
    assert_eq!(
        serde_json::to_value(attention).unwrap(),
        json!({ "id": "activity-1", "limit": 12 })
    );
    assert!(with_id::<ActivityGet>("activity-1".into(), json!({ "owner_uid": 0 })).is_err());
}

#[test]
fn activity_requests_reject_identity_overrides_and_unbounded_fields() {
    for body in [
        json!({ "id": "other" }),
        json!({ "owner_uid": 0 }),
        json!({ "title": "x".repeat(241) }),
        json!({ "resources": [{ "label": "Ref", "reference": "file", "execute": true }] }),
        json!({ "resources": vec![json!({ "label": "Ref", "reference": "file" }); 33] }),
        json!([]),
    ] {
        assert!(with_id::<ActivityUpdate>("activity-1".into(), body).is_err());
    }
    assert!(with_id::<ActivityRun>("../other".into(), json!({})).is_err());
    assert!(serde_json::from_value::<ActivityCreate>(
        json!({ "title": "Goal", "goal": "Result", "owner_uid": 0 })
    )
    .is_err());
    assert!(serde_json::from_value::<ActivityList>(json!({ "state": "running" })).is_err());
}

#[test]
fn activity_completion_confirmation_is_forwarded_not_inferred() {
    let body = with_id::<ActivityTransition>(
        "activity-1".into(),
        json!({ "state": "completed", "completion_note": "I verified the deliverable." }),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(body).unwrap(),
        json!({
            "id": "activity-1",
            "state": "completed",
            "completion_note": "I verified the deliverable.",
        })
    );
    let missing_note =
        with_id::<ActivityTransition>("activity-1".into(), json!({ "state": "completed" }))
            .unwrap();
    assert!(serde_json::to_value(missing_note)
        .unwrap()
        .get("completion_note")
        .is_none());
}

#[test]
fn activity_object_requests_preserve_typed_opaque_ids_and_reject_overrides() {
    let object = json!({
        "app_id": "archive", "object_type": "entry",
        "object_id": " release.status /?#& ", "revision": "rev 2",
    });
    let body = json!({ "label": "Release status", "object": object });
    let request = with_id::<ActivityObjectAttach>("activity-1".into(), body.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({ "id": "activity-1", "label": "Release status", "object": object })
    );
    for (field, value) in [
        ("owner_uid", json!(0)),
        ("id", json!("another")),
        ("reference", json!("app://unverified/entry?id=x")),
        ("resources", json!([])),
        ("label", json!("x".repeat(241))),
        (
            "object",
            json!({ "app_id": "archive", "object_type": "entry", "object_id": "x".repeat(1025) }),
        ),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(with_id::<ActivityObjectAttach>("activity-1".into(), invalid).is_err());
    }
    let mut nested_owner = body.clone();
    nested_owner["object"]["owner_uid"] = json!(0);
    // SDK value types may discard unknown fields, but cannot forward identity.
    if let Ok(request) = with_id::<ActivityObjectAttach>("activity-1".into(), nested_owner) {
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({ "id": "activity-1", "label": "Release status", "object": object })
        );
    }
    let objects = with_id::<ActivityObjects>("activity-1".into(), json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(objects).unwrap(),
        json!({ "id": "activity-1" })
    );
}

#[tokio::test]
async fn activity_http_routes_require_authentication() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    let routes = [
        ("GET", "/api/activities"),
        ("POST", "/api/activities"),
        ("GET", "/api/activities/activity-1"),
        ("GET", "/api/activities/activity-1/attention"),
        ("POST", "/api/activities/activity-1/update"),
        ("POST", "/api/activities/activity-1/transition"),
        ("POST", "/api/activities/activity-1/run"),
        ("GET", "/api/activities/activity-1/objects"),
        ("POST", "/api/activities/activity-1/objects"),
        ("POST", "/api/activities/activity-1/operation-preview"),
        ("GET", "/api/activities/activity-1/receipts"),
        ("GET", "/api/activities/activity-1/object-state"),
        ("POST", "/api/activities/activity-1/object-state"),
        ("GET", "/api/activities/activity-1/execution-limits"),
        ("POST", "/api/activities/activity-1/execution-limits"),
        (
            "POST",
            "/api/activities/activity-1/execution-limits/enabled",
        ),
        ("GET", "/api/activities/activity-1/monetary-budget"),
        ("POST", "/api/activities/activity-1/monetary-budget"),
        ("POST", "/api/activities/activity-1/monetary-budget/enabled"),
        ("GET", "/api/activities/activity-1/scheduling-priority"),
        ("POST", "/api/activities/activity-1/scheduling-priority"),
        ("GET", "/api/activities/activity-1/continuity/export"),
        ("POST", "/api/activities/continuity/import"),
        ("GET", "/api/activities/capability-policy-catalog"),
        ("GET", "/api/activities/activity-1/capability-policy"),
        ("POST", "/api/activities/activity-1/capability-policy"),
        (
            "POST",
            "/api/activities/activity-1/capability-policy/enabled",
        ),
    ];
    for local_only in [true, false] {
        let state = crate::agent::web::state::AppState::new_with_locality(
            crate::config::AgentConfig::default(),
            1000,
            local_only,
        );
        for (method, path) in routes {
            let response = crate::agent::web::server::build_app(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}; local_only={local_only}"
            );
        }
    }
}

#[test]
fn activity_continuity_http_dto_is_closed_bounded_and_precision_safe() {
    let document = continuity_document();
    let projected = ContinuityDocumentHttp::from_core(document.clone()).unwrap();
    assert_eq!(projected.lineage.revision, "9007199254740993");
    assert_eq!(projected.clone().into_core().unwrap(), document);

    let mut value = serde_json::to_value(projected.clone()).unwrap();
    value["owner_uid"] = json!(1000);
    assert!(serde_json::from_value::<ContinuityDocumentHttp>(value).is_err());
    let mut value = serde_json::to_value(projected.clone()).unwrap();
    value["rules"]["authority"] = json!({"grant":"all"});
    assert!(serde_json::from_value::<ContinuityDocumentHttp>(value).is_err());
    let mut value = serde_json::to_value(projected.clone()).unwrap();
    value["lineage"]["revision"] = json!(9_007_199_254_740_993_u64);
    assert!(serde_json::from_value::<ContinuityDocumentHttp>(value).is_err());
    assert!(serde_json::from_value::<ContinuityImportHttp>(json!({
        "placement": "local",
        "document": projected,
        "server_path": "/home/user/activity.json",
    }))
    .is_err());
    assert!(serde_json::from_value::<ContinuityImportHttp>(json!({
        "placement": "remote",
        "document": ContinuityDocumentHttp::from_core(continuity_document()).unwrap(),
    }))
    .is_err());
}

#[test]
fn activity_continuity_import_ack_is_exact_paused_local_and_owner_scoped() {
    let document = continuity_document();
    let activity = Activity {
        id: "00000000-0000-4000-8000-000000000999".into(),
        owner_uid: 1000,
        title: document.intent.title.clone(),
        goal: document.intent.goal.clone(),
        completion_criteria: document.intent.completion_criteria.clone(),
        boundaries: document.intent.boundaries.clone(),
        resources: document
            .references
            .iter()
            .map(|reference| ActivityResource {
                label: reference.label.clone(),
                reference: reference.reference.clone(),
            })
            .collect(),
        state: ActivityState::Paused,
        completion_note: None,
        created_at: "2026-09-14T00:00:00Z".into(),
        updated_at: "2026-09-14T00:00:00Z".into(),
    };
    let imported = ActivityContinuityImport {
        activity: activity.clone(),
        continuity_id: document.lineage.id.clone(),
        continuity_revision: document.lineage.revision,
        placement: ActivityExecutionPlacement::Local,
    };
    let view = validate_import_acknowledgement(
        imported.clone(),
        &document,
        ActivityExecutionPlacement::Local,
        1000,
    )
    .unwrap();
    assert_eq!(view.continuity_revision, "9007199254740993");
    for invalid in [
        ActivityContinuityImport {
            continuity_id: "00000000-0000-4000-8000-000000000124".into(),
            ..imported.clone()
        },
        ActivityContinuityImport {
            continuity_revision: document.lineage.revision + 1,
            ..imported.clone()
        },
        ActivityContinuityImport {
            activity: Activity {
                state: ActivityState::Active,
                ..activity.clone()
            },
            ..imported.clone()
        },
        ActivityContinuityImport {
            activity: Activity {
                owner_uid: 1001,
                ..activity.clone()
            },
            ..imported.clone()
        },
        ActivityContinuityImport {
            activity: Activity {
                goal: "Changed acknowledgement".into(),
                ..activity.clone()
            },
            ..imported.clone()
        },
    ] {
        assert!(validate_import_acknowledgement(
            invalid,
            &document,
            ActivityExecutionPlacement::Local,
            1000,
        )
        .is_err());
    }
}

#[test]
fn activity_monetary_budget_http_dto_is_closed_bounded_and_precision_safe() {
    let body: MonetaryBudgetHttpSet = serde_json::from_value(json!({
        "expected_revision": "9007199254740993",
        "budget": {
            "currency": "USD",
            "max_total_microusd": "5000000",
            "input_microusd_per_million_tokens": "250000",
            "output_microusd_per_million_tokens": "1000000",
            "max_output_tokens_per_turn": 4096,
        }
    }))
    .unwrap();
    assert_eq!(
        decimal_revision(
            "expected_revision",
            body.expected_revision.as_deref().unwrap()
        )
        .unwrap(),
        9_007_199_254_740_993
    );
    let budget = body.budget.into_core().unwrap();
    assert_eq!(budget.currency, "USD");
    assert_eq!(budget.max_total_microusd, 5_000_000);
    assert_eq!(budget.max_output_tokens_per_turn, 4096);

    for value in [
        json!({"budget": {
            "currency": "USD", "max_total_microusd": "1",
            "input_microusd_per_million_tokens": "1",
            "output_microusd_per_million_tokens": "1",
            "max_output_tokens_per_turn": 1,
        }}),
        json!({"expected_revision": null, "owner_uid": 0, "budget": {
            "currency": "USD", "max_total_microusd": "1",
            "input_microusd_per_million_tokens": "1",
            "output_microusd_per_million_tokens": "1",
            "max_output_tokens_per_turn": 1,
        }}),
        json!({"expected_revision": null, "budget": {
            "currency": "EUR", "max_total_microusd": "1",
            "input_microusd_per_million_tokens": "1",
            "output_microusd_per_million_tokens": "1",
            "max_output_tokens_per_turn": 1,
        }}),
        json!({"expected_revision": null, "budget": {
            "currency": "USD", "max_total_microusd": 1,
            "input_microusd_per_million_tokens": "1",
            "output_microusd_per_million_tokens": "1",
            "max_output_tokens_per_turn": 1,
        }}),
    ] {
        match serde_json::from_value::<MonetaryBudgetHttpSet>(value) {
            Ok(body) => assert!(body.budget.into_core().is_err()),
            Err(_) => {}
        }
    }
    assert!(serde_json::from_value::<MonetaryBudgetHttpEnabled>(json!({
        "expected_revision": "7", "enabled": false, "spent_microusd": "0"
    }))
    .is_err());
}

#[test]
fn activity_scheduling_priority_http_dto_is_closed_and_precision_safe() {
    let body: SchedulingPriorityHttpSet = serde_json::from_value(json!({
        "expected_revision": "9007199254740993",
        "priority": "background",
    }))
    .unwrap();
    assert_eq!(
        decimal_revision(
            "expected_revision",
            body.expected_revision.as_deref().unwrap()
        )
        .unwrap(),
        9_007_199_254_740_993
    );
    assert_eq!(body.priority, ActivitySchedulingPriority::Background);

    for value in [
        json!({"priority": "standard"}),
        json!({"expected_revision": null, "priority": "urgent"}),
        json!({"expected_revision": 7, "priority": "foreground"}),
        json!({"expected_revision": "7", "priority": "foreground", "owner_uid": 0}),
        json!({"expected_revision": "7", "priority": "foreground", "job_id": "job-1"}),
        json!({"expected_revision": "7", "priority": "foreground", "preempt": true}),
    ] {
        assert!(
            serde_json::from_value::<SchedulingPriorityHttpSet>(value.clone()).is_err(),
            "{value}"
        );
    }
}

#[test]
fn activity_scheduling_priority_validates_owner_identity_revision_and_timestamps() {
    let policy = ActivitySchedulingPolicy {
        activity_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
        owner_uid: 1000,
        revision: u64::MAX,
        priority: ActivitySchedulingPriority::Foreground,
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    };
    let view =
        validate_scheduling_policy(policy.clone(), "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE", 1000)
            .unwrap();
    assert_eq!(view.revision, u64::MAX.to_string());
    assert_eq!(view.priority, ActivitySchedulingPriority::Foreground);

    for invalid in [
        ActivitySchedulingPolicy {
            owner_uid: 1001,
            ..policy.clone()
        },
        ActivitySchedulingPolicy {
            revision: 0,
            ..policy.clone()
        },
        ActivitySchedulingPolicy {
            created_at: "not-a-time".into(),
            ..policy.clone()
        },
        ActivitySchedulingPolicy {
            activity_id: "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff".into(),
            ..policy
        },
    ] {
        assert!(
            validate_scheduling_policy(invalid, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee", 1000,)
                .is_err()
        );
    }
}

#[test]
fn activity_monetary_budget_http_projection_preserves_u64_amounts_as_decimal_strings() {
    let policy = ActivityMonetaryBudget {
        activity_id: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
        owner_uid: 1000,
        revision: 9_007_199_254_740_993,
        enabled: false,
        spent_microusd: 9_007_199_254_740_993,
        reserved_microusd: u64::MAX,
        budget: MonetaryBudgetDraft {
            currency: "USD".into(),
            max_total_microusd: 1_000_000_000_000,
            input_microusd_per_million_tokens: 1,
            output_microusd_per_million_tokens: 1_000_000_000_000,
            max_output_tokens_per_turn: 1_000_000,
        },
        created_at: "2026-09-13T00:00:00Z".into(),
        updated_at: "2026-09-13T01:00:00Z".into(),
    };
    let projected =
        validate_monetary_policy(policy, "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE", 1000).unwrap();
    let value = serde_json::to_value(projected).unwrap();
    assert_eq!(value["revision"], "9007199254740993");
    assert_eq!(value["spent_microusd"], "9007199254740993");
    assert_eq!(value["reserved_microusd"], u64::MAX.to_string());
    assert_eq!(value["budget"]["currency"], "USD");
    assert_eq!(value["budget"]["max_total_microusd"], "1000000000000");
    assert_eq!(value.as_object().unwrap().len(), 9);
}

#[test]
fn activity_capability_policy_requests_preserve_cas_and_reject_authority_fields() {
    let policy = json!({"rules": [{"verb": "fs.delete", "mode": "deny", "scopes": []}]});
    let body = json!({"expected_revision": 2, "policy": policy});
    let request =
        with_id::<ActivityCapabilityPolicySet>("activity-1".into(), body.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"id": "activity-1", "expected_revision": 2, "policy": policy})
    );
    let enabled = with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(),
        json!({"expected_revision": 2, "enabled": false}),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(enabled).unwrap(),
        json!({"id": "activity-1", "expected_revision": 2, "enabled": false})
    );
    for (field, value) in [
        ("id", json!("another")),
        ("owner_uid", json!(0)),
        ("enabled", json!(true)),
        ("revision", json!(2)),
        ("grant", json!("forged")),
        ("expected_revision", json!(-1)),
        ("policy", json!({"rules": [], "enabled": false})),
        (
            "policy",
            json!({"rules": [{
                "verb": "fs.read", "mode": "normal", "scopes": [{"kind": "wild"}],
            }]}),
        ),
        (
            "policy",
            json!({"rules": [{
                "verb": "fs.read", "mode": "normal",
                "scopes": [{"kind": "path", "value": "x".repeat(16 * 1024)}],
            }]}),
        ),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(with_id::<ActivityCapabilityPolicySet>("activity-1".into(), invalid).is_err());
    }
    assert!(with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(),
        json!({"enabled": false}),
    )
    .is_err());
    assert!(with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(),
        json!({"expected_revision": 1, "enabled": "false"}),
    )
    .is_err());
}

#[tokio::test]
async fn capability_policy_catalog_is_static_core_metadata_not_app_or_owner_data() {
    let Json(catalog) = capability_policy_catalog(Ok(Query(NoBody::default())))
        .await
        .unwrap();
    assert_eq!(catalog["schema"], 1);
    let verbs = catalog["verbs"].as_array().unwrap();
    assert_eq!(verbs.len(), crate::caps::CATALOG.len());
    for (value, entry) in verbs.iter().zip(crate::caps::CATALOG) {
        assert_eq!(
            value,
            &json!({
                "verb": entry.verb, "scope_kind": entry.scope_kind,
                "label": entry.label.current(), "description": entry.blurb.current(),
            })
        );
    }
    assert!(catalog.get("owner_uid").is_none());
    assert!(catalog.get("capabilities").is_none());
}

#[test]
fn activity_receipt_queries_preserve_limits_and_reject_owner_or_source_overrides() {
    let request = with_id::<ActivityReceipts>("activity-1".into(), json!({ "limit": 50 })).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({ "id": "activity-1", "limit": 50 })
    );
    for body in [
        json!({ "id": "another" }),
        json!({ "owner_uid": 0 }),
        json!({ "source": "os_confirmed" }),
        json!({ "report": {} }),
        json!({ "limit": -1 }),
        json!({ "limit": "all" }),
    ] {
        assert!(with_id::<ActivityReceipts>("activity-1".into(), body).is_err());
    }
    assert!(with_id::<ActivityReceipts>("../another".into(), json!({})).is_err());
}

#[test]
fn object_state_http_uses_the_broker_draft_without_origin_or_identity_overrides() {
    let draft = json!({
        "id":"00000000-0000-4000-8000-000000000002",
        "reference":"app://demo/entry?id=release",
        "content":{"kind":"agent_inference","text":"Review might be needed"},
    });
    let body = json!({"entry":draft});
    let request = with_id::<ActivityObjectStateRecord>("activity-1".into(), body.clone()).unwrap();
    let request = serde_json::to_value(request).unwrap();
    assert_eq!(request["id"], "activity-1");
    assert_eq!(request["entry"]["id"], draft["id"]);
    assert_eq!(request["entry"]["content"], draft["content"]);
    for field in ["id", "owner_uid", "source", "verified"] {
        let mut invalid = body.clone();
        invalid[field] = json!("forged");
        assert!(with_id::<ActivityObjectStateRecord>("activity-1".into(), invalid).is_err());
    }
    let mut forged = body;
    forged["entry"]["source"] = json!("os_confirmed");
    assert!(with_id::<ActivityObjectStateRecord>("activity-1".into(), forged).is_err());
    let query = with_id::<ActivityObjectState>(
        "activity-1".into(),
        json!({"reference":"app://demo/entry?id=release","limit":20}),
    )
    .unwrap();
    assert_eq!(serde_json::to_value(query).unwrap()["limit"], 20);
    assert!(serde_json::from_value::<ObjectStateQuery>(json!({"owner_uid":0})).is_err());
}

#[tokio::test]
async fn activity_receipts_http_surface_has_no_authoring_methods() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get as http_get;
    use axum::Router;
    use tower::ServiceExt;

    let router = Router::new().route("/activities/{id}/receipts", http_get(receipts));
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/activities/activity-1/receipts")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

#[test]
fn activity_operation_preview_preserves_argv_and_rejects_identity_or_effect_claims() {
    let body = json!({
        "app_id": "archive", "operation": "get",
        "args": ["--message=$(printf inert); <script>", "--", "../requested/draft.txt"],
    });
    let request = with_id::<ActivityOperationPreview>("activity-1".into(), body.clone()).unwrap();
    let mut expected = body.clone();
    expected["id"] = json!("activity-1");
    assert_eq!(serde_json::to_value(request).unwrap(), expected);
    for (field, value) in [
        ("id", json!("another")),
        ("owner_uid", json!(0)),
        ("executed", json!(true)),
        ("effects", json!([])),
        ("args", json!("joined shell command")),
        ("args", json!(vec!["x"; 65])),
        ("args", json!(["x".repeat(8193)])),
        ("args", json!([42])),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(with_id::<ActivityOperationPreview>("activity-1".into(), invalid).is_err());
    }
    assert!(with_id::<ActivityOperationPreview>("../other".into(), body).is_err());
}

#[tokio::test]
async fn activity_run_and_read_errors_use_the_shared_broker_without_fallback() {
    use crate::clawd::protocol::{Request, Response};
    use crate::clawd::transport::frame;
    use crate::clawd::wire::{HEADER_BYTES, KIND_REQUEST, KIND_RESPONSE, MAX_REQUEST_BYTES};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    let _lock = crate::test_env::lock_env();
    let directory = tempfile::tempdir().expect("create socket fixture on the native filesystem");
    let current_directory = std::env::current_dir().expect("resolve test working directory");
    let runtime_directory = directory
        .path()
        .strip_prefix(&current_directory)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|_| directory.path().to_path_buf());
    let socket = runtime_directory.join("clawd.sock");
    let _runtime = RuntimeDirectory {
        previous: std::env::var_os("COS_RUNTIME_DIR"),
        directory,
    };
    std::env::set_var("COS_RUNTIME_DIR", runtime_directory);
    let listener = UnixListener::bind(socket).unwrap();
    let job = json!({
        "id": "job-1", "status": "pending",
        "activity_id": "activity-1", "session_id": "session-1",
    });
    let submitted = job.clone();
    let object_view = json!({
        "schema": 1, "activity_id": "activity-1",
        "objects": [{
            "label": "Release status", "reference": "app://archive/entry?id=release.status",
            "status": "unavailable", "description": null, "error": "App package is quarantined",
        }],
    });
    let object_projection = object_view.clone();
    let attached_activity = json!({
        "id": "activity-1", "owner_uid": 1000, "title": "Release", "goal": "Prepare",
        "completion_criteria": "", "boundaries": "", "state": "active", "completion_note": null,
        "created_at": "2026-09-10T12:00:00Z", "updated_at": "2026-09-10T13:00:00Z",
        "resources": [{ "label": "Release status", "reference": "app://archive/entry?id=release.status" }],
    });
    let attachment_result = attached_activity.clone();
    let preview = json!({
        "schema": 1, "app_id": "archive", "app_name": "Archive", "app_version": "1.0",
        "package_digest": "fixture", "operation": "get", "operation_label": "Get entry",
        "effects_declared": false, "effects": [], "unresolved_arguments": ["provider"],
        "authorization_checked": false, "executed": false, "effects_confirmed": false,
        "notes": ["Effects are unknown, not implicitly read-only."],
    });
    let preview_result = preview.clone();
    let receipt_view = json!({
        "schema": 1, "activity_id": "activity-1",
        "receipts": [{
            "id": "receipt-1", "activity_id": "activity-1", "owner_uid": 1000,
            "received_at": "2026-09-10T12:00:00Z", "source": "caller_reported",
            "report": {
                "id": "report-1", "app_id": "archive", "operation": "write",
                "package_digest": "a".repeat(64), "outcome": "indeterminate",
                "result": null, "error": "Result could not be captured",
            },
            "declaration": null, "declaration_error": "Matching App package unavailable",
        }],
    });
    let receipt_result = receipt_view.clone();
    let policy_draft = json!({"rules": [{"verb": "fs.delete", "mode": "deny", "scopes": []}]});
    let policy_result = json!({
        "activity_id": "activity-1", "owner_uid": 1000, "revision": 1, "enabled": true,
        "rules": policy_draft["rules"], "created_at": "2026-09-11T00:00:00Z",
        "updated_at": "2026-09-11T00:00:00Z",
    });
    let policy_response = policy_result.clone();
    let forwarded_draft = policy_draft.clone();
    let continuity = continuity_document();
    let continuity_response = serde_json::to_value(&continuity).unwrap();
    let imported_activity = Activity {
        id: "00000000-0000-4000-8000-000000000999".into(),
        owner_uid: 1000,
        title: continuity.intent.title.clone(),
        goal: continuity.intent.goal.clone(),
        completion_criteria: continuity.intent.completion_criteria.clone(),
        boundaries: continuity.intent.boundaries.clone(),
        resources: continuity
            .references
            .iter()
            .map(|reference| ActivityResource {
                label: reference.label.clone(),
                reference: reference.reference.clone(),
            })
            .collect(),
        state: ActivityState::Paused,
        completion_note: None,
        created_at: "2026-09-14T00:00:00Z".into(),
        updated_at: "2026-09-14T00:00:00Z".into(),
    };
    let import_response = serde_json::to_value(ActivityContinuityImport {
        activity: imported_activity.clone(),
        continuity_id: continuity.lineage.id.clone(),
        continuity_revision: continuity.lineage.revision,
        placement: ActivityExecutionPlacement::Local,
    })
    .unwrap();
    let forwarded_continuity = continuity.clone();
    let broker = tokio::spawn(async move {
        let mut preview_count = 0;
        let mut receipt_count = 0;
        let mut policy_reads = 0;
        let mut policy_writes = 0;
        for command in [
            Command::ActivityRun,
            Command::ActivityGet,
            Command::ActivityObjects,
            Command::ActivityObjectAttach,
            Command::ActivityOperationPreview,
            Command::ActivityOperationPreview,
            Command::ActivityReceipts,
            Command::ActivityReceipts,
            Command::ActivityCapabilityPolicyGet,
            Command::ActivityCapabilityPolicySet,
            Command::ActivityCapabilityPolicyEnabled,
            Command::ActivityCapabilityPolicySet,
            Command::ActivityCapabilityPolicyGet,
            Command::ActivityContinuityExport,
            Command::ActivityContinuityImport,
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = [0; HEADER_BYTES];
            socket.read_exact(&mut header).await.unwrap();
            let size = frame::parse_header(&header, KIND_REQUEST, MAX_REQUEST_BYTES).unwrap();
            let mut body = vec![0; size];
            socket.read_exact(&mut body).await.unwrap();
            let request: Request = serde_json::from_slice(&body).unwrap();
            assert_eq!(request.command, command);
            if command == Command::ActivityContinuityImport {
                assert!(request.params.get("id").is_none());
            } else {
                assert_eq!(request.params["id"], "activity-1");
            }
            assert!(request.params.get("owner_uid").is_none());
            let response = match command {
                Command::ActivityRun => {
                    assert_eq!(request.params["prompt"], "Continue the saved goal");
                    assert_eq!(request.params["session_id"], "session-1");
                    Response::ok(request.id, submitted.clone())
                }
                Command::ActivityGet => {
                    assert_eq!(request.params["limit"], 20);
                    Response::error(request.id, "unavailable", "Activity store unavailable")
                }
                Command::ActivityObjects => {
                    assert_eq!(request.params, json!({ "id": "activity-1" }));
                    Response::ok(request.id, object_projection.clone())
                }
                Command::ActivityObjectAttach => {
                    assert_eq!(
                        request.params,
                        json!({
                            "id": "activity-1", "label": "Release status",
                            "object": { "app_id": "archive", "object_type": "entry", "object_id": "release.status" },
                        })
                    );
                    Response::ok(request.id, attachment_result.clone())
                }
                Command::ActivityOperationPreview => {
                    assert_eq!(
                        request.params,
                        json!({
                            "id": "activity-1", "app_id": "archive", "operation": "get",
                            "args": ["--message=$(printf inert); <script>"],
                        })
                    );
                    preview_count += 1;
                    if preview_count == 1 {
                        Response::ok(request.id, preview_result.clone())
                    } else {
                        Response::error(request.id, "unavailable", "App signature unavailable")
                    }
                }
                Command::ActivityReceipts => {
                    assert_eq!(request.params, json!({ "id": "activity-1", "limit": 50 }));
                    receipt_count += 1;
                    if receipt_count == 1 {
                        Response::ok(request.id, receipt_result.clone())
                    } else {
                        Response::error(request.id, "unavailable", "Receipt ledger unavailable")
                    }
                }
                Command::ActivityCapabilityPolicyGet => {
                    assert_eq!(request.params, json!({"id": "activity-1"}));
                    policy_reads += 1;
                    if policy_reads == 1 {
                        Response::ok(
                            request.id,
                            json!({
                                "schema": 1, "activity_id": "activity-1", "capability_policy": null,
                            }),
                        )
                    } else {
                        Response::error(request.id, "unavailable", "Capability policy unavailable")
                    }
                }
                Command::ActivityCapabilityPolicySet => {
                    policy_writes += 1;
                    if policy_writes == 1 {
                        assert_eq!(
                            request.params,
                            json!({
                                "id": "activity-1", "policy": forwarded_draft,
                            })
                        );
                        Response::ok(request.id, policy_response.clone())
                    } else {
                        assert_eq!(
                            request.params,
                            json!({
                                "id": "activity-1", "expected_revision": 1, "policy": forwarded_draft,
                            })
                        );
                        Response::error(
                            request.id,
                            "execution_failed",
                            "Capability policy revision conflict",
                        )
                    }
                }
                Command::ActivityCapabilityPolicyEnabled => {
                    assert_eq!(
                        request.params,
                        json!({
                            "id": "activity-1", "expected_revision": 1, "enabled": false,
                        })
                    );
                    let mut disabled = policy_response.clone();
                    disabled["revision"] = json!(2);
                    disabled["enabled"] = json!(false);
                    Response::ok(request.id, disabled)
                }
                Command::ActivityContinuityExport => {
                    assert_eq!(request.params, json!({"id":"activity-1"}));
                    Response::ok(request.id, continuity_response.clone())
                }
                Command::ActivityContinuityImport => {
                    assert_eq!(request.params["placement"], "local");
                    assert!(request.params.get("owner_uid").is_none());
                    assert!(request.params.get("server_path").is_none());
                    let document = ActivityContinuityDocument::from_json(
                        request.params["document"].as_str().unwrap().as_bytes(),
                    )
                    .unwrap();
                    assert_eq!(document, forwarded_continuity);
                    Response::ok(request.id, import_response.clone())
                }
                _ => unreachable!(),
            };
            socket
                .write_all(&frame::encode_frame(
                    KIND_RESPONSE,
                    &serde_json::to_vec(&response).unwrap(),
                ))
                .await
                .unwrap();
        }
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run(
            Path("activity-1".into()),
            Ok(Json(
                json!({ "prompt": "Continue the saved goal", "session_id": "session-1" }),
            )),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.0, job);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        get(
            Path("activity-1".into()),
            Ok(Query(DetailQuery { limit: Some(20) })),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    let (status, Json(body)) = error;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "Activity store unavailable");
    let described = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        objects(Path("activity-1".into()), Ok(Query(NoBody::default()))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(described.0, object_view);
    let attached = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        attach_object(
            Path("activity-1".into()),
            Ok(Json(json!({
                "label": "Release status",
                "object": { "app_id": "archive", "object_type": "entry", "object_id": "release.status" },
            }))),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(attached.0, attached_activity);
    for attempt in 0..2 {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            operation_preview(
                Path("activity-1".into()),
                Ok(Json(json!({
                    "app_id": "archive", "operation": "get",
                    "args": ["--message=$(printf inert); <script>"],
                }))),
            ),
        )
        .await
        .unwrap();
        if attempt == 0 {
            assert_eq!(result.unwrap().0, preview);
        } else {
            let (status, Json(body)) = result.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"], "App signature unavailable");
        }
    }
    for attempt in 0..2 {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            receipts(
                Path("activity-1".into()),
                Ok(Query(DetailQuery { limit: Some(50) })),
            ),
        )
        .await
        .unwrap();
        if attempt == 0 {
            assert_eq!(result.unwrap().0, receipt_view);
        } else {
            let (status, Json(body)) = result.unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"], "Receipt ledger unavailable");
        }
    }
    let Json(absent) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        capability_policy(Path("activity-1".into()), Ok(Query(NoBody::default()))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        absent,
        json!({
            "schema": 1, "activity_id": "activity-1", "capability_policy": null,
        })
    );
    let Json(saved) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        set_capability_policy(
            Path("activity-1".into()),
            Ok(Json(json!({
                "expected_revision": null, "policy": policy_draft,
            }))),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved, policy_result);
    let Json(disabled) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        enable_capability_policy(
            Path("activity-1".into()),
            Ok(Json(json!({
                "expected_revision": 1, "enabled": false,
            }))),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(disabled["revision"], 2);
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["rules"], policy_result["rules"]);
    let (status, Json(error)) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        set_capability_policy(
            Path("activity-1".into()),
            Ok(Json(json!({
                "expected_revision": 1, "policy": policy_draft,
            }))),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "Capability policy revision conflict");
    let (status, Json(error)) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        capability_policy(Path("activity-1".into()), Ok(Query(NoBody::default()))),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "Capability policy unavailable");
    let Json(exported) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        export_continuity(Path("activity-1".into()), Ok(Query(NoBody::default()))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(exported.lineage.revision, "9007199254740993");
    let Json(imported) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        import_continuity(
            Extension(AuthenticatedToken {
                uid: 1000,
                token_id: "test-token".into(),
                expires_at: u64::MAX,
            }),
            Ok(Json(ContinuityImportHttp {
                placement: ActivityExecutionPlacement::Local,
                document: ContinuityDocumentHttp::from_core(continuity).unwrap(),
            })),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(imported.activity, imported_activity);
    assert_eq!(imported.continuity_revision, "9007199254740993");
    assert_eq!(imported.placement, ActivityExecutionPlacement::Local);
    broker.await.unwrap();
    let unavailable = list(Ok(Query(ActivityList {
        state: None,
        limit: None,
    })))
    .await
    .unwrap_err();
    assert_eq!(unavailable.0, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn activity_decode_failures_are_readable_json_errors_before_broker_access() {
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use axum::routing::{get as http_get, post};
    use axum::Router;
    use tower::ServiceExt;

    let router = Router::new()
        .route("/activities", http_get(list).post(create))
        .route(
            "/activities/{id}/objects",
            http_get(objects).post(attach_object),
        )
        .route(
            "/activities/{id}/operation-preview",
            post(operation_preview),
        )
        .route("/activities/{id}/receipts", http_get(receipts))
        .route(
            "/activities/{id}/continuity/export",
            http_get(export_continuity),
        )
        .route("/activities/continuity/import", post(import_continuity))
        .route(
            "/activities/capability-policy-catalog",
            http_get(capability_policy_catalog),
        )
        .route(
            "/activities/{id}/capability-policy",
            http_get(capability_policy).post(set_capability_policy),
        )
        .route(
            "/activities/{id}/capability-policy/enabled",
            post(enable_capability_policy),
        )
        .route("/activities/{id}/update", post(update))
        .layer(Extension(AuthenticatedToken {
            uid: 1000,
            token_id: "decode-test".into(),
            expires_at: u64::MAX,
        }));
    for (method, path, body) in [
        (
            "POST",
            "/activities",
            r#"{"title":"Goal","goal":"Result","owner_uid":0}"#,
        ),
        ("GET", "/activities?owner_uid=0", ""),
        ("GET", "/activities?state=running", ""),
        ("GET", "/activities/activity-1/objects?owner_uid=0", ""),
        ("GET", "/activities/activity-1/receipts?owner_uid=0", ""),
        ("GET", "/activities/activity-1/receipts?id=another", ""),
        (
            "GET",
            "/activities/activity-1/receipts?source=os_confirmed",
            "",
        ),
        ("GET", "/activities/activity-1/receipts?limit=-1", ""),
        (
            "POST",
            "/activities/continuity/import",
            r#"{"placement":"local","document":{"kind":"claw_os.activity_continuity","schema_version":1,"lineage":{"id":"00000000-0000-4000-8000-000000000123","revision":"7"},"snapshot":"sha256:invalid","intent":{"title":"Release","goal":"Publish","completion_criteria":"","boundaries":""},"references":[],"rules":{"execution_limits":null,"scheduling":null}},"owner_uid":0}"#,
        ),
        (
            "POST",
            "/activities/continuity/import",
            r#"{"placement":"local","placement":"local","document":{}}"#,
        ),
        (
            "POST",
            "/activities/continuity/import",
            r#"{"placement":"remote","document":{}}"#,
        ),
        (
            "GET",
            "/activities/activity-1/continuity/export?owner_uid=0",
            "",
        ),
        (
            "GET",
            "/activities/capability-policy-catalog?owner_uid=0",
            "",
        ),
        (
            "GET",
            "/activities/activity-1/capability-policy?owner_uid=0",
            "",
        ),
        (
            "GET",
            "/activities/activity-1/capability-policy?expected_revision=1",
            "",
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"id":"other","policy":{"rules":[]}}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"owner_uid":0,"policy":{"rules":[]}}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"policy":{"rules":[]},"enabled":true}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"policy":{"rules":[{"verb":"fs.delete","mode":"deny","scopes":[]}],"grant":"forged"}}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"policy":{"rules":[{"verb":"not.known","mode":"deny","scopes":[]}]}}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy",
            r#"{"policy":"rules"}"#,
        ),
        ("POST", "/activities/activity-1/capability-policy", "{"),
        (
            "POST",
            "/activities/activity-1/capability-policy/enabled",
            r#"{"enabled":false}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy/enabled",
            r#"{"expected_revision":1,"enabled":false,"id":"other"}"#,
        ),
        (
            "POST",
            "/activities/activity-1/capability-policy/enabled",
            r#"{"expected_revision":1,"enabled":false,"owner_uid":0}"#,
        ),
        (
            "POST",
            "/activities/activity-1/operation-preview",
            r#"{"app_id":"archive","operation":"get","args":[],"owner_uid":0}"#,
        ),
        (
            "POST",
            "/activities/activity-1/operation-preview",
            r#"{"id":"another","app_id":"archive","operation":"get","args":[]}"#,
        ),
        (
            "POST",
            "/activities/activity-1/objects",
            r#"{"label":"Ref","reference":"app://archive/entry?id=x"}"#,
        ),
        (
            "POST",
            "/activities/activity-1/objects",
            r#"{"id":"other","label":"Ref","object":{"app_id":"archive","object_type":"entry","object_id":"x"}}"#,
        ),
        (
            "POST",
            "/activities/activity-1/update",
            r#"{"id":"another"}"#,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_client_error(), "{path}");
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert!(body["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
    }
}

struct RuntimeDirectory {
    previous: Option<std::ffi::OsString>,
    directory: tempfile::TempDir,
}

impl Drop for RuntimeDirectory {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_RUNTIME_DIR", value),
            None => std::env::remove_var("COS_RUNTIME_DIR"),
        }
    }
}
