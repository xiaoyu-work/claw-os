use super::*;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
};
use cos_agent_protocol::PROTOCOL_VERSION_HEADER;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const TOKEN: &str = "scheduling-priority-test-token";

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: TOKEN.into(),
        clawd: clawd_client::Client::new("missing-scheduling-priority-test.sock"),
    };
    crate::routes::api()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::require_auth,
        ))
        .route_layer(middleware::from_fn(crate::require_protocol_version))
        .with_state(state)
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
async fn scheduling_priority_routes_require_authentication_and_version() {
    let path = format!("/activities/{ACTIVITY}/scheduling-priority");
    for method in [Method::GET, Method::POST] {
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
fn scheduling_priority_params_forward_only_path_policy_and_cas() {
    let body = ActivitySchedulingPrioritySetRequest {
        expected_revision: Some(u64::MAX - 1),
        priority: cos_agent_protocol::ActivitySchedulingPriority::Background,
    };
    let params = with_id(ACTIVITY, &body).ok().unwrap();
    assert_eq!(
        params,
        json!({
            "id": ACTIVITY,
            "expected_revision": u64::MAX - 1,
            "priority": "background"
        })
    );
    for field in ["owner_uid", "job_id", "authority", "preempt", "cancel"] {
        assert!(params.get(field).is_none(), "{field}");
    }
}

#[tokio::test]
async fn scheduling_priority_routes_reject_selectors_unknown_fields_and_fake_authority() {
    let path = format!("/activities/{ACTIVITY}/scheduling-priority");
    for query in ["owner_uid=0", "job_id=job-1", "priority=foreground"] {
        assert_eq!(
            request(Method::GET, &format!("{path}?{query}"), None)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for value in [
        json!({"expected_revision": null, "priority": "urgent"}),
        json!({"expected_revision": 0, "priority": "standard"}),
        json!({"expected_revision": 1, "priority": "foreground", "owner_uid": 0}),
        json!({"expected_revision": 1, "priority": "foreground", "job_id": "job-1"}),
        json!({"expected_revision": 1, "priority": "foreground", "preempt": true}),
    ] {
        assert_eq!(
            request(Method::POST, &path, Some(value)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn scheduling_priority_broker_failure_is_explicit_and_no_extra_routes_exist() {
    assert!(owner_uid().is_ok());
    let path = format!("/activities/{ACTIVITY}/scheduling-priority");
    assert_eq!(
        request(Method::GET, &path, None).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        request(
            Method::POST,
            &path,
            Some(json!({"expected_revision": null, "priority": "standard"})),
        )
        .await
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    for suffix in ["preempt", "cancel", "job", "authority"] {
        assert_eq!(
            request(Method::POST, &format!("{path}/{suffix}"), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
