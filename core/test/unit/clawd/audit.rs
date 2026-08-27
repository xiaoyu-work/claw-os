use super::*;

use serde_json::Value;

use crate::audit_policy;
use crate::clawd::protocol::{BrokerError, Response};

/// Values a broker record must never carry, whichever field they
/// arrived in.
const FORBIDDEN: &[&str] = &[
    "d34db33f-launch-handle",
    "ya29.oauth-access-token",
    "hunter2-password",
    "approval_requests",
];

fn assert_clean(rendered: &str) {
    for secret in FORBIDDEN {
        assert!(
            !rendered.contains(secret),
            "broker audit record leaked {secret}: {rendered}"
        );
    }
}

fn request_audit(command: &str, params: Value, response: &Response) -> String {
    let facts = audit_policy::request_facts(command, &params);
    let outcome = response.audit_facts();
    let audit = RequestAudit {
        ts: Utc::now(),
        event: "clawd.request",
        request: &facts,
        outcome: &outcome,
        duration_ms: 1,
        client: &ClientIdentity::unknown(),
    };
    serde_json::to_string(&audit).expect("serialize")
}

#[test]
fn launch_handles_are_never_written_to_the_audit_trail() {
    let rendered = request_audit(
        "app_session.bind",
        json!({"session_id": "app-1", "handle": "d34db33f-launch-handle", "pid": 4242}),
        &Response::ok(crate::clawd::protocol::RequestId::unknown(), json!({"bound": true})),
    );
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["command"], json!("app_session.bind"));
    assert_eq!(record["params"]["session_id"], json!("app-1"));
    assert_eq!(record["params"]["pid"], json!(4242));
    assert_eq!(record["params_omitted"], json!(1));
    assert!(
        record["params"].get("handle").is_none(),
        "the bearer field is not recorded in any form: {rendered}"
    );
}

#[test]
fn credential_material_never_reaches_the_audit_trail() {
    let rendered = request_audit(
        "credential.oauth-refresh",
        json!({
            "session": "app-1",
            "namespace": "default",
            "credential": "ya29.oauth-access-token",
            "refresh_token": "hunter2-password",
        }),
        &Response::error(crate::clawd::protocol::RequestId::unknown(), "request_failed", "credential is not eligible"),
    );
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["params"]["namespace"], json!("default"));
    assert_eq!(record["params"]["credential"], json!(audit_policy::OTHER));
    assert_eq!(record["params_omitted"], json!(1));
}

#[test]
fn handler_errors_that_echo_input_are_stored_as_class_and_digest() {
    let message = "invalid credential hunter2-password for user ada";
    let rendered = request_audit(
        "system.users.control",
        json!({"session": "app-1", "action": "create"}),
        &Response::error(crate::clawd::protocol::RequestId::unknown(), "request_failed", message),
    );
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["error"]["code"], json!("request_failed"));
    assert_eq!(record["error"]["class"], json!(audit_policy::UNCLASSIFIED));
    assert_eq!(record["error"]["message"]["bytes"], json!(message.len()));
    assert!(record["error"]["message"]["digest"].is_string());
}

#[test]
fn error_data_never_reaches_the_audit_record() {
    let rendered = request_audit(
        "app_session.register",
        json!({"app_id": "user-manager", "kind": "operation", "operation": "create-user"}),
        &Response::error_with_data(
            crate::clawd::protocol::RequestId::unknown(),
            "request_failed",
            BrokerError::with_data(
                "launcher cannot delegate sys.identity:name:accounts; awaiting approval",
                json!({"status": "approval_required", "approval_requests": ["ap-1"]}),
            )
            .classified("approval_required"),
        ),
    );
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["error"]["class"], json!("approval_required"));
    assert_eq!(record["params"]["app_id"], json!("user-manager"));
    assert_eq!(record["params"]["operation"], json!("create-user"));
}

#[test]
fn unknown_commands_are_audited_by_outcome_alone() {
    let rendered = request_audit(
        "vendor.debug.dump",
        json!({"authorization": "ya29.oauth-access-token"}),
        &Response::error_classified(
            crate::clawd::protocol::RequestId::unknown(),
            "request_failed",
            "unknown_command",
            "unknown clawd command: vendor.debug.dump",
        ),
    );
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["command"], json!(audit_policy::UNRECOGNIZED));
    assert_eq!(record["params"], json!({}));
    assert_eq!(record["params_omitted"], json!(1));
    assert_eq!(record["error"]["class"], json!("unknown_command"));
    assert!(
        !rendered.contains("vendor.debug.dump"),
        "an unrouted command name is caller text: {rendered}"
    );
}

#[test]
fn refused_frames_are_audited_as_bounded_metadata() {
    // The refused frame quoted an OAuth token. Nothing of it — not
    // verbatim, not as a digest — may reach the record.
    let facts = audit_policy::protocol_failure_facts("invalid_json", 78, None);
    let response = Response::fault(
        crate::clawd::protocol::RequestId::unknown(),
        crate::clawd::wire::Fault::MalformedBody,
    );
    let outcome = response.audit_facts();
    let audit = ProtocolFailureAudit {
        ts: Utc::now(),
        event: "clawd.protocol-failure",
        request: &facts,
        outcome: &outcome,
        duration_ms: 1,
        client: &ClientIdentity::unknown(),
    };
    let rendered = serde_json::to_string(&audit).expect("serialize");
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["class"], json!("invalid_json"));
    assert_eq!(record["bytes"], json!(78));
    assert_eq!(record["error"]["class"], json!("invalid_json"));
    assert!(
        record.get("body").is_none(),
        "the frame itself is never stored in any form"
    );
}

#[test]
fn tool_mutation_wrappers_carry_no_model_input() {
    let facts = audit_policy::tool_facts(
        "cos_app_run",
        &json!({
            "app": "email",
            "command": "send",
            "args": ["--password", "hunter2-password"],
        }),
    );
    let forward = json!({
        "tool": facts.tool,
        "tool_known": facts.known,
        "tool_use_id": audit_policy::safe_identity("toolu_01"),
        "input": facts.input,
        "input_omitted": facts.input_omitted,
        "turn_index": 3,
    });
    let rendered = serde_json::to_string(&forward).expect("serialize");
    assert_clean(&rendered);
    assert_eq!(forward["tool"], json!("cos_app_run"));
    assert_eq!(forward["input"]["app"], json!("email"));
    assert_eq!(forward["input"]["args"], json!({"type": "array", "len": 2}));
}

#[test]
fn tool_audit_records_reduce_failures_to_a_digest() {
    let message = "refused: hunter2-password";
    let audit = RuntimeToolAudit {
        ts: Utc::now(),
        event: "clawd.agent.tool.finished",
        session_id: audit_policy::safe_identity("sess-1"),
        turn_index: 2,
        tool: audit_policy::tool_facts("cos_memory", &json!({"command": "write", "content": "x"})),
        tool_use_id: audit_policy::safe_identity("toolu_01"),
        success: false,
        latency_ms: 12,
        bytes_returned: 0,
        error: audit_policy::optional_text_digest(Some(message)),
    };
    let rendered = serde_json::to_string(&audit).expect("serialize");
    assert_clean(&rendered);
    let record: Value = serde_json::from_str(&rendered).expect("parse");
    assert_eq!(record["tool"], json!("cos_memory"));
    assert_eq!(record["input"]["command"], json!("write"));
    assert_eq!(record["input_omitted"], json!(1));
    assert_eq!(record["latency_ms"], json!(12));
    assert_eq!(record["error"]["bytes"], json!(message.len()));
}
