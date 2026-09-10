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
    let broker = tokio::spawn(async move {
        for command in [Command::ActivityRun, Command::ActivityGet] {
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
            let response = if command == Command::ActivityRun {
                assert_eq!(request.params["prompt"], "Continue the saved goal");
                assert_eq!(request.params["session_id"], "session-1");
                Response::ok(request.id, submitted.clone())
            } else {
                assert_eq!(request.params["limit"], 20);
                Response::error(request.id, "unavailable", "Activity store unavailable")
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
        .route("/activities/{id}/update", post(update));
    for (method, path, body) in [
        (
            "POST",
            "/activities",
            r#"{"title":"Goal","goal":"Result","owner_uid":0}"#,
        ),
        ("GET", "/activities?owner_uid=0", ""),
        ("GET", "/activities?state=running", ""),
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
