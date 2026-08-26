use super::*;

#[test]
fn check_requires_verb_arg() {
    let err = check(&[]).unwrap_err();
    assert!(err.contains("usage:"));
}

#[test]
fn check_rejects_unknown_verb() {
    let err = check(&["fs.invalid".into()]).unwrap_err();
    assert!(err.contains("unknown verb"));
}

#[test]
fn check_no_scope_defaults_to_wild_and_permissive_allows() {
    let _lock = crate::test_env::lock_env();
    let prev_sess = std::env::var("COS_SESSION").ok();
    let prev_mode = std::env::var("COS_PERMS_MODE").ok();
    std::env::remove_var("COS_SESSION");
    std::env::set_var("COS_PERMS_MODE", "permissive");
    let v = check(&["ui.notify".into()]).unwrap();
    assert_eq!(v["decision"], "allow");
    restore_env("COS_SESSION", prev_sess);
    restore_env("COS_PERMS_MODE", prev_mode);
}

#[test]
fn check_with_path_scope_encodes_into_response() {
    let _lock = crate::test_env::lock_env();
    let prev_sess = std::env::var("COS_SESSION").ok();
    let prev_mode = std::env::var("COS_PERMS_MODE").ok();
    std::env::remove_var("COS_SESSION");
    std::env::set_var("COS_PERMS_MODE", "permissive");
    let v = check(&["fs.read".into(), "--path".into(), "/tmp/x".into()]).unwrap();
    assert_eq!(v["decision"], "allow");
    assert_eq!(v["verb"], "fs.read");
    assert_eq!(v["scope"]["kind"], "path");
    assert_eq!(v["scope"]["value"], "/tmp/x");
    restore_env("COS_SESSION", prev_sess);
    restore_env("COS_PERMS_MODE", prev_mode);
}

fn restore_env(name: &str, value: Option<String>) {
    match value {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}
