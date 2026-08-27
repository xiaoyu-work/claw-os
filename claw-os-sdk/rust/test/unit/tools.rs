use super::*;

#[test]
fn call_rejects_blank_name() {
    let err = call("", &serde_json::json!({})).unwrap_err();
    assert!(matches!(err, ToolError::InvalidArg(_)));
}

#[test]
fn for_chat_passes_through() {
    let names = for_chat(["fs.read_text", "kv.get"]);
    assert_eq!(names, vec!["fs.read_text", "kv.get"]);
}

#[test]
#[cfg(unix)]
fn denial_preserves_original_structured_payload() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("cos");
    std::fs::write(
        &bin,
        "#!/bin/sh\nprintf '%s\\n' '{\"error\":\"opaque\",\"code\":\"DENIED\",\"detail\":{\"scope\":\"x\"}}'\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("CLAW_COS_BIN", &bin);
    std::env::set_var("COS_APP_ID", "notes");

    let error = call("echo", &serde_json::json!(null)).unwrap_err();
    match error {
        ToolError::Denied { payload, .. } => assert_eq!(
            payload,
            serde_json::json!({
                "error": "opaque",
                "code": "DENIED",
                "detail": {"scope": "x"}
            })
        ),
        other => panic!("expected denial, got {other:?}"),
    }

    std::env::remove_var("CLAW_COS_BIN");
    std::env::remove_var("COS_APP_ID");
}
