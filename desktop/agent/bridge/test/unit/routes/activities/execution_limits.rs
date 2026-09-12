use super::*;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    middleware,
};
use cos_agent_protocol::{ErrorCode, ErrorEnvelope, PROTOCOL_VERSION_HEADER};
use serde_json::Value;
use tower::ServiceExt as _;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const TOKEN: &str = "execution-limits-test-token";

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: TOKEN.into(),
        clawd: clawd_client::Client::new("missing-execution-limits-test.sock"),
    };
    crate::routes::api()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::require_auth,
        ))
        .route_layer(middleware::from_fn(crate::require_protocol_version))
        .with_state(state)
}

fn set_value() -> Value {
    json!({
        "expected_revision": null,
        "limits": {"max_attempts": 5, "max_turns_per_attempt": 20, "expires_at": "2099-01-01T12:00:00Z"}
    })
}

async fn request(method: Method, path: &str, body: Option<Value>) -> axum::response::Response {
    router()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(PROTOCOL_VERSION_HEADER, "1")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(body.map_or_else(Body::empty, |value| {
                    Body::from(serde_json::to_vec(&value).unwrap())
                }))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn activity_execution_limits_routes_require_authentication_and_version() {
    for (method, suffix) in [
        (Method::GET, "execution-limits"),
        (Method::POST, "execution-limits"),
        (Method::POST, "execution-limits/enabled"),
    ] {
        let path = format!("/activities/{ACTIVITY}/{suffix}");
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(&path)
                    .header(PROTOCOL_VERSION_HEADER, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    }
}

#[test]
fn activity_execution_limits_params_forward_only_fixed_path_identity_and_explicit_cas() {
    let body: ActivityExecutionLimitsSetRequest = serde_json::from_value(set_value()).unwrap();
    let params = with_id(ACTIVITY, &body).ok().unwrap();
    assert_eq!(params["id"], ACTIVITY);
    assert!(params["expected_revision"].is_null());
    assert_eq!(params.as_object().unwrap().len(), 3);
    assert!(params.get("owner_uid").is_none());
    let toggle = ActivityExecutionLimitsEnabledRequest {
        expected_revision: 7,
        enabled: false,
    };
    assert_eq!(
        with_id(ACTIVITY, toggle).ok().unwrap(),
        json!({
            "id": ACTIVITY, "expected_revision": 7, "enabled": false
        })
    );
}

#[tokio::test]
async fn activity_execution_limits_routes_reject_owner_counter_revision_and_authority_input() {
    let path = format!("/activities/{ACTIVITY}/execution-limits");
    for query in [
        "owner_uid=0",
        "revision=1",
        "used_attempts=0",
        "enabled=true",
        "caps=all",
    ] {
        let response = request(Method::GET, &format!("{path}?{query}"), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let mut invalid = Vec::new();
    for field in [
        "id",
        "owner_uid",
        "revision",
        "used_attempts",
        "enabled",
        "status",
        "caps",
        "approved",
    ] {
        let mut value = set_value();
        value[field] = json!(0);
        invalid.push(value);
    }
    for revision in [json!(0), json!(u64::MAX), json!(-1), json!("1")] {
        let mut value = set_value();
        value["expected_revision"] = revision;
        invalid.push(value);
    }
    let mut missing_revision = set_value();
    missing_revision
        .as_object_mut()
        .unwrap()
        .remove("expected_revision");
    invalid.push(missing_revision);
    for (field, replacement) in [
        ("max_attempts", json!(0)),
        ("max_attempts", json!(1001)),
        ("max_turns_per_attempt", json!(101)),
        ("expires_at", json!("not RFC3339")),
        ("used_attempts", json!(0)),
    ] {
        let mut value = set_value();
        value["limits"][field] = replacement;
        invalid.push(value);
    }
    for value in invalid {
        let response = request(Method::POST, &path, Some(value)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: ErrorEnvelope =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
    for value in [
        json!({"expected_revision": null, "enabled": false}),
        json!({"expected_revision": 0, "enabled": false}),
        json!({"expected_revision": 1, "enabled": true, "owner_uid": 0}),
        json!({"expected_revision": 1, "enabled": true, "used_attempts": 0}),
    ] {
        let response = request(Method::POST, &format!("{path}/enabled"), Some(value)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn activity_execution_limits_broker_failure_never_becomes_unconfigured_or_mutation_success() {
    assert!(owner_uid().is_ok());
    let path = format!("/activities/{ACTIVITY}/execution-limits");
    for (method, path, value) in [
        (Method::GET, path.clone(), None),
        (Method::POST, path.clone(), Some(set_value())),
        (
            Method::POST,
            format!("{path}/enabled"),
            Some(json!({"expected_revision": 5, "enabled": false})),
        ),
    ] {
        let response = request(method, &path, value).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error: ErrorEnvelope =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(error.code, ErrorCode::ServiceUnavailable);
    }
}

#[tokio::test]
async fn activity_execution_limits_have_no_delete_reset_or_unversioned_edit_route() {
    let path = format!("/activities/{ACTIVITY}/execution-limits");
    for method in [Method::DELETE, Method::PUT, Method::PATCH] {
        assert_eq!(
            request(method, &path, Some(set_value())).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    assert_eq!(
        request(Method::POST, &format!("{path}/reset"), None)
            .await
            .status(),
        StatusCode::NOT_FOUND,
    );
}
