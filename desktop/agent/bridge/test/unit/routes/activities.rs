use super::*;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
    middleware,
    response::IntoResponse,
};
use cos_agent_protocol::{ErrorEnvelope, PROTOCOL_VERSION_HEADER};
use tower::ServiceExt as _;

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: "activity-test-token".into(),
        clawd: clawd_client::Client::new("missing-activity-test.sock"),
    };
    crate::routes::api()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::require_auth,
        ))
        .route_layer(middleware::from_fn(crate::require_protocol_version))
        .with_state(state)
}

#[tokio::test]
async fn every_activity_surface_requires_authentication_and_version() {
    for (method, path) in [
        (Method::GET, "/activities"),
        (Method::POST, "/activities"),
        (Method::GET, "/activities/a"),
        (Method::GET, "/activities/a/receipts"),
        (Method::GET, "/activities/a/object-state"),
        (Method::POST, "/activities/a/object-state"),
        (Method::GET, "/activities/a/objects"),
        (Method::POST, "/activities/a/objects"),
        (Method::POST, "/activities/a/operation-preview"),
        (Method::PATCH, "/activities/a"),
        (Method::POST, "/activities/a/transition"),
        (Method::POST, "/activities/a/run"),
        (Method::POST, "/tasks/j/retry"),
    ] {
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header(PROTOCOL_VERSION_HEADER, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(response.headers()[PROTOCOL_VERSION_HEADER], "1");

        let response = router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("authorization", "Bearer activity-test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED, "{path}");
    }
}

#[tokio::test]
async fn owner_injection_and_malformed_requests_return_typed_errors() {
    for (method, path, body) in [
        (
            Method::POST,
            "/activities",
            r#"{"title":"Goal","goal":"Work","owner_uid":0}"#,
        ),
        (
            Method::PATCH,
            "/activities/a",
            r#"{"goal":"Work","owner_uid":0}"#,
        ),
        (
            Method::POST,
            "/activities/a/transition",
            r#"{"state":"active","owner_uid":0}"#,
        ),
        (Method::POST, "/activities/a/run", r#"{"owner_uid":0}"#),
        (
            Method::POST,
            "/activities/a/operation-preview",
            r#"{"app_id":"kv","operation":"get","args":["entry"],"owner_uid":0}"#,
        ),
        (
            Method::POST,
            "/activities/a/operation-preview",
            r#"{"app_id":"kv","operation":"get","args":["entry"],"execute":true}"#,
        ),
        (
            Method::POST,
            "/activities/a/operation-preview",
            r#"{"app_id":"kv","operation":"get","args":"entry"}"#,
        ),
        (
            Method::POST,
            "/activities/a/objects",
            r#"{"label":"Object","object":{"app_id":"kv","object_type":"entry","object_id":"x"},"owner_uid":0}"#,
        ),
        (
            Method::POST,
            "/activities/a/objects",
            r#"{"label":"Object","reference":"app://kv/entry?id=x"}"#,
        ),
        (Method::GET, "/activities?owner_uid=0", ""),
        (Method::GET, "/activities?limit=101", ""),
        (Method::GET, "/activities/a/receipts?owner_uid=0", ""),
        (Method::GET, "/activities/a/receipts?limit=0", ""),
        (Method::GET, "/activities/a/receipts?limit=101", ""),
    ] {
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(PROTOCOL_VERSION_HEADER, "1")
                    .header("authorization", "Bearer activity-test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let error: ErrorEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
}

#[test]
fn typed_params_forward_only_activity_fields_and_path_identity() {
    let request = ActivityRunRequest {
        prompt: Some("Continue".into()),
        session_id: Some("session".into()),
        ..ActivityRunRequest::default()
    };
    assert_eq!(
        with_id("activity", request).ok().unwrap(),
        json!({"id": "activity", "prompt": "Continue", "session_id": "session"})
    );
    assert!(with_id("", json!({})).is_err());
}

#[test]
fn object_params_forward_opaque_identity_without_constructing_a_uri() {
    let request: ActivityObjectAttachRequest = serde_json::from_value(json!({
        "label": "Object",
        "object": {
            "app_id": "kv", "object_type": "entry",
            "object_id": " a/b?x=1&y=2 ", "revision": "version/#? "
        }
    }))
    .unwrap();
    let params = with_id("activity", request).ok().unwrap();
    assert_eq!(params["id"], "activity");
    assert_eq!(params["object"]["object_id"], " a/b?x=1&y=2 ");
    assert_eq!(params["object"]["revision"], "version/#? ");
    assert!(params.get("owner_uid").is_none());
    assert!(params.get("reference").is_none());
    assert!(params.get("invocation").is_none());
    assert_eq!(
        with_id("activity", json!({})).ok().unwrap(),
        json!({"id": "activity"})
    );
}

#[test]
fn operation_preview_params_are_owner_free_and_preserve_opaque_argv() {
    let request = ActivityOperationPreviewRequest {
        app_id: "kv".into(),
        operation: "get".into(),
        args: vec!["entry with ; $() metacharacters".into()],
    };
    assert_eq!(
        with_id("activity", request).ok().unwrap(),
        json!({
            "id": "activity", "app_id": "kv", "operation": "get",
            "args": ["entry with ; $() metacharacters"]
        })
    );
}

#[test]
fn receipt_query_forwards_only_activity_identity_and_limit() {
    assert_eq!(
        with_id("activity", ActivityReceiptsQuery { limit: Some(100) })
            .ok()
            .unwrap(),
        json!({"id": "activity", "limit": 100})
    );
}

#[tokio::test]
async fn receipt_surface_does_not_expose_authoring_mutation_or_execution() {
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri("/activities/a/receipts")
                    .header(PROTOCOL_VERSION_HEADER, "1")
                    .header("authorization", format!("Bearer {}", "activity-test-token"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
    }
}

#[tokio::test]
async fn broker_refusals_are_errors_not_local_success() {
    let error = upstream_error(BrokerError::Remote(clawd_client::RemoteError {
        code: BrokerErrorCode::NotAuthorized,
        message: "not authorized".into(),
        data: None,
    }));
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let error: ErrorEnvelope = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(error.code, ErrorCode::Unauthorized);
}

fn object_state_body() -> Value {
    json!({"entry": {
        "id": "22222222-2222-4222-8222-222222222222",
        "reference": "app://kv/entry?id=a%2Fb%3Fx%3D1",
        "content": {"kind": "user_statement", "text": "Caller report"},
        "observed_at": null, "valid_until": null, "supersedes": null
    }})
}

async fn object_state_http(method: Method, uri: &str, value: Option<Value>) -> axum::response::Response {
    let body = value.map_or_else(Body::empty, |value| {
        Body::from(serde_json::to_vec(&value).unwrap())
    });
    router().oneshot(Request::builder()
        .method(method).uri(uri)
        .header(PROTOCOL_VERSION_HEADER, "1")
        .header("authorization", format!("Bearer {}", "activity-test-token"))
        .header("content-type", "application/json")
        .body(body).unwrap()
    ).await.unwrap()
}

#[test]
fn activity_object_state_params_preserve_draft_identity_and_only_path_activity_identity() {
    let body = object_state_body();
    let request: ActivityObjectStateRecordRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        with_id("11111111-1111-4111-8111-111111111111", &request).ok().unwrap(),
        json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "entry": body["entry"].clone()
        }),
    );
    assert_eq!(
        with_id("a", ActivityObjectStateQuery::default()).ok().unwrap(), json!({"id": "a"})
    );
    let reference = "app://kv/entry?id=a%2Fb%3Fx%3D1";
    assert_eq!(
        with_id("a", ActivityObjectStateQuery {
            reference: Some(reference.into()), limit: Some(100),
        }).ok().unwrap(),
        json!({"id": "a", "reference": reference, "limit": 100}),
    );
}

#[tokio::test]
async fn activity_object_state_routes_reject_untrusted_selectors_and_malformed_shapes() {
    for query in [
        "owner_uid=0", "source=caller_reported", "limit=0", "limit=101",
        "limit=not-a-number", "reference=", "execute=true",
    ] {
        let response = object_state_http(
            Method::GET, &format!("/activities/a/object-state?{query}"), None,
        ).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let error: ErrorEnvelope = serde_json::from_slice(
            &to_bytes(response.into_body(), 4096).await.unwrap()
        ).unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
    let mut invalid_bodies = Vec::new();
    for field in ["id", "owner_uid", "source", "execute", "receipt"] {
        let mut body = object_state_body();
        body[field] = json!(0);
        invalid_bodies.push(body);
    }
    for field in ["owner_uid", "source", "validity", "receipt"] {
        let mut body = object_state_body();
        body["entry"][field] = json!(0);
        invalid_bodies.push(body);
    }
    for content in [
        json!({"kind": "user_statement", "text": ""}),
        json!({"kind": "agent_inference", "text": "\u{e9}".repeat(2049)}),
        json!({"kind": "app_report", "receipt_id": ""}),
        json!({"kind": "app_report", "receipt_id": "r", "text": "Replacement output"}),
        json!({"kind": "relation", "relation": "related_to", "target": "", "note": ""}),
        json!({"kind": "relation", "relation": "related_to", "target": "app://kv/entry?id=x",
            "note": "\u{e9}".repeat(1025)}),
        json!({"kind": "retracted", "reason": "Missing predecessor"}),
        json!({"kind": "verified_fact", "text": "Not accepted"}),
    ] {
        let mut body = object_state_body();
        body["entry"]["content"] = content;
        invalid_bodies.push(body);
    }
    let mut half_window = object_state_body();
    half_window["entry"]["observed_at"] = json!("2026-09-11T12:00:00Z");
    invalid_bodies.push(half_window);
    for body in invalid_bodies {
        let response = object_state_http(
            Method::POST, "/activities/a/object-state", Some(body),
        ).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: ErrorEnvelope = serde_json::from_slice(
            &to_bytes(response.into_body(), 4096).await.unwrap()
        ).unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
}

#[tokio::test]
async fn activity_object_state_valid_routes_surface_missing_broker_without_local_success() {
    for (method, value) in [(Method::GET, None), (Method::POST, Some(object_state_body()))] {
        let response = object_state_http(
            method, "/activities/11111111-1111-4111-8111-111111111111/object-state", value,
        ).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error: ErrorEnvelope = serde_json::from_slice(
            &to_bytes(response.into_body(), 4096).await.unwrap()
        ).unwrap();
        assert_eq!(error.code, ErrorCode::ServiceUnavailable);
    }
}

#[tokio::test]
async fn activity_object_state_is_append_only_without_update_or_delete_routes() {
    for method in [Method::PUT, Method::PATCH, Method::DELETE] {
        let response = object_state_http(
            method, "/activities/a/object-state", Some(object_state_body()),
        ).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
