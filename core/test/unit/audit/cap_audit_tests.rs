use super::*;
use serde_json::json;

/// Points both durable capability-decision sinks at one temp dir:
/// `caps.jsonl` follows `COS_LOG_DIR`, the system-operations journal
/// follows `COS_DATA_DIR`. Field order matters — the env guards
/// restore the previous values on drop and the shared env lock is
/// declared last so it is released only after that restore.
struct SinkDirs {
    _dir: tempfile::TempDir,
    _log_dir: crate::test_env::TestEnvVarGuard,
    _data_dir: crate::test_env::TestEnvVarGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl SinkDirs {
    fn new() -> Self {
        let lock = crate::test_env::lock_env();
        let dir = tempfile::tempdir().unwrap();
        let log_dir = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", dir.path());
        let data_dir = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
        Self {
            _dir: dir,
            _log_dir: log_dir,
            _data_dir: data_dir,
            _lock: lock,
        }
    }
}

fn caps_log_lines() -> Vec<serde_json::Value> {
    let path = crate::paths::caps_audit_log_path();
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    body.lines()
        .map(|line| serde_json::from_str(line).expect("caps.jsonl line is JSON"))
        .collect()
}

fn journal_cap_decisions() -> Vec<serde_json::Value> {
    let path = crate::paths::system_operations_log_path();
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    body.lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("journal line is JSON"))
        .filter(|record| record["source"] == json!("caps.require"))
        .collect()
}

#[test]
fn log_cap_decision_writes_under_log_dir() {
    let _sinks = SinkDirs::new();

    log_cap_decision(json!({
        "session_id": "s-1",
        "verb": "fs.read",
        "decision": "allow",
    }));

    let lines = caps_log_lines();
    assert_eq!(lines.len(), 1, "expected one audit line, got {lines:?}");
    assert_eq!(lines[0]["session_id"], json!("s-1"));
    assert_eq!(lines[0]["verb"], json!("fs.read"));
    assert!(lines[0]["timestamp"].is_string());
}

/// Regression: `log_cap_decision` used to return early when the
/// caller's environment carried `COS_CAPS_AUDIT=0`. That handed the
/// process under enforcement a switch for its own audit trail — it
/// could drop its denials and keep running. The variable must now be
/// inert, and both decision classes must reach both sinks.
#[test]
fn env_var_cannot_suppress_cap_decisions() {
    let _sinks = SinkDirs::new();
    let _flag = crate::test_env::TestEnvVarGuard::set("COS_CAPS_AUDIT", "0");

    log_cap_decision(json!({
        "session_id": "s-1",
        "verb": "fs.read",
        "decision": "allow",
    }));
    log_cap_decision(json!({
        "session_id": "s-1",
        "verb": "fs.delete",
        "decision": "deny",
        "reason": "verb-not-granted",
    }));

    let lines = caps_log_lines();
    assert_eq!(lines.len(), 2, "expected two audit lines, got {lines:?}");
    assert_eq!(lines[0]["decision"], json!("allow"));
    assert_eq!(lines[0]["verb"], json!("fs.read"));
    assert_eq!(lines[1]["decision"], json!("deny"));
    assert_eq!(lines[1]["verb"], json!("fs.delete"));
    assert_eq!(lines[1]["reason"], json!("verb-not-granted"));

    let journal = journal_cap_decisions();
    assert_eq!(
        journal.len(),
        2,
        "expected both decisions in the system journal, got {journal:?}"
    );
    assert_eq!(journal[0]["decision"], json!("allow"));
    assert_eq!(journal[0]["ok"], json!(true));
    assert_eq!(journal[1]["decision"], json!("deny"));
    assert_eq!(journal[1]["ok"], json!(false));
}

/// The audited process controls its whole environment, so no value
/// of `COS_CAPS_AUDIT` may change what is recorded.
#[test]
fn no_value_of_the_legacy_env_var_changes_recording() {
    for value in ["0", "false", "off", "no", "", "1"] {
        let _sinks = SinkDirs::new();
        let _flag = crate::test_env::TestEnvVarGuard::set("COS_CAPS_AUDIT", value);

        log_cap_decision(json!({
            "session_id": "s-1",
            "verb": "fs.delete",
            "decision": "deny",
        }));

        let lines = caps_log_lines();
        assert_eq!(
            lines.len(),
            1,
            "COS_CAPS_AUDIT={value:?} changed recording: {lines:?}"
        );
        assert_eq!(lines[0]["decision"], json!("deny"));
    }
}
