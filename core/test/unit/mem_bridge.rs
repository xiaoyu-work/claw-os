use super::*;

#[test]
fn remember_requires_json_arg() {
    let err = remember(&[]).unwrap_err();
    assert!(err.contains("missing --json"));
}

#[test]
fn list_rejects_unknown_flag() {
    let err = list(&["--bogus".into()]).unwrap_err();
    assert!(err.contains("unexpected"));
}

#[test]
fn search_requires_query() {
    let err = search(&[]).unwrap_err();
    assert!(err.contains("missing <query>"));
}

#[test]
fn forget_requires_exactly_one_target() {
    let err = forget(&[]).unwrap_err();
    assert!(err.contains("exactly one"));
    let err = forget(&[
        "--source".into(),
        "expense-tracker".into(),
        "--row".into(),
        "1".into(),
    ])
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn run_unknown_command() {
    let err = run("nope", &[]).unwrap_err();
    assert!(err.contains("unknown internal memory command"));
}
