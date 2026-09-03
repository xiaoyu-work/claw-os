use super::*;

/// Values that must never reach a durable record, whatever route or
/// tool carried them.
const FORBIDDEN: &[&str] = &[
    "d34db33f-launch-handle",
    "ya29.oauth-access-token",
    "4/0AX4XfWh-authorization-code",
    "hunter2-password",
    "sk-live-provider-key",
    "restore-bearer-token",
];

fn assert_no_secrets(rendered: &str) {
    for secret in FORBIDDEN {
        assert!(
            !rendered.contains(secret),
            "durable record leaked {secret}: {rendered}"
        );
    }
}

fn render<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize")
}

#[test]
fn unknown_commands_contribute_no_name_and_no_arguments() {
    let command = "totally.new.route";
    let facts = request_facts(
        command,
        &json!({"token": "restore-bearer-token", "nested": {"a": {"b": 1}}}),
    );
    assert_eq!(facts.command, UNRECOGNIZED);
    assert_eq!(facts.params, json!({}));
    assert_eq!(facts.params_omitted, 2);
    let rendered = render(&facts);
    assert_no_secrets(&rendered);
    assert!(
        !rendered.contains(command),
        "an unrouted command name is caller text: {rendered}"
    );
    assert!(
        facts
            .command_text
            .is_some_and(|text| text.bytes == command.len()),
        "the unknown name is still countable"
    );
}

#[test]
fn app_session_handles_never_reach_a_record() {
    for command in [
        "app_session.bind",
        "app_session.set_transient",
        "app_session.deregister",
    ] {
        let facts = request_facts(
            command,
            &json!({
                "session_id": "app-1",
                "handle": "d34db33f-launch-handle",
                "pid": 4242,
            }),
        );
        let rendered = render(&facts);
        assert_no_secrets(&rendered);
        assert!(
            !rendered.contains("handle"),
            "{command} must not name the bearer field at all: {rendered}"
        );
        assert_eq!(facts.params["session_id"], json!("app-1"));
        assert_eq!(
            facts.params_omitted,
            if command == "app_session.bind" { 1 } else { 2 }
        );
    }
}

#[test]
fn app_session_bind_keeps_the_child_pid() {
    let facts = request_facts(
        "app_session.bind",
        &json!({"session_id": "app-1", "handle": "d34db33f-launch-handle", "pid": 4242}),
    );
    assert_eq!(facts.params["pid"], json!(4242));
}

#[test]
fn oauth_refresh_records_only_the_enumerated_credential() {
    let allowed = request_facts(
        "credential.oauth-refresh",
        &json!({
            "session": "app-1",
            "namespace": "default",
            "credential": "GOOGLE_ACCESS_TOKEN",
        }),
    );
    assert_eq!(allowed.params["credential"], json!("GOOGLE_ACCESS_TOKEN"));
    assert_eq!(allowed.params["namespace"], json!("default"));

    let smuggled = request_facts(
        "credential.oauth-refresh",
        &json!({
            "session": "app-1",
            "namespace": "default",
            "credential": "ya29.oauth-access-token",
            "code": "4/0AX4XfWh-authorization-code",
        }),
    );
    assert_eq!(smuggled.params["credential"], json!(OTHER));
    assert_eq!(smuggled.params_omitted, 1);
    assert_no_secrets(&render(&smuggled));
}

#[test]
fn scheduler_arguments_are_counted_not_stored() {
    let facts = request_facts(
        "scheduler.run",
        &json!({
            "subsystem": "cron",
            "command": "add",
            "args": ["--id", "nightly", "--credential", "hunter2-password"],
        }),
    );
    assert_eq!(facts.params["subsystem"], json!("cron"));
    assert_eq!(facts.params["command"], json!("add"));
    assert_eq!(facts.params["args"], json!({"type": "array", "len": 4}));
    assert_no_secrets(&render(&facts));
}

#[test]
fn scheduler_subsystem_outside_the_enumeration_is_not_echoed() {
    let facts = request_facts(
        "scheduler.run",
        &json!({"subsystem": "sk-live-provider-key", "command": "add"}),
    );
    assert_eq!(facts.params["subsystem"], json!(OTHER));
    assert_no_secrets(&render(&facts));
}

#[test]
fn config_and_package_routes_keep_only_their_resource() {
    let config = request_facts(
        "system.config.control",
        &json!({
            "session": "app-1",
            "action": "restore",
            "target": "/etc/ssh/sshd_config",
            "token": "restore-bearer-token",
            "confirm": true,
        }),
    );
    assert_eq!(config.params["target"], json!("/etc/ssh/sshd_config"));
    assert_eq!(config.params["confirm"], json!(true));
    assert_eq!(config.params_omitted, 1);
    assert_no_secrets(&render(&config));

    let package = request_facts(
        "system.package.install",
        &json!({"session": "app-1", "action": "install", "package": "nginx", "version": "1.24.0-2"}),
    );
    assert_eq!(package.params["package"], json!("nginx"));
    assert_eq!(package.params["version"], json!("1.24.0-2"));
    assert_eq!(package.params_omitted, 0);
}

#[test]
fn user_routes_drop_personal_and_credential_fields() {
    let facts = request_facts(
        "system.users.control",
        &json!({
            "session": "app-1",
            "action": "create",
            "user": "ada",
            "full_name": "Ada Lovelace",
            "credential": "hunter2-password",
            "token": "restore-bearer-token",
        }),
    );
    assert_eq!(facts.params["user"], json!("ada"));
    assert_eq!(facts.params_omitted, 3);
    let rendered = render(&facts);
    assert_no_secrets(&rendered);
    assert!(!rendered.contains("Ada Lovelace"), "{rendered}");
}

#[test]
fn nested_caller_objects_are_never_walked() {
    let facts = request_facts(
        "app_session.set_transient",
        &json!({
            "session_id": "app-1",
            "handle": "d34db33f-launch-handle",
            "call": {"tool": "send", "arguments": {"authorization": "sk-live-provider-key"}},
        }),
    );
    assert_eq!(facts.params["call"], json!({"type": "object", "len": 2}));
    assert_no_secrets(&render(&facts));
}

#[test]
fn values_that_fail_their_rule_become_a_shape_not_a_truncation() {
    let facts = request_facts(
        "task.get",
        &json!({"id": "sk-live-provider-key with spaces and /slashes"}),
    );
    assert_eq!(facts.params["id"], json!(UNLOGGABLE));
    assert_no_secrets(&render(&facts));

    let wrong_type = request_facts("task.get", &json!({"id": ["a", "b"]}));
    assert_eq!(wrong_type.params["id"], json!({"type": "array", "len": 2}));
}

#[test]
fn non_object_params_are_recorded_as_a_kind_and_a_count() {
    let facts = request_facts("task.count", &json!("sk-live-provider-key"));
    assert_eq!(facts.params, json!({}));
    assert_eq!(facts.params_kind, "string");
    assert_eq!(facts.params_omitted, 1);
    assert_no_secrets(&render(&facts));

    let empty = request_facts("task.count", &Value::Null);
    assert_eq!(empty.params_kind, "null");
    assert_eq!(empty.params_omitted, 0);
}

#[test]
fn refused_frames_are_described_never_stored() {
    // The frame that produced this refusal quoted a credential. What
    // survives is the stable class, the byte count, and — only when a
    // route was actually resolved — the registry's own static name.
    let facts = protocol_failure_facts("invalid_json", 4096, None);
    assert_eq!(facts.class, "invalid_json");
    assert_eq!(facts.bytes, 4096);
    assert!(facts.command.is_none());
    assert_no_secrets(&render(&facts));

    let named = protocol_failure_facts("invalid_params", 128, Some("credential.oauth-refresh"));
    assert_eq!(named.command, Some("credential.oauth-refresh"));
    assert_no_secrets(&render(&named));
}

#[test]
fn identical_bodies_share_a_digest_and_different_ones_do_not() {
    let first = text_digest("ya29.oauth-access-token");
    let same = text_digest("ya29.oauth-access-token");
    let other = text_digest("ya29.oauth-access-tokem");
    assert_eq!(first.digest, same.digest);
    assert_ne!(first.digest, other.digest);
}

#[test]
fn handler_messages_are_reduced_to_a_class_and_a_digest() {
    let echoed = "unknown clawd command: sk-live-provider-key";
    let unclassified = error_facts("request_failed", None, echoed);
    assert_eq!(unclassified.class, UNCLASSIFIED);
    assert_eq!(unclassified.message.bytes, echoed.len());
    assert_no_secrets(&render(&unclassified));

    let classified = error_facts("request_failed", Some("unknown_command"), echoed);
    assert_eq!(classified.class, "unknown_command");
    assert_no_secrets(&render(&classified));
}

#[test]
fn error_codes_are_bounded_too() {
    let facts = error_facts("sk-live-provider-key is not a code", None, "boom");
    assert_eq!(facts.code, UNLOGGABLE);
    assert_no_secrets(&render(&facts));
}

#[test]
fn unknown_tools_contribute_no_input() {
    let facts = tool_facts(
        "mcp__vendor__send_mail",
        &json!({"to": "ada@example.com", "body": "sk-live-provider-key"}),
    );
    assert!(!facts.known);
    assert_eq!(facts.tool, "mcp__vendor__send_mail");
    assert_eq!(facts.input, json!({}));
    assert_eq!(facts.input_omitted, 2);
    assert_no_secrets(&render(&facts));
}

#[test]
fn typed_app_tools_do_not_persist_caller_defined_fields() {
    let facts = tool_facts(
        "app_email__email_send",
        &json!({
            "to": "recipient@example.com",
            "body": "hunter2-password",
        }),
    );
    assert!(!facts.known);
    assert_eq!(facts.input, json!({}));
    assert_eq!(facts.input_omitted, 2);
    assert_no_secrets(&render(&facts));
}

#[test]
fn discovery_and_usage_tools_log_only_bounded_shapes() {
    let help = tool_facts(
        "cos_help",
        &json!({"path": ["agent", "usage"], "unexpected": "secret"}),
    );
    assert!(help.known);
    assert_eq!(help.input["path"], json!({"type": "array", "len": 2}));
    assert_eq!(help.input_omitted, 1);

    let usage = tool_facts(
        "cos_usage",
        &json!({"command": "session", "args": ["private-session-id"]}),
    );
    assert!(usage.known);
    assert_eq!(usage.input["command"], "session");
    assert_eq!(usage.input["args"], json!({"type": "array", "len": 1}));
    assert_no_secrets(&render(&help));
    assert_no_secrets(&render(&usage));
}

#[test]
fn model_authored_text_is_reduced_to_a_byte_count() {
    let facts = tool_facts(
        "cos_imagegen",
        &json!({"prompt": "sk-live-provider-key", "provider": "noop", "n": 2}),
    );
    assert_eq!(
        facts.input["prompt"],
        json!({"type": "string", "bytes": 20})
    );
    assert_eq!(facts.input["provider"], json!("noop"));
    assert_eq!(facts.input["n"], json!(2));
    assert_no_secrets(&render(&facts));
}

#[test]
fn hostile_tool_names_are_bounded() {
    let facts = tool_facts("../../etc/shadow ya29.oauth-access-token", &json!({}));
    assert_eq!(facts.tool, UNLOGGABLE);
    assert_no_secrets(&render(&facts));
}

#[test]
fn no_policy_field_is_a_known_secret_carrier() {
    // The allowlist is the mechanism, but a typo that admitted one of
    // these names would be silent, so name them once here.
    const NEVER: &[&str] = &[
        "handle",
        "token",
        "password",
        "secret",
        "authorization",
        "code",
        "payload",
        "metadata",
        "content",
        "parent_caps",
        "reason",
        "note",
        "full_name",
    ];
    for route in crate::clawd::routes::ROUTES {
        for (field, rule) in route.audit_fields {
            assert!(
                !NEVER.contains(field) || matches!(rule, FieldRule::Size),
                "{}.{field} is allowlisted as a value",
                route.name
            );
        }
    }
    for policy in TOOL_POLICIES {
        for (field, rule) in policy.fields {
            assert!(
                !NEVER.contains(field) || matches!(rule, FieldRule::Size),
                "tool {}.{field} is allowlisted as a value",
                policy.tool
            );
        }
    }
}
