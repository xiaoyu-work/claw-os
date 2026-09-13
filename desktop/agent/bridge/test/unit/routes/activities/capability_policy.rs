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
const TOKEN: &str = "capability-policy-test-token";

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: TOKEN.into(),
        clawd: clawd_client::Client::new("missing-capability-policy-test.sock"),
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
    json!({"expected_revision": null, "policy": {"rules": [
        {"verb": "fs.read", "mode": "normal", "scopes": [{"kind": "path", "value": "/workspace/**"}]}
    ]}})
}

async fn request(method: Method, path: &str, value: Option<Value>) -> axum::response::Response {
    router()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(PROTOCOL_VERSION_HEADER, "1")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(value.map_or_else(Body::empty, |value| {
                    Body::from(serde_json::to_vec(&value).unwrap())
                }))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn capability_policy_routes_require_authentication_and_version() {
    for (method, suffix) in [
        (Method::GET, "capability-policy"),
        (Method::POST, "capability-policy"),
        (Method::POST, "capability-policy/enabled"),
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
fn capability_policy_params_keep_path_identity_and_explicit_cas_without_authority() {
    let body: ActivityCapabilityPolicySetRequest = serde_json::from_value(set_value()).unwrap();
    let params = with_id(ACTIVITY, body).ok().unwrap();
    assert_eq!(params["id"], ACTIVITY);
    assert!(params["expected_revision"].is_null());
    assert_eq!(params.as_object().unwrap().len(), 3);
    assert_eq!(
        with_id(
            ACTIVITY,
            ActivityCapabilityPolicyEnabledRequest {
                expected_revision: 5,
                enabled: false,
            }
        )
        .ok()
        .unwrap(),
        json!({"id": ACTIVITY, "expected_revision": 5, "enabled": false})
    );
}

#[tokio::test]
async fn capability_policy_routes_reject_selectors_malformed_rules_and_missing_cas() {
    let path = format!("/activities/{ACTIVITY}/capability-policy");
    for query in [
        "owner_uid=0",
        "revision=1",
        "enabled=true",
        "approved=true",
        "grant=all",
    ] {
        assert_eq!(
            request(Method::GET, &format!("{path}?{query}"), None)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let mut invalid = Vec::new();
    for field in [
        "id",
        "owner_uid",
        "revision",
        "enabled",
        "approved",
        "grant",
    ] {
        let mut data = set_value();
        data[field] = json!(true);
        invalid.push(data);
    }
    let mut missing = set_value();
    missing.as_object_mut().unwrap().remove("expected_revision");
    invalid.push(missing);
    for rule in [
        json!({"verb": "fs.read", "mode": "normal", "scopes": []}),
        json!({"verb": "fs.delete", "mode": "deny", "scopes": [{"kind": "wild"}]}),
        json!({"verb": "fs.read", "mode": "grant", "scopes": [{"kind": "wild"}]}),
        json!({"verb": "fs.read", "mode": "normal", "scopes": [{"kind": "none"}]}),
        json!({"verb": "ui.notify", "mode": "normal", "scopes": [{"kind": "wild", "value": "*"}]}),
    ] {
        let mut data = set_value();
        data["policy"]["rules"] = json!([rule]);
        invalid.push(data);
    }
    for data in invalid {
        let response = request(Method::POST, &path, Some(data)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: ErrorEnvelope =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
    for data in [
        json!({"expected_revision": null, "enabled": false}),
        json!({"expected_revision": 0, "enabled": true}),
        json!({"expected_revision": 5, "enabled": true, "approved": true}),
    ] {
        assert_eq!(
            request(Method::POST, &format!("{path}/enabled"), Some(data))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn capability_policy_broker_failure_is_not_unconfigured_or_local_success() {
    let path = format!("/activities/{ACTIVITY}/capability-policy");
    for (method, path, body) in [
        (Method::GET, path.clone(), None),
        (Method::POST, path.clone(), Some(set_value())),
        (
            Method::POST,
            format!("{path}/enabled"),
            Some(json!({"expected_revision": 5, "enabled": false})),
        ),
    ] {
        let response = request(method, &path, body).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let error: ErrorEnvelope =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(error.code, ErrorCode::ServiceUnavailable);
    }
}

#[tokio::test]
async fn capability_policy_has_no_grant_approval_delete_or_reset_route() {
    let path = format!("/activities/{ACTIVITY}/capability-policy");
    for method in [Method::DELETE, Method::PUT, Method::PATCH] {
        assert_eq!(
            request(method, &path, Some(set_value())).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    for action in ["grant", "approve", "approval/decide", "reset"] {
        assert_eq!(
            request(Method::POST, &format!("{path}/{action}"), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
