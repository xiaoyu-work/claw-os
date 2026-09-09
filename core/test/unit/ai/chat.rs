use super::*;

#[test]
fn summary_sdk_input_files_obey_the_exact_utf8_byte_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-prompt");
    let limit = crate::clawd::wire::bounded::FILE_TEXT_MAX_BYTES;
    assert_eq!(limit, 1_000_000);
    let mut text = "\u{00e9}".repeat(limit / 2);
    assert_eq!(text.len(), limit);
    std::fs::write(&path, &text).unwrap();
    assert_eq!(read_input_file(path.to_str().unwrap()).unwrap(), text);
    text.push('x');
    std::fs::write(&path, &text).unwrap();
    let error = read_input_file(path.to_str().unwrap()).unwrap_err();
    assert!(error.contains("exceeds the text limit"), "{error}");
}

#[test]
fn rejects_missing_app() {
    let err = chat_cmd(&["--prompt".into(), "hi".into()]).unwrap_err();
    assert!(err.contains("--app"), "got: {err}");
}

#[test]
fn rejects_unknown_flag() {
    let err = chat_cmd(&["--app".into(), "foo".into(), "--frobnicate".into()]).unwrap_err();
    assert!(err.contains("unknown flag"), "got: {err}");
}

#[test]
fn identity_rejects_unset_env() {
    let err = enforce_identity("summarize", None).unwrap_err();
    assert!(err.contains("COS_APP_ID is not set"), "got: {err}");
    assert!(err.contains("summarize"), "got: {err}");
}

#[test]
fn identity_rejects_mismatch() {
    let err = enforce_identity("summarize", Some("other-app")).unwrap_err();
    assert!(err.contains("identity mismatch"), "got: {err}");
    assert!(err.contains("--app=summarize"), "got: {err}");
    assert!(err.contains("COS_APP_ID=other-app"), "got: {err}");
}

#[test]
fn identity_accepts_exact_match() {
    assert!(enforce_identity("summarize", Some("summarize")).is_ok());
}

#[test]
fn identity_is_case_sensitive() {
    assert!(enforce_identity("summarize", Some("Summarize")).is_err());
}

#[test]
fn parse_tools_flag_basic() {
    let v = parse_tools_flag("fs.read_text,kv.get");
    assert_eq!(v, vec!["fs.read_text".to_string(), "kv.get".to_string()]);
}

#[test]
fn parse_tools_flag_trims_and_drops_empty() {
    let v = parse_tools_flag("fs.read_text,  ,kv.get , ");
    assert_eq!(v, vec!["fs.read_text".to_string(), "kv.get".to_string()]);
}

#[test]
fn parse_tools_flag_empty_string_yields_empty_vec() {
    assert!(parse_tools_flag("").is_empty());
    assert!(parse_tools_flag("  ,  ").is_empty());
}

#[test]
fn unavailable_broker_never_falls_back_to_app_local_identity_or_provider() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().unwrap();
    let _socket = crate::test_env::TestEnvVarGuard::set(
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
        dir.path().join("absent.sock"),
    );
    let _session = crate::test_env::TestEnvVarGuard::set("COS_SESSION", "test-unmounted");
    let error = chat_cmd(&["--app", "cosmic-edit", "--prompt", "document"].map(str::to_string))
        .unwrap_err();
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["code"], "KERNEL_UNAVAILABLE");
    assert!(!error["error"]
        .as_str()
        .unwrap()
        .contains("session is not registered"));
}
