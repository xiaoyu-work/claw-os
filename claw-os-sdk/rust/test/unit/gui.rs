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
#[cfg(target_os = "linux")]
fn overlay_ignores_executable_environment_override() {
    std::env::set_var("COS_AGENT_UI_BIN", "/nonexistent/attacker");
    let context = Context {
        app_id: "notes".into(),
        files: Vec::new(),
    };
    let error = context.open_agent_overlay(None).unwrap_err();
    std::env::remove_var("COS_AGENT_UI_BIN");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
