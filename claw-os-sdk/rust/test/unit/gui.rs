use super::*;

#[test]
fn gui_command_constant_matches_default_exec() {
    assert_eq!(GUI_COMMAND, "--gui");
}

#[test]
fn context_from_env_decodes_files() {
    std::env::set_var("COS_APP_ID", "notes");
    std::env::set_var("COS_ARGS_JSON", r#"["/tmp/a.md","/tmp/b.md"]"#);
    let ctx = Context::from_env();
    assert_eq!(ctx.app_id, "notes");
    assert_eq!(ctx.files, vec!["/tmp/a.md", "/tmp/b.md"]);
    std::env::remove_var("COS_APP_ID");
    std::env::remove_var("COS_ARGS_JSON");
}

#[test]
fn context_defaults_when_env_absent() {
    std::env::remove_var("COS_APP_ID");
    std::env::remove_var("COS_ARGS_JSON");
    let ctx = Context::from_env();
    assert_eq!(ctx.app_id, "unknown");
    assert!(ctx.files.is_empty());
}
