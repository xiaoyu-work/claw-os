use super::*;

/// Mirror the EnvGuard from `import::tests` — flip COS_DATA_DIR
/// to a per-test temp dir so the dispatcher's calls into
/// `paths::models_dir()` resolve under that root.
struct EnvGuard {
    prev: Option<String>,
}
impl EnvGuard {
    fn set(dir: &std::path::Path) -> Self {
        let prev = std::env::var("COS_DATA_DIR").ok();
        std::env::set_var("COS_DATA_DIR", dir);
        Self { prev }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

#[test]
fn import_cmd_requires_args() {
    let err = run("import", &[]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn import_cmd_requires_as_flag() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::set(dir.path());
    let src = dir.path().join("x.gguf");
    std::fs::write(&src, b"X").unwrap();
    let err = run("import", &[src.display().to_string()]).unwrap_err();
    assert!(err.contains("--as"), "got {err}");
}

#[test]
fn import_cmd_rejects_unknown_flag() {
    let err = run("import", &["a.gguf".into(), "--bogus".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("unknown flag"), "got {err}");
}

#[test]
fn import_cmd_routes_into_module_with_explicit_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::set(dir.path());
    let src = dir.path().join("model.weights");
    std::fs::write(&src, b"DATA").unwrap();
    let v = run(
        "import",
        &[
            src.display().to_string(),
            "--as".into(),
            "named".into(),
            "--format".into(),
            "gguf".into(),
            "--engine".into(),
            "llama".into(),
            "--task".into(),
            "llm".into(),
        ],
    )
    .unwrap();
    assert_eq!(v["status"], "imported");
    assert_eq!(v["name"], "named");
    assert_eq!(v["engine"], "llama");
    assert_eq!(v["format"], "gguf");
    assert_eq!(v["task"], "llm");
}

#[test]
fn import_cmd_supports_version_and_force_flow() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::set(dir.path());
    let src = dir.path().join("v1.gguf");
    std::fs::write(&src, b"V1").unwrap();
    run(
        "import",
        &[
            src.display().to_string(),
            "--as".into(),
            "vt".into(),
            "--version".into(),
            "v2".into(),
        ],
    )
    .unwrap();
    // Listing should now report the model under v2.
    let listed = run("list", &[]).unwrap();
    let arr = listed["models"].as_array().unwrap();
    assert!(arr
        .iter()
        .any(|m| m["name"] == "vt" && m["version"] == "v2"));

    // Re-import without --force fails.
    let src2 = dir.path().join("v2bytes.gguf");
    std::fs::write(&src2, b"V2-BYTES").unwrap();
    let err = run(
        "import",
        &[
            src2.display().to_string(),
            "--as".into(),
            "vt".into(),
            "--version".into(),
            "v2".into(),
        ],
    )
    .unwrap_err();
    assert!(
        err.to_lowercase().contains("already registered"),
        "got {err}"
    );

    // With --force, succeeds.
    let v = run(
        "import",
        &[
            src2.display().to_string(),
            "--as".into(),
            "vt".into(),
            "--version".into(),
            "v2".into(),
            "--force".into(),
        ],
    )
    .unwrap();
    assert_eq!(v["status"], "imported");
    assert_eq!(v["size"], 8);
}

#[test]
fn rm_cmd_requires_spec() {
    let err = run("rm", &[]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn rm_cmd_rejects_missing_at_separator() {
    let err = run("rm", &["nameonly".into()]).unwrap_err();
    assert!(err.contains("@"), "got {err}");
}

#[test]
fn rm_cmd_returns_false_for_missing_model() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::set(dir.path());
    let v = run("rm", &["does-not-exist@v1".into()]).unwrap();
    assert_eq!(v["removed"], false);
    assert_eq!(v["model"], "does-not-exist@v1");
}

#[test]
fn rm_cmd_drops_existing_model() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = EnvGuard::set(dir.path());
    let src = dir.path().join("rm.gguf");
    std::fs::write(&src, b"X").unwrap();
    run(
        "import",
        &[src.display().to_string(), "--as".into(), "removable".into()],
    )
    .unwrap();
    let v = run("rm", &["removable@v1".into()]).unwrap();
    assert_eq!(v["removed"], true);
    // Listing no longer reports it.
    let listed = run("list", &[]).unwrap();
    let arr = listed["models"].as_array().unwrap();
    assert!(!arr.iter().any(|m| m["name"] == "removable"));
}

#[test]
fn parse_task_recognises_all_kinds() {
    use registry::Task;
    for (input, want) in [
        ("llm", Task::Llm),
        ("LLM", Task::Llm),
        ("stt", Task::Stt),
        ("tts", Task::Tts),
        ("embed", Task::Embed),
        ("vision", Task::Vision),
        ("imagegen", Task::Imagegen),
    ] {
        assert_eq!(parse_task(input).unwrap(), want, "input {input}");
    }
    assert!(parse_task("unknown").is_err());
}

#[test]
fn parse_engine_accepts_aliases() {
    use registry::Engine;
    assert_eq!(parse_engine("ort").unwrap(), Engine::Ort);
    assert_eq!(parse_engine("llama").unwrap(), Engine::Llama);
    assert_eq!(parse_engine("llama-cpp").unwrap(), Engine::Llama);
    assert_eq!(parse_engine("LLAMA_CPP").unwrap(), Engine::Llama);
    assert!(parse_engine("ggml").is_err());
}

#[test]
fn parse_format_recognises_canonical_names() {
    use registry::Format;
    assert_eq!(parse_format("onnx").unwrap(), Format::Onnx);
    assert_eq!(parse_format("ONNX").unwrap(), Format::Onnx);
    assert_eq!(parse_format("gguf").unwrap(), Format::Gguf);
    assert!(parse_format("safetensors").is_err());
}
