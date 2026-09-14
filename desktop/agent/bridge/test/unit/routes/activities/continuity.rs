use super::*;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
};
use cos_agent_protocol::{
    ActivityContinuityDocument, ActivityContinuityImportRequest, ActivityExecutionPlacement,
    PROTOCOL_VERSION_HEADER,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ACTIVITY: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
const TOKEN: &str = "continuity-route-test-token";
const SNAPSHOT: &str = "sha256:6d8c822b0519a02e7ab1ebeab48392cb83756fa9cc8b6d39e94b01ab1682882b";

fn document_value() -> Value {
    json!({
        "kind": "claw_os.activity_continuity",
        "schema_version": 1,
        "lineage": {"id": "00000000-0000-4000-8000-000000000123", "revision": 7},
        "snapshot": SNAPSHOT,
        "intent": {
            "title": "Release",
            "goal": "Publish the release",
            "completion_criteria": "Reviewed and available",
            "boundaries": "Ask before publishing"
        },
        "references": [{
            "label": "Status",
            "reference": "app://kv/entry?id=release.status&revision=v1"
        }],
        "rules": {
            "execution_limits": {
                "enabled": false,
                "max_attempts": 10,
                "max_turns_per_attempt": 5,
                "expires_at": "2030-01-01T00:00:00.000000000Z"
            },
            "scheduling": {"priority": "foreground"}
        }
    })
}

fn router() -> Router {
    let state = AppState {
        port: 0,
        auth_token: TOKEN.into(),
        clawd: clawd_client::Client::new("missing-continuity-route-test.sock"),
    };
    crate::routes::api()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::require_auth,
        ))
        .route_layer(middleware::from_fn(crate::require_protocol_version))
        .with_state(state)
}

async fn request(method: Method, path: &str, body: Body) -> axum::response::Response {
    router()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(PROTOCOL_VERSION_HEADER, "1")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn continuity_routes_require_authentication_and_protocol_version() {
    for (method, path) in [
        (
            Method::GET,
            format!("/activities/{ACTIVITY}/continuity/export"),
        ),
        (Method::POST, "/activities/continuity/import".into()),
    ] {
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
fn continuity_params_forward_only_exact_document_and_explicit_local_placement() {
    let document =
        ActivityContinuityDocument::from_json(&serde_json::to_vec(&document_value()).unwrap())
            .unwrap();
    let canonical = document.to_json().unwrap();
    let params = import_params(ActivityExecutionPlacement::Local, canonical.clone());
    assert_eq!(
        params,
        json!({"placement": "local", "document": canonical})
    );
    assert_eq!(params.as_object().unwrap().len(), 2);
    for field in ["owner_uid", "authority", "restore", "live_sync", "path"] {
        assert!(params.get(field).is_none(), "{field}");
    }
    assert_eq!(
        with_id(ACTIVITY, json!({})).ok().unwrap(),
        json!({"id": ACTIVITY})
    );
}

#[tokio::test]
async fn continuity_routes_reject_selectors_authority_duplicates_versions_and_bounds() {
    let export_path = format!("/activities/{ACTIVITY}/continuity/export");
    for query in ["owner_uid=0", "authority=root", "placement=local"] {
        assert_eq!(
            request(Method::GET, &format!("{export_path}?{query}"), Body::empty())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let document = serde_json::to_string(&document_value()).unwrap();
    for value in [
        json!({"placement": "remote", "document": document}),
        json!({"placement": "local", "document": document, "owner_uid": 0}),
        json!({"placement": "local", "document": document, "authority": "root"}),
    ] {
        assert_eq!(
            request(
                Method::POST,
                "/activities/continuity/import",
                Body::from(serde_json::to_vec(&value).unwrap()),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let duplicate_document = document.replacen(
        r#""kind":"claw_os.activity_continuity""#,
        r#""kind":"claw_os.activity_continuity","kind":"claw_os.activity_continuity""#,
        1,
    );
    let value = ActivityContinuityImportRequest {
        placement: ActivityExecutionPlacement::Local,
        document: duplicate_document,
    };
    assert_eq!(
        request(
            Method::POST,
            "/activities/continuity/import",
            Body::from(serde_json::to_vec(&value).unwrap()),
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let duplicate_outer = format!(
        r#"{{"placement":"local","placement":"local","document":{}}}"#,
        serde_json::to_string(&document).unwrap()
    );
    assert_eq!(
        request(
            Method::POST,
            "/activities/continuity/import",
            Body::from(duplicate_outer),
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let too_large = serde_json::to_vec(&json!({
        "placement": "local",
        "document": "x".repeat(crate::routes::CONTINUITY_IMPORT_MAX_HTTP_BYTES)
    }))
    .unwrap();
    assert_eq!(
        request(
            Method::POST,
            "/activities/continuity/import",
            Body::from(too_large),
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn valid_continuity_requests_surface_broker_failures_without_local_success() {
    let export_path = format!("/activities/{ACTIVITY}/continuity/export");
    assert_eq!(
        request(Method::GET, &export_path, Body::empty())
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let body = ActivityContinuityImportRequest {
        placement: ActivityExecutionPlacement::Local,
        document: serde_json::to_string(&document_value()).unwrap(),
    };
    assert_eq!(
        request(
            Method::POST,
            "/activities/continuity/import",
            Body::from(serde_json::to_vec(&body).unwrap()),
        )
        .await
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
