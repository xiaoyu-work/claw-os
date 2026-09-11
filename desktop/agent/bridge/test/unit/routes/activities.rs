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
        (Method::GET, "/activities/a/objects"),
        (Method::POST, "/activities/a/objects"),
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
