use super::*;

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

    let state =
        crate::agent::web::state::AppState::new(crate::config::AgentConfig::default(), 1000);
    for (method, path) in [
        ("GET", "/api/activities"),
        ("POST", "/api/activities"),
        ("GET", "/api/activities/activity-1"),
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
        ("GET", "/api/activities/capability-policy-catalog"),
        ("GET", "/api/activities/activity-1/capability-policy"),
        ("POST", "/api/activities/activity-1/capability-policy"),
        ("POST", "/api/activities/activity-1/capability-policy/enabled"),
    ] {
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
            "{method} {path}"
        );
    }
}

#[test]
fn activity_capability_policy_requests_preserve_cas_and_reject_authority_fields() {
    let policy = json!({"rules": [{"verb": "fs.delete", "mode": "deny", "scopes": []}]});
    let body = json!({"expected_revision": 2, "policy": policy});
    let request = with_id::<ActivityCapabilityPolicySet>("activity-1".into(), body.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"id": "activity-1", "expected_revision": 2, "policy": policy})
    );
    let enabled = with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(), json!({"expected_revision": 2, "enabled": false}),
    ).unwrap();
    assert_eq!(serde_json::to_value(enabled).unwrap(),
        json!({"id": "activity-1", "expected_revision": 2, "enabled": false}));
    for (field, value) in [
        ("id", json!("another")),
        ("owner_uid", json!(0)),
        ("enabled", json!(true)),
        ("revision", json!(2)),
        ("grant", json!("forged")),
        ("expected_revision", json!(-1)),
        ("policy", json!({"rules": [], "enabled": false})),
        ("policy", json!({"rules": [{
            "verb": "fs.read", "mode": "normal", "scopes": [{"kind": "wild"}],
        }]})),
        ("policy", json!({"rules": [{
            "verb": "fs.read", "mode": "normal",
            "scopes": [{"kind": "path", "value": "x".repeat(16 * 1024)}],
        }]})),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(with_id::<ActivityCapabilityPolicySet>("activity-1".into(), invalid).is_err());
    }
    assert!(with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(), json!({"enabled": false}),
    ).is_err());
    assert!(with_id::<ActivityCapabilityPolicyEnabled>(
        "activity-1".into(), json!({"expected_revision": 1, "enabled": "false"}),
    ).is_err());
}

#[tokio::test]
async fn capability_policy_catalog_is_static_core_metadata_not_app_or_owner_data() {
    let Json(catalog) = capability_policy_catalog(Ok(Query(NoBody::default()))).await.unwrap();
    assert_eq!(catalog["schema"], 1);
    let verbs = catalog["verbs"].as_array().unwrap();
    assert_eq!(verbs.len(), crate::caps::CATALOG.len());
    for (value, entry) in verbs.iter().zip(crate::caps::CATALOG) {
        assert_eq!(value, &json!({
            "verb": entry.verb, "scope_kind": entry.scope_kind,
            "label": entry.label.current(), "description": entry.blurb.current(),
        }));
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
    let socket = directory.path().join("clawd.sock");
    let _runtime = RuntimeDirectory {
        previous: std::env::var_os("COS_RUNTIME_DIR"),
        directory,
    };
    std::env::set_var("COS_RUNTIME_DIR", _runtime.directory.path());
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
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = [0; HEADER_BYTES];
            socket.read_exact(&mut header).await.unwrap();
            let size = frame::parse_header(&header, KIND_REQUEST, MAX_REQUEST_BYTES).unwrap();
            let mut body = vec![0; size];
            socket.read_exact(&mut body).await.unwrap();
            let request: Request = serde_json::from_slice(&body).unwrap();
            assert_eq!(request.command, command);
            assert_eq!(request.params["id"], "activity-1");
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
                        Response::ok(request.id, json!({
                            "schema": 1, "activity_id": "activity-1", "capability_policy": null,
                        }))
                    } else {
                        Response::error(request.id, "unavailable", "Capability policy unavailable")
                    }
                }
                Command::ActivityCapabilityPolicySet => {
                    policy_writes += 1;
                    if policy_writes == 1 {
                        assert_eq!(request.params, json!({
                            "id": "activity-1", "policy": forwarded_draft,
                        }));
                        Response::ok(request.id, policy_response.clone())
                    } else {
                        assert_eq!(request.params, json!({
                            "id": "activity-1", "expected_revision": 1, "policy": forwarded_draft,
                        }));
                        Response::error(request.id, "execution_failed", "Capability policy revision conflict")
                    }
                }
                Command::ActivityCapabilityPolicyEnabled => {
                    assert_eq!(request.params, json!({
                        "id": "activity-1", "expected_revision": 1, "enabled": false,
                    }));
                    let mut disabled = policy_response.clone();
                    disabled["revision"] = json!(2);
                    disabled["enabled"] = json!(false);
                    Response::ok(request.id, disabled)
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
    ).await.unwrap().unwrap();
    assert_eq!(absent, json!({
        "schema": 1, "activity_id": "activity-1", "capability_policy": null,
    }));
    let Json(saved) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        set_capability_policy(Path("activity-1".into()), Ok(Json(json!({
            "expected_revision": null, "policy": policy_draft,
        })))),
    ).await.unwrap().unwrap();
    assert_eq!(saved, policy_result);
    let Json(disabled) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        enable_capability_policy(Path("activity-1".into()), Ok(Json(json!({
            "expected_revision": 1, "enabled": false,
        })))),
    ).await.unwrap().unwrap();
    assert_eq!(disabled["revision"], 2);
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["rules"], policy_result["rules"]);
    let (status, Json(error)) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        set_capability_policy(Path("activity-1".into()), Ok(Json(json!({
            "expected_revision": 1, "policy": policy_draft,
        })))),
    ).await.unwrap().unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "Capability policy revision conflict");
    let (status, Json(error)) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        capability_policy(Path("activity-1".into()), Ok(Query(NoBody::default()))),
    ).await.unwrap().unwrap_err();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "Capability policy unavailable");
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
        .route("/activities/capability-policy-catalog", http_get(capability_policy_catalog))
        .route("/activities/{id}/capability-policy", http_get(capability_policy).post(set_capability_policy))
        .route("/activities/{id}/capability-policy/enabled", post(enable_capability_policy))
        .route("/activities/{id}/update", post(update));
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
        ("GET", "/activities/capability-policy-catalog?owner_uid=0", ""),
        ("GET", "/activities/activity-1/capability-policy?owner_uid=0", ""),
        ("GET", "/activities/activity-1/capability-policy?expected_revision=1", ""),
        ("POST", "/activities/activity-1/capability-policy", r#"{"id":"other","policy":{"rules":[]}}"#),
        ("POST", "/activities/activity-1/capability-policy", r#"{"owner_uid":0,"policy":{"rules":[]}}"#),
        ("POST", "/activities/activity-1/capability-policy", r#"{"policy":{"rules":[]},"enabled":true}"#),
        ("POST", "/activities/activity-1/capability-policy", r#"{"policy":{"rules":[{"verb":"fs.delete","mode":"deny","scopes":[]}],"grant":"forged"}}"#),
        ("POST", "/activities/activity-1/capability-policy", r#"{"policy":{"rules":[{"verb":"not.known","mode":"deny","scopes":[]}]}}"#),
        ("POST", "/activities/activity-1/capability-policy", r#"{"policy":"rules"}"#),
        ("POST", "/activities/activity-1/capability-policy", "{"),
        ("POST", "/activities/activity-1/capability-policy/enabled", r#"{"enabled":false}"#),
        ("POST", "/activities/activity-1/capability-policy/enabled", r#"{"expected_revision":1,"enabled":false,"id":"other"}"#),
        ("POST", "/activities/activity-1/capability-policy/enabled", r#"{"expected_revision":1,"enabled":false,"owner_uid":0}"#),
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
