use super::*;

#[test]
fn run_routes_known_subcommands_and_rejects_unknown() {
    let err = run("frobnicate", &[]).unwrap_err();
    assert!(err.contains("unknown command"), "got: {err}");
    assert!(err.contains("chat"), "got: {err}");
    assert!(err.contains("tool"), "got: {err}");
}

#[test]
fn tools_list_returns_catalog_as_json() {
    let v = tools_list_cmd(&[]).unwrap();
    let arr = v.get("tools").and_then(|x| x.as_array()).expect("tools array");
    assert!(!arr.is_empty(), "catalog should not be empty");
    for t in arr {
        assert!(t.get("name").and_then(|x| x.as_str()).is_some());
        assert!(t.get("verb").and_then(|x| x.as_str()).is_some());
    }
}

#[test]
fn tools_list_rejects_extra_args() {
    let err = tools_list_cmd(&["unexpected".into()]).unwrap_err();
    assert!(err.contains("no arguments"), "got: {err}");
}

#[test]
fn tool_cmd_requires_name() {
    let err = tool_cmd(&["--app".into(), "x".into()]).unwrap_err();
    assert!(err.contains("missing tool name"), "got: {err}");
}

#[test]
fn tool_cmd_requires_app() {
    let err = tool_cmd(&["fs.read_text".into()]).unwrap_err();
    assert!(err.contains("--app"), "got: {err}");
}
