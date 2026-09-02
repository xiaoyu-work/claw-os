use super::*;
use crate::agent::run;

#[test]
fn notes_list_returns_dir_and_names() {
    let v = notes_cmd(&[]).expect("notes list ok");
    assert!(v.get("dir").is_some());
    assert!(v.get("notes").and_then(|x| x.as_array()).is_some());
}

// ---- semantic_cmd: clear-all guards + status drift ----

#[test]
fn semantic_clear_all_refuses_without_yes() {
    let err = semantic_cmd(&["clear-all".into()]).unwrap_err();
    assert!(
        err.contains("--yes"),
        "expected error to point at --yes, got: {err}"
    );
}

#[test]
fn semantic_no_subcommand_errs_with_usage() {
    let err = semantic_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"));
    assert!(err.contains("clear-all"));
}

// -----------------------------------------------------------------
// learn (memory curator CLI)
// -----------------------------------------------------------------

/// Pin the curator default log under a per-test temp dir so we
/// don't trample the real machine's `%ProgramData%\cos\` state.
/// Returns a guard that holds the crate-wide env lock for the
/// test's lifetime: each call mutates `COS_DATA_DIR`, and two
/// tests running in parallel would otherwise observe each
/// other's data directory (cargo test runs many threads).
/// The guard derefs to `&Path` so existing `dir.join(...)`
/// callers keep working without changes.
struct LearnDataDir {
    path: std::path::PathBuf,
    _env: std::sync::MutexGuard<'static, ()>,
}

impl std::ops::Deref for LearnDataDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl LearnDataDir {
    fn join(&self, p: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.path.join(p)
    }
}

fn isolate_cos_data_dir(tag: &str) -> LearnDataDir {
    let env = crate::test_env::lock_env();
    let dir = std::env::temp_dir().join(format!(
        "cos-learn-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("COS_DATA_DIR", &dir);
    LearnDataDir {
        path: dir,
        _env: env,
    }
}

#[test]
fn learn_cmd_extract_requires_session_flag() {
    let _dir = isolate_cos_data_dir("missing-session");
    let err = learn_cmd(&["extract".into()]).unwrap_err();
    assert!(err.contains("--session"), "got {err}");
}

#[test]
fn learn_cmd_extract_min_confidence_out_of_range_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--min-confidence".into(),
        "1.5".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--min-confidence"), "got {err}");
}

#[test]
fn learn_cmd_extract_min_confidence_not_float_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--min-confidence".into(),
        "abc".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--min-confidence"), "got {err}");
}

#[test]
fn learn_cmd_extract_limit_not_integer_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--limit".into(),
        "abc".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--limit"), "got {err}");
}

#[test]
fn learn_cmd_extract_dry_run_with_unknown_session_succeeds() {
    // dry-run skips both LLM and dedupe, and an unknown session
    // simply has zero recent messages — should be a clean
    // success envelope with empty facts.
    let _dir = isolate_cos_data_dir("dry-run-empty");
    let v = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "no-such-session".into(),
        "--dry-run".into(),
    ])
    .expect("dry-run should not fail");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["dry_run"], serde_json::Value::Bool(true));
    assert_eq!(v["messages_examined"], serde_json::json!(0));
    assert!(
        v["facts_proposed"].as_array().unwrap().is_empty(),
        "got {v}"
    );
}

#[test]
fn learn_cmd_status_default_is_empty_when_log_missing() {
    let _dir = isolate_cos_data_dir("status-empty");
    let v = learn_cmd(&["status".into()]).expect("ok");
    assert_eq!(v["session_count"], serde_json::json!(0));
    assert_eq!(v["log_exists"], serde_json::Value::Bool(false));
}

#[test]
fn learn_cmd_default_subcommand_is_status() {
    let _dir = isolate_cos_data_dir("status-default");
    let v = learn_cmd(&[]).expect("ok");
    assert!(v.get("session_count").is_some(), "got {v}");
}

#[test]
fn learn_cmd_clear_log_requires_session_or_all() {
    let _dir = isolate_cos_data_dir("clear-needs-flag");
    let err = learn_cmd(&["clear-log".into()]).unwrap_err();
    assert!(
        err.contains("--session") || err.contains("--all"),
        "got {err}"
    );
}

#[test]
fn learn_cmd_clear_log_all_writes_empty_log() {
    let dir = isolate_cos_data_dir("clear-all");
    let v = learn_cmd(&["clear-log".into(), "--all".into()]).expect("ok");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    // log file is now created on disk under the isolated dir.
    let log = dir.join("agent").join("memory").join("curation_log.json");
    assert!(log.exists(), "expected {} to exist", log.display());
}

#[test]
fn learn_cmd_clear_log_for_unknown_session_reports_zero_removed() {
    let _dir = isolate_cos_data_dir("clear-unknown");
    let v = learn_cmd(&["clear-log".into(), "--session".into(), "ghost".into()]).expect("ok");
    assert_eq!(v["removed_entries"], serde_json::json!(0));
}

#[test]
fn learn_cmd_prompt_returns_embedded_default() {
    let v = learn_cmd(&["prompt".into()]).expect("ok");
    let s = v["system_prompt"].as_str().unwrap();
    assert!(s.contains("<fact"), "prompt should mention <fact tags");
    assert!(s.contains("category"));
}

#[test]
fn run_learn_routes_to_learn_cmd() {
    // dispatcher routing through `dev` namespace — using `prompt` because it's IO-free.
    let v = run("dev", &["learn".into(), "prompt".into()]).expect("ok");
    assert!(v.get("system_prompt").is_some(), "got {v}");
}
