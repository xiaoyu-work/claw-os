use super::*;
use crate::test_env::TestEnvVarGuard;

fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    TestEnvVarGuard,
    TestEnvVarGuard,
    Value,
) {
    let data = tempfile::tempdir().unwrap();
    let apps = tempfile::tempdir().unwrap();
    let data_guard = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let apps_guard = TestEnvVarGuard::set("COS_APPS_DIR", apps.path());
    let dir = apps.path().join("demo");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("app.json"), json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{"get":{"label":{"en":"Get"},"args":[{"name":"key","kind":"name","required":true}]}}
    }).to_string()).unwrap();
    std::fs::write(
        dir.join("main.py"),
        "raise AssertionError('test executor only')\n",
    )
    .unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "demo");
    let activity =
        crate::clawd::activities::create(json!({"title":"Goal","goal":"Read key"}), &peer())
            .unwrap();
    (data, apps, data_guard, apps_guard, activity)
}

fn peer() -> crate::clawd::client_identity::ClientIdentity {
    crate::clawd::client_identity::ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(1000),
        gid: Some(1000),
        start_time_ticks: Some(1),
    }
}

fn broker(command: Command, params: Value) -> Result<Value, String> {
    match command {
        Command::ActivityGet => {
            crate::clawd::activities::get(params, &peer()).map_err(|e| e.to_string())
        }
        Command::ActivityReceiptRecord => {
            crate::clawd::activity_receipts::record(params, &peer()).map_err(|e| e.to_string())
        }
        _ => panic!("unexpected command"),
    }
}

fn invocation(activity: &Value) -> Vec<String> {
    vec![
        "demo".into(),
        "get".into(),
        "--activity".into(),
        activity["id"].as_str().unwrap().into(),
        "--".into(),
        "--".into(),
        "--schema".into(),
    ]
}

#[test]
fn operation_execution_captures_the_normal_app_result_without_completing_the_activity() {
    let _lock = crate::test_env::lock_env();
    let (_data, _apps, _dg, _ag, activity) = fixture();
    let receipt = execute_with(&invocation(&activity), broker, |app, operation, args| {
        assert!(app.is_verified());
        assert_eq!(operation, "get");
        assert_eq!(args, ["--", "--schema"]);
        Ok(Some(r#"{"value":"ready"}"#.into()))
    })
    .unwrap();
    assert_eq!(receipt["source"], "caller_reported");
    assert_eq!(receipt["report"]["outcome"], "returned");
    let current = broker(Command::ActivityGet, json!({"id":activity["id"]})).unwrap();
    assert_eq!(current["activity"]["state"], "active");
}

#[test]
fn inactive_activity_blocks_execution_and_app_errors_remain_errors() {
    let _lock = crate::test_env::lock_env();
    let (_data, _apps, _dg, _ag, activity) = fixture();
    crate::clawd::activities::transition(json!({"id":activity["id"],"state":"paused"}), &peer())
        .unwrap();
    assert!(
        execute_with(&invocation(&activity), broker, |_, _, _| panic!(
            "executed paused activity"
        ))
        .is_err()
    );
    crate::clawd::activities::transition(json!({"id":activity["id"],"state":"active"}), &peer())
        .unwrap();
    let error = execute_with(&invocation(&activity), broker, |_, _, _| {
        Err("normal App policy refused".into())
    })
    .unwrap_err();
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["receipt"]["report"]["outcome"], "indeterminate");
    assert!(error.get("error").is_some());
}

#[test]
fn recording_failure_returns_a_retryable_report_not_a_false_success_or_reexecution() {
    let _lock = crate::test_env::lock_env();
    let (_data, _apps, _dg, _ag, activity) = fixture();
    let mut executions = 0;
    let error = execute_with(
        &invocation(&activity),
        |command, params| {
            if command == Command::ActivityReceiptRecord {
                Err("broker disconnected".into())
            } else {
                broker(command, params)
            }
        },
        |_, _, _| {
            executions += 1;
            Ok(Some("result".into()))
        },
    )
    .unwrap_err();
    assert_eq!(executions, 1);
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["report"]["outcome"], "returned");
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("do not re-execute"));
    assert!(error["retry"].as_str().unwrap().contains("record-receipt"));
}
