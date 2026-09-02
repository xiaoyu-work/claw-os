use super::*;

use crate::clawd::protocol::{BrokerError, Response};

const FORBIDDEN: &[&str] = &[
    "d34db33f-launch-handle",
    "ya29.oauth-access-token",
    "hunter2-password",
    "0123456789abcdef",
    "approval_requests",
];

fn assert_clean(rendered: &str) {
    for secret in FORBIDDEN {
        assert!(
            !rendered.contains(secret),
            "system journal leaked {secret}: {rendered}"
        );
    }
}

fn journal_record(command: &str, params: Value, response: &Response) -> Value {
    clawd_request_record(
        &audit_policy::request_facts(command, &params),
        &response.audit_facts(),
        Duration::from_millis(1),
        &ClientIdentity::unknown(),
    )
}

#[test]
fn query_returns_recent_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", tmp.path());

    let client = ClientIdentity::unknown();
    let response = Response::ok(
        crate::clawd::protocol::RequestId::unknown(),
        json!({"status": "ok"}),
    );
    record_clawd_request(
        &audit_policy::request_facts("daemon.health", &Value::Null),
        &response.audit_facts(),
        Duration::from_millis(3),
        &client,
    );
    let result = query(json!({"limit": 10})).unwrap();
    assert_eq!(result["operations"][0]["source"], "clawd.request");
    assert_eq!(result["operations"][0]["operation"], "daemon.health");

    match prev {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}

#[test]
fn the_journal_projection_matches_the_broker_audit_projection() {
    // Both sinks are handed the same facts, so a value masked in one
    // can never survive in the other.
    let params = json!({"session_id": "app-1", "handle": "d34db33f-launch-handle", "pid": 4242});
    let response = Response::ok(
        crate::clawd::protocol::RequestId::unknown(),
        json!({"bound": true}),
    );
    let facts = audit_policy::request_facts("app_session.bind", &params);
    let record = journal_record("app_session.bind", params, &response);
    assert_eq!(record["request"], serde_json::to_value(&facts).unwrap());
    assert_clean(&serde_json::to_string(&record).unwrap());
}

#[test]
fn launch_handles_are_masked_in_the_system_journal() {
    let record = journal_record(
        "app_session.bind",
        json!({"session_id": "app-1", "handle": "d34db33f-launch-handle", "pid": 4242}),
        &Response::ok(
            crate::clawd::protocol::RequestId::unknown(),
            json!({"bound": true}),
        ),
    );
    assert_eq!(record["operation"], json!("app_session.bind"));
    assert_eq!(record["request"]["params"]["session_id"], json!("app-1"));
    assert!(record["request"]["params"].get("handle").is_none());
    assert_clean(&serde_json::to_string(&record).unwrap());
}

#[test]
fn peer_only_denial_data_never_reaches_the_system_journal() {
    let record = journal_record(
        "app_session.register",
        json!({"app_id": "user-manager", "handle": "d34db33f-launch-handle"}),
        &Response::handler_error(
            crate::clawd::protocol::RequestId::unknown(),
            BrokerError::authorization_required(
                "launcher cannot delegate sys.identity:name:accounts; awaiting approval",
                json!({"status": "approval_required", "approval_requests": ["ap-1"]}),
            ),
        ),
    );
    assert_eq!(record["error"]["class"], json!("approval_required"));
    assert_eq!(record["request"]["params"]["app_id"], json!("user-manager"));
    assert_clean(&serde_json::to_string(&record).unwrap());
}

#[test]
fn scheduler_credentials_are_counted_not_journalled() {
    let record = journal_record(
        "scheduler.run",
        json!({
            "subsystem": "cron",
            "command": "add",
            "args": ["--credential", "hunter2-password"],
        }),
        &Response::ok(
            crate::clawd::protocol::RequestId::unknown(),
            json!({"added": true}),
        ),
    );
    assert_eq!(record["request"]["params"]["command"], json!("add"));
    assert_eq!(
        record["request"]["params"]["args"],
        json!({"type": "array", "len": 2})
    );
    assert_clean(&serde_json::to_string(&record).unwrap());
}

#[test]
fn refused_frames_are_journalled_as_metadata() {
    let response = Response::fault(
        crate::clawd::protocol::RequestId::unknown(),
        crate::clawd::wire::Fault::MalformedBody,
    );
    let record = protocol_failure_record(
        &audit_policy::protocol_failure_facts("invalid_json", 78, None),
        &response.audit_facts(),
        Duration::from_millis(1),
        &ClientIdentity::unknown(),
    );
    let rendered = serde_json::to_string(&record).unwrap();
    assert_clean(&rendered);
    assert_eq!(record["operation"], json!("invalid_json"));
    assert_eq!(record["request"]["bytes"], json!(78));
    assert!(
        record.get("raw").is_none(),
        "the frame itself is never stored"
    );
    assert!(
        record["request"].get("body").is_none(),
        "not even a digest of the frame is stored"
    );
}

#[test]
fn approval_reasons_are_journalled_as_metadata() {
    let request = crate::approvals::Request {
        id: "ap-1".to_string(),
        verb: "secret.read".to_string(),
        scope: crate::caps::Scope::name("default/GOOGLE_ACCESS_TOKEN"),
        session: "app-1".to_string(),
        reason: "needs hunter2-password to continue".to_string(),
        requested_at: 0,
        owner_uid: Some(1000),
        risk: Some(crate::caps::Risk::High),
        context: Some(crate::caps::ConsentContext::Attended),
        execution: Some(crate::approvals::ApprovalExecutionBinding {
            identity: crate::approvals::ApprovalExecutionIdentity {
                task_id: "task-1".to_string(),
                worker_pid: 42,
                worker_start_time_ticks: Some(7),
                lease_nonce: "0123456789abcdef".to_string(),
            },
            expires_at: 10,
            generation: 0,
        }),
        resumable_until: None,
        operation_digest: Some(crate::crypto::sha256_hex(b"validated invocation")),
        requester: Some("uid:1000".to_string()),
    };
    let record = approval_request_record(&request);
    let rendered = serde_json::to_string(&record).unwrap();
    assert_clean(&rendered);
    assert_eq!(record["approval_id"], json!("ap-1"));
    assert_eq!(record["session_id"], json!("app-1"));
    assert_eq!(record["verb"], json!("secret.read"));
    assert_eq!(record["risk"], json!("high"));
    assert_eq!(record["consent_context"], json!("attended"));
    assert_eq!(record["task_id"], json!("task-1"));
    assert_eq!(record["worker_pid"], json!(42));
    assert_eq!(record["request_expires_at"], json!(10));
    assert_eq!(record["request_generation"], json!(0));
    assert_eq!(record["operation_digest"], json!(request.operation_digest));
    assert_eq!(record["reason"]["bytes"], json!(request.reason.len()));
}

#[test]
fn worker_failures_are_journalled_as_metadata() {
    let error = "provider rejected key hunter2-password";
    let job: Job = serde_json::from_value(json!({
        "id": "job-1",
        "prompt": "summarise ya29.oauth-access-token",
        "status": "error",
        "created_at": "2026-01-01T00:00:00Z",
        "session_id": "sess-1",
        "provider": "anthropic",
        "model": "claude",
        "error": error,
        "owner_uid": 1000,
    }))
    .expect("job");
    let record = task_event_record("task.failed", &job);
    let rendered = serde_json::to_string(&record).unwrap();
    assert_clean(&rendered);
    assert_eq!(record["job_id"], json!("job-1"));
    assert_eq!(record["provider"], json!("anthropic"));
    assert_eq!(record["error"]["bytes"], json!(error.len()));
    assert_eq!(record["client_source"], json!("unknown"));
    assert_eq!(record["attended"], json!(false));
    assert!(
        !rendered.contains("summarise"),
        "the prompt is never journalled: {rendered}"
    );
}

#[test]
fn owner_scoped_queries_hide_other_owners() {
    let mine = json!({"client": {"uid": 1000}, "source": "clawd.request"});
    let theirs = json!({"client": {"uid": 1001}, "source": "clawd.request"});
    let root_owned = json!({"owner_uid": 0, "source": "clawd.task"});
    assert!(operation_visible_to(&mine, Some(1000)));
    assert!(!operation_visible_to(&theirs, Some(1000)));
    assert!(!operation_visible_to(&root_owned, Some(1000)));
    assert!(operation_visible_to(&theirs, None));
}
