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
fn selected_write_and_save_require_their_closed_root_dtos() {
    let write = serde_json::json!({
        "version":1,"selection_id":format!("sw-{}", "b".repeat(32)),
        "node":9,"access":"write","state":"pending",
    });
    crate::generated::validate_gui_file_write(&write).unwrap();
    let mut read = write.clone();
    read["access"] = serde_json::json!("read");
    assert!(crate::generated::validate_gui_file_write(&read).is_err());
    let mut save = serde_json::json!({
        "version":1,"reservation_id":format!("ss-{}", "c".repeat(32)),
        "selection_id":format!("sw-{}", "b".repeat(32)), "node":9, "parent_node":1,
        "name":format!(".cos-save-{}", "c".repeat(32)), "expires_in_ms":170000,
    });
    crate::generated::validate_gui_file_save(&save).unwrap();
    save["name"] = serde_json::json!("../sibling");
    assert!(crate::generated::validate_gui_file_save(&save).is_err());
    assert!(request_file_write(0).is_err());
    assert!(reserve_file_save(&format!("sr-{}", "b".repeat(32)), 9).is_err());
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

#[test]
fn create_parent_one_literal_leaf_and_closed_commit_states_are_validated() {
    let request = serde_json::json!({"parent_node":1.0,"name":"报告 ; '$x'.txt"});
    crate::generated::validate_gui_file_create_request(&request).unwrap();
    for name in ["", ".", "..", "../leaf", "a/b", "a\\b", "a\0b"] {
        assert!(request_file_create(1, name).is_err());
    }
    assert!(request_file_create(1, &"é".repeat(128)).is_err());
    assert!(request_file_create(0, "leaf").is_err());
    assert!(reserve_file_create(&format!("sw-{}", "d".repeat(32)), 1).is_err());
    let mut reply = serde_json::json!({
        "version":1,"selection_id":format!("sc-{}", "d".repeat(32)),
        "parent_node":1,"name":"leaf","access":"create","state":"selected",
    });
    crate::generated::validate_gui_file_create(&reply).unwrap();
    reply["state"] = serde_json::json!("created");
    crate::generated::validate_gui_file_create(&reply).unwrap();
    reply["owner_uid"] = serde_json::json!(0);
    assert!(crate::generated::validate_gui_file_create(&reply).is_err());
    let mut slot = serde_json::json!({
        "version":1,"reservation_id":format!("ss-{}", "e".repeat(32)),
        "selection_id":format!("sc-{}", "d".repeat(32)), "parent_node":1,
        "name":format!(".cos-save-{}", "e".repeat(32)), "target_name":"leaf","expires_in_ms":190000,
    });
    crate::generated::validate_gui_file_create_slot(&slot).unwrap();
    slot["target_name"] = serde_json::json!("../another");
    assert!(crate::generated::validate_gui_file_create_slot(&slot).is_err());
}
