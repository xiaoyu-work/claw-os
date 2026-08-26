use super::*;

#[test]
fn rejects_missing_app() {
    let err = chat_cmd(&["--prompt".into(), "hi".into()]).unwrap_err();
    assert!(err.contains("--app"), "got: {err}");
}

#[test]
fn rejects_unknown_flag() {
    let err = chat_cmd(&[
        "--app".into(),
        "foo".into(),
        "--frobnicate".into(),
    ])
    .unwrap_err();
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
