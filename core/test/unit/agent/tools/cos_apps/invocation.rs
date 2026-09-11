use super::*;
use crate::activities::{ReceiptOutcome, ReceiptReport};
use crate::operations::reporting::ReceiptRecorder;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Recorder {
    reports: Mutex<Vec<ReceiptReport>>,
    failure: Option<String>,
    wrong_id: bool,
}

impl ReceiptRecorder for Recorder {
    fn record(&self, report: ReceiptReport) -> Result<String, String> {
        let id = report.id.clone();
        self.reports.lock().unwrap().push(report);
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        Ok(if self.wrong_id {
            uuid::Uuid::new_v4().to_string()
        } else {
            id
        })
    }
}

fn launch() -> (tempfile::TempDir, AppLaunch) {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("demo");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        serde_json::json!({
            "id":"demo","version":"1","name":"Demo",
            "operations":{"run":{"label":"Run"}}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        app.join("main.py"),
        "raise AssertionError('result capture must not execute an App')\n",
    )
    .unwrap();
    let launch = crate::test_env::app_launch(&app, "demo");
    (root, launch)
}

#[tokio::test]
async fn receipts_preserve_original_tool_results_and_reported_outcomes() {
    let _lock = crate::test_env::lock_env();
    let (_root, launch) = launch();
    let recorder = Arc::new(Recorder::default());
    for (result, expected_content, is_error, outcome) in [
        (
            Ok(Some(" exact output \n".into())),
            " exact output \n",
            false,
            ReceiptOutcome::Returned,
        ),
        (
            Ok(Some(r#"{"error":"App refused"}"#.into())),
            r#"{"error":"App refused"}"#,
            false,
            ReceiptOutcome::ReportedError,
        ),
        (Ok(None), "", false, ReceiptOutcome::Returned),
        (
            Err("launch was refused".into()),
            "launch was refused",
            true,
            ReceiptOutcome::Indeterminate,
        ),
    ] {
        let output =
            reporting::with_recorder(Some(recorder.clone()), finish(&launch, "run", result)).await;
        assert_eq!(output.content, expected_content);
        assert_eq!(output.is_error, is_error);
        let reports = recorder.reports.lock().unwrap();
        let report = reports.last().unwrap();
        report.validate().unwrap();
        assert_eq!(report.outcome, outcome);
        assert_eq!(report.app_id, "demo");
        assert_eq!(report.operation, "run");
        assert_eq!(report.package_digest, launch.package().content_digest());
        if let Some(result) = &report.result {
            assert_eq!(result.bytes, expected_content.len() as u64);
            assert_eq!(
                result.sha256,
                format!(
                    "sha256:{}",
                    crate::crypto::sha256_hex(expected_content.as_bytes())
                )
            );
        }
    }
    assert_eq!(recorder.reports.lock().unwrap().len(), 4);
    assert!(reporting::current().is_none());
}

#[tokio::test]
async fn schema_unknown_operations_and_unassociated_runs_do_not_record() {
    let _lock = crate::test_env::lock_env();
    let (_root, launch) = launch();
    let recorder = Arc::new(Recorder::default());
    for operation in ["__schema__", "not_declared"] {
        let output = reporting::with_recorder(
            Some(recorder.clone()),
            finish(&launch, operation, Ok(Some("original".into()))),
        )
        .await;
        assert_eq!(output.content, "original");
    }
    reporting::with_recorder(
        Some(recorder.clone()),
        reporting::with_recorder(None, finish(&launch, "run", Ok(None))),
    )
    .await;
    finish(&launch, "run", Ok(None)).await;
    assert!(recorder.reports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recording_failures_keep_the_result_and_return_only_a_retryable_report() {
    let _lock = crate::test_env::lock_env();
    let (_root, launch) = launch();
    let secret = format!("ghp_{}", "x".repeat(36));
    for recorder in [
        Arc::new(Recorder {
            failure: Some(format!("recording unavailable: {secret}")),
            ..Default::default()
        }),
        Arc::new(Recorder {
            wrong_id: true,
            ..Default::default()
        }),
    ] {
        let output = reporting::with_recorder(
            Some(recorder.clone()),
            finish(&launch, "run", Ok(Some("the operation returned".into()))),
        )
        .await;
        assert!(output.is_error);
        assert!(output.content.starts_with("the operation returned\n"));
        assert!(output.content.contains("Do not repeat the App operation"));
        assert!(!output.content.contains(&secret));
        let retry: ReceiptReport =
            serde_json::from_str(output.content.lines().last().unwrap()).unwrap();
        retry.validate().unwrap();
        let reports = recorder.reports.lock().unwrap();
        assert_eq!(reports.as_slice(), &[retry]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn both_gateways_run_the_declared_runtime_and_never_repeat_for_recording() {
    use crate::agent::tools::cos_apps::{CosAppRun, CosAppTool, APPS_ROOT_OVERRIDE};
    use crate::agent::tools::Tool;
    use crate::test_env::{TestEnvVarGuard, TestSessionGuard};
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::test_env::lock_env();
    let state = tempfile::tempdir().unwrap();
    let apps = state.path().join("packages");
    let app = apps.join("demo");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        serde_json::json!({
            "id":"demo","version":"1","name":"Demo","runtime":"shell",
            "operations":{"run":{"label":"Run"}}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        app.join("main.sh"),
        "#!/bin/sh\nprintf 'run\\n' >> \"$COS_DATA_DIR/invocations\"\nprintf '{\"runtime\":\"shell\"}\\n'\n",
    )
    .unwrap();
    let _launch = crate::test_env::app_launch(&app, "demo");
    let _session = TestSessionGuard::admin(state.path());
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", state.path());
    let runner = state.path().join("claw-app-runner");
    std::fs::write(
        &runner,
        "#!/bin/sh\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _runner = TestEnvVarGuard::set("CLAW_APP_RUNNER_BIN", &runner);
    let recorder = Arc::new(Recorder::default());
    let typed = CosAppTool::new("cos_app_demo", "demo", "Demo", &["run"]);
    for generic in [false, true] {
        let output = reporting::with_recorder(
            Some(recorder.clone()),
            APPS_ROOT_OVERRIDE.scope(apps.clone(), async {
                if generic {
                    CosAppRun
                        .exec(serde_json::json!({"app":"demo","command":"run"}))
                        .await
                } else {
                    typed.exec(serde_json::json!({"command":"run"})).await
                }
            }),
        )
        .await;
        assert!(!output.is_error, "{}", output.content);
        let result: serde_json::Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(result["runtime"], "shell");
    }
    assert_eq!(recorder.reports.lock().unwrap().len(), 2);
    let failed = Arc::new(Recorder {
        failure: Some("receipt store offline".into()),
        ..Default::default()
    });
    let output = reporting::with_recorder(
        Some(failed.clone()),
        APPS_ROOT_OVERRIDE.scope(
            apps,
            CosAppRun.exec(serde_json::json!({"app":"demo","command":"run"})),
        ),
    )
    .await;
    assert!(output.is_error);
    assert!(output.content.contains("Do not repeat"));
    assert_eq!(failed.reports.lock().unwrap().len(), 1);
    let executions = std::fs::read_to_string(state.path().join("apps/demo/invocations")).unwrap();
    assert_eq!(executions.lines().count(), 3);
}
