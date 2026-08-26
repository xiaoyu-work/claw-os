use super::*;
use std::fs;
use tempfile::tempdir;

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

#[test]
fn parses_minimal_manifest_and_substitutes_manifest_dir() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("org.tesseract.json");
    write(
        &p,
        r#"{
          "schema": "claw.agent-api/v1",
          "id": "org.tesseract",
          "name": "tesseract",
          "transport": "mcp+stdio",
          "command": "python3",
          "args": ["${manifest_dir}/main.py"],
          "env": {"PYTHONPATH": "${manifest_dir}/lib"}
        }"#,
    );
    let spec = load_manifest(&p).unwrap().expect("enabled by default");
    assert_eq!(spec.name, "tesseract");
    assert_eq!(spec.command, "python3");
    assert_eq!(
        spec.args,
        vec![format!("{}/main.py", dir.path().display())]
    );
    assert_eq!(
        spec.env.get("PYTHONPATH").unwrap(),
        &format!("{}/lib", dir.path().display())
    );
    assert_eq!(spec.timeout_secs, 30);
}

#[test]
fn rejects_unknown_schema() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("bad.json");
    write(
        &p,
        r#"{"schema":"other/v9","id":"x","name":"x","transport":"mcp+stdio","command":"true"}"#,
    );
    let err = load_manifest(&p).unwrap_err();
    assert!(matches!(err, ManifestError::Schema { .. }), "got {err:?}");
}

#[test]
fn rejects_unsupported_transport() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("bad.json");
    write(
        &p,
        r#"{"id":"x","name":"x","transport":"mcp+carrier-pigeon","command":"true"}"#,
    );
    let err = load_manifest(&p).unwrap_err();
    assert!(matches!(err, ManifestError::Transport { .. }), "got {err:?}");
}

#[test]
fn disabled_manifest_returns_none() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("off.json");
    write(
        &p,
        r#"{"id":"x","name":"x","transport":"mcp+stdio","command":"true","enabled":false}"#,
    );
    assert!(load_manifest(&p).unwrap().is_none());
}

#[test]
fn callable_by_ai_false_returns_none() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("off.json");
    write(
        &p,
        r#"{"id":"x","name":"x","transport":"mcp+stdio","command":"true",
            "ai":{"callable_by_ai":false}}"#,
    );
    assert!(load_manifest(&p).unwrap().is_none());
}

#[test]
fn discover_dedupes_by_id_first_wins() {
    let root = tempdir().unwrap();
    let high = root.path().join("high");
    let low = root.path().join("low");
    fs::create_dir_all(&high).unwrap();
    fs::create_dir_all(&low).unwrap();
    // Same `id`, different `name` so we can tell which won.
    write(
        &high.join("a.json"),
        r#"{"id":"org.thing","name":"high","transport":"mcp+stdio","command":"high-cmd"}"#,
    );
    write(
        &low.join("a.json"),
        r#"{"id":"org.thing","name":"low","transport":"mcp+stdio","command":"low-cmd"}"#,
    );
    let specs = discover_in(&[high.clone(), low.clone()]);
    assert_eq!(specs.len(), 1, "second manifest with same id ignored");
    assert_eq!(specs[0].0.name, "high");
    assert_eq!(specs[0].0.command, "high-cmd");
}

#[test]
fn discover_skips_invalid_and_keeps_valid() {
    let dir = tempdir().unwrap();
    write(&dir.path().join("good.json"),
        r#"{"id":"a","name":"a","transport":"mcp+stdio","command":"x"}"#);
    write(&dir.path().join("malformed.json"), "not json at all");
    write(
        &dir.path().join("badschema.json"),
        r#"{"schema":"other","id":"b","name":"b","transport":"mcp+stdio","command":"x"}"#,
    );
    // Hidden file is ignored even when otherwise valid.
    write(&dir.path().join(".hidden.json"),
        r#"{"id":"c","name":"c","transport":"mcp+stdio","command":"x"}"#);
    let specs = discover_in(&[dir.path().to_path_buf()]);
    let names: Vec<_> = specs.iter().map(|(s, _)| s.name.as_str()).collect();
    assert_eq!(names, vec!["a"]);
}

#[test]
fn discover_handles_missing_directory() {
    let nope = PathBuf::from("/tmp/claw-agent-api-does-not-exist-xyzzy");
    let out = discover_in(&[nope]);
    assert!(out.is_empty());
}

#[test]
fn default_search_paths_uses_xdg() {
    let home = tempdir().unwrap();
    // Save + restore env so other tests stay deterministic.
    let prev_home = std::env::var("XDG_DATA_HOME").ok();
    let prev_dirs = std::env::var("XDG_DATA_DIRS").ok();
    std::env::set_var("XDG_DATA_HOME", home.path());
    std::env::set_var("XDG_DATA_DIRS", "/opt/share:/usr/share");

    let paths = default_search_paths();
    assert!(paths.iter().any(|p| p.starts_with(home.path())));
    assert!(paths
        .iter()
        .any(|p| p == &PathBuf::from("/opt/share/claw/agent-api")));
    assert!(paths
        .iter()
        .any(|p| p == &PathBuf::from("/usr/share/claw/agent-api")));

    match prev_home {
        Some(v) => std::env::set_var("XDG_DATA_HOME", v),
        None => std::env::remove_var("XDG_DATA_HOME"),
    }
    match prev_dirs {
        Some(v) => std::env::set_var("XDG_DATA_DIRS", v),
        None => std::env::remove_var("XDG_DATA_DIRS"),
    }
}
