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
const TOKEN: &str = "monetary-budget-test-token";

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: TOKEN.into(),
        clawd: clawd_client::Client::new("missing-monetary-budget-test.sock"),
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
        "budget": {
            "currency": "USD", "max_total_microusd": 5000000,
            "input_microusd_per_million_tokens": 250000,
            "output_microusd_per_million_tokens": 1000000,
            "max_output_tokens_per_turn": 4096
        }
    })
}

async fn request(method: Method, path: &str, body: Option<Value>) -> axum::response::Response {
    router()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(
                    PROTOCOL_VERSION_HEADER,
                    cos_agent_protocol::CURRENT_PROTOCOL_VERSION_HEADER_VALUE,
                )
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
async fn monetary_budget_routes_require_authentication_and_version() {
    for (method, suffix) in [
        (Method::GET, "monetary-budget"),
        (Method::POST, "monetary-budget"),
        (Method::POST, "monetary-budget/enabled"),
    ] {
        let path = format!("/activities/{ACTIVITY}/{suffix}");
        let response = router()
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(&path)
                    .header(
                        PROTOCOL_VERSION_HEADER,
                        cos_agent_protocol::CURRENT_PROTOCOL_VERSION_HEADER_VALUE,
                    )
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
fn monetary_budget_params_forward_only_path_identity_policy_and_cas() {
    let body: ActivityMonetaryBudgetSetRequest = serde_json::from_value(set_value()).unwrap();
    let params = with_id(ACTIVITY, &body).ok().unwrap();
    assert_eq!(params["id"], ACTIVITY);
    assert!(params["expected_revision"].is_null());
    assert_eq!(params.as_object().unwrap().len(), 3);
    assert!(params.get("owner_uid").is_none());
    assert!(params.get("spent_microusd").is_none());
    let toggle = ActivityMonetaryBudgetEnabledRequest {
        expected_revision: u64::MAX - 1,
        enabled: false,
    };
    assert_eq!(
        with_id(ACTIVITY, toggle).ok().unwrap(),
        json!({"id": ACTIVITY, "expected_revision": u64::MAX - 1, "enabled": false})
    );
}

#[tokio::test]
async fn monetary_budget_routes_reject_owner_ledger_prices_and_invalid_bounds() {
    let path = format!("/activities/{ACTIVITY}/monetary-budget");
    for query in ["owner_uid=0", "spent_microusd=0", "provider=demo", "revision=1"] {
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
        "spent_microusd",
        "reserved_microusd",
        "provider",
        "model",
        "enabled",
    ] {
        let mut value = set_value();
        value[field] = json!(0);
        invalid.push(value);
    }
    for (field, replacement) in [
        ("currency", json!("EUR")),
        ("max_total_microusd", json!(0)),
        ("input_microusd_per_million_tokens", json!(1_000_000_000_001_u64)),
        ("max_output_tokens_per_turn", json!(0)),
    ] {
        let mut value = set_value();
        value["budget"][field] = replacement;
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
        json!({"expected_revision": 1, "enabled": true, "spent_microusd": 0}),
    ] {
        assert_eq!(
            request(Method::POST, &format!("{path}/enabled"), Some(value))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn monetary_budget_broker_failure_never_becomes_absent_or_mutation_success() {
    assert!(owner_uid().is_ok());
    let path = format!("/activities/{ACTIVITY}/monetary-budget");
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
    }
}

#[tokio::test]
async fn monetary_budget_has_no_delete_reset_or_provider_price_route() {
    let path = format!("/activities/{ACTIVITY}/monetary-budget");
    for method in [Method::DELETE, Method::PUT, Method::PATCH] {
        assert_eq!(
            request(method, &path, Some(set_value())).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    for suffix in ["reset", "provider-price", "settle"] {
        assert_eq!(
            request(Method::POST, &format!("{path}/{suffix}"), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
