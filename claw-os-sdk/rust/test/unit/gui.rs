use super::*;

#[test]
fn context_from_env_decodes_files() {
    std::env::set_var("COS_APP_ID", "notes");
    std::env::set_var("COS_ARGS_JSON", r#"["/tmp/a.md","/tmp/b.md"]"#);
    let ctx = Context::from_env().unwrap();
    assert_eq!(ctx.app_id, "notes");
    assert_eq!(ctx.files, vec!["/tmp/a.md", "/tmp/b.md"]);
    std::env::remove_var("COS_APP_ID");
    std::env::remove_var("COS_ARGS_JSON");
}

#[test]
fn context_requires_app_identity() {
    std::env::remove_var("COS_APP_ID");
    std::env::remove_var("COS_ARGS_JSON");
    assert_eq!(
        Context::from_env().unwrap_err(),
        "COS_APP_ID is required for a GUI launch"
    );
}

#[test]
fn context_rejects_malformed_file_arguments() {
    std::env::set_var("COS_APP_ID", "notes");
    std::env::set_var("COS_ARGS_JSON", r#"["/tmp/a.md",7]"#);
    assert!(Context::from_env()
        .unwrap_err()
        .starts_with("COS_ARGS_JSON must be an array of strings:"));
    std::env::remove_var("COS_APP_ID");
    std::env::remove_var("COS_ARGS_JSON");
}

#[test]
fn selected_read_pending_and_closed_reply_are_not_authority() {
    let mut reply: serde_json::Value = serde_json::from_str(&format!(r#"{{
        "version":1.0,"selection_id":"sr-{}",
        "node":7e0,"access":"read","state":"pending"
    }}"#, "a".repeat(32))).unwrap();
    assert_eq!(selected_read_response("request", reply.clone()).unwrap().state, "pending");
    reply["state"] = serde_json::json!("selected");
    assert_eq!(selected_read_response("status", reply.clone()).unwrap().state, "selected");
    reply["owner_uid"] = serde_json::json!(0);
    assert!(selected_read_response("status", reply).is_err());
    assert!(request_file_read(0).is_err());
    assert!(file_read_status("another-window", 7).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn overlay_ignores_executable_environment_override() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let attacker = directory.path().join("attacker");
    let marker = directory.path().join("unexpected-execution");
    std::fs::write(
        &attacker,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&attacker, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::env::set_var("COS_AGENT_UI_BIN", &attacker);
    let context = Context {
        app_id: "notes".into(),
        files: Vec::new(),
    };
    let error = context.open_agent_overlay(None).unwrap_err();
    std::env::remove_var("COS_AGENT_UI_BIN");
    assert!(
        !marker.exists(),
        "an environment-selected program was executed"
    );
    assert!(
        error.kind() == std::io::ErrorKind::NotFound
            || (error.kind() == std::io::ErrorKind::PermissionDenied
                && error.to_string().starts_with("untrusted Ask Claw launcher")),
        "expected a missing or untrusted fixed launcher, got {error}"
    );
}
