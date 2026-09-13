use super::*;
use crate::activities::{ReceiptOutcome, ReceiptReport};
use crate::operations::reporting::{self, ReceiptRecorder};
use crate::test_env::{TestEnvVarGuard, TestSessionGuard};

#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<ReceiptReport>>);

impl ReceiptRecorder for Recorder {
    fn record(&self, report: ReceiptReport) -> Result<String, String> {
        let id = report.id.clone();
        self.0.lock().unwrap().push(report);
        Ok(id)
    }
}

const SERVER: &str = r#"
import json
import os
from pathlib import Path
import sys
import time

counter = Path(os.environ["COS_DATA_DIR"]) / "calls"
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {
            "protocolVersion":request["params"]["protocolVersion"],
            "capabilities":{"tools":{}}, "serverInfo":{"name":"failure-fixture","version":"1"},
        }
    elif method == "tools/list":
        result = {"tools":[{"name":"fixture.call","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        count = int(counter.read_text()) + 1 if counter.exists() else 1
        counter.write_text(str(count))
        mode = request["params"]["arguments"]["mode"]
        if mode == "timeout":
            time.sleep(30)
        elif mode == "disconnect":
            os._exit(0)
        result = {
            "content":[{"type":"text","text":"handler refused" if mode == "error" else str(count)}],
            "isError":mode == "error",
        }
    else:
        raise AssertionError("unexpected method: " + method)
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}), flush=True)
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_calls_retire_the_real_session_without_repeating_effects() {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let apps = root.path().join("packages");
    let app = apps.join("failure-session");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("server.py"), SERVER).unwrap();
    std::fs::write(
        app.join("app.json"),
        json!({
            "id":"failure-session","version":"1","name":"Failure session",
            "operations":{},"session":{"entry":"server.py","tools":[{
                "name":"fixture.call","summary":"Exercise one controlled failure",
                "args":[{"name":"mode","kind":"text","required":true}],
            }]},
        })
        .to_string(),
    )
    .unwrap();
    crate::test_env::install_test_trust();
    crate::test_env::sign_test_package_with_entrypoints(
        &app,
        crate::provenance::PackageKind::App,
        "failure-session",
        &["server.py"],
    );
    let _apps = TestEnvVarGuard::set("COS_APPS_DIR", &apps);
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let _session = TestSessionGuard::admin(root.path());
    let _local = TestEnvVarGuard::set("COS_TEST_LOCAL_APP_SESSIONS", "1");
    let _runner = install_test_app_runner(root.path());
    let installed = crate::apps::find_verified(&apps, "failure-session").unwrap();
    let mut tool =
        AppSessionTool::from_manifest_tool(Arc::new(installed.manifest), app, apps.clone(), 0);
    tool.timeout = Duration::from_secs(1);
    let recorder = Arc::new(Recorder::default());
    let key = session_key("failure-session", &apps).unwrap();
    open_session("failure-session").await.unwrap();
    let first = manager().lock().await[&key].process_identity.clone();
    for mode in ["error", "ok"] {
        let result =
            reporting::with_recorder(Some(recorder.clone()), tool.exec(json!({"mode":mode}))).await;
        assert_eq!(result.is_error, mode == "error", "{}", result.content);
        assert_eq!(manager().lock().await[&key].process_identity, first);
    }
    for (index, mode) in ["timeout", "disconnect"].into_iter().enumerate() {
        open_session("failure-session").await.unwrap();
        let (process, session_id) = {
            let sessions = manager().lock().await;
            let session = &sessions[&key];
            (
                session.process_identity.clone(),
                session.identity.id().to_string(),
            )
        };
        let result =
            reporting::with_recorder(Some(recorder.clone()), tool.exec(json!({"mode":mode}))).await;
        assert!(result.is_error);
        assert!(
            result.content.contains("indeterminate"),
            "{}",
            result.content
        );
        assert!(!manager().lock().await.contains_key(&key));
        assert!(!process.still_matches());
        assert!(crate::proc::session_info_by_id(&session_id).is_none());
        assert_eq!(
            std::fs::read_to_string(root.path().join("apps/failure-session/calls")).unwrap(),
            (index + 3).to_string(),
            "the failing operation must run exactly once",
        );
    }
    let reports = recorder.0.lock().unwrap();
    assert_eq!(reports.len(), 4);
    for (report, outcome) in reports.iter().zip([
        ReceiptOutcome::ReportedError,
        ReceiptOutcome::Returned,
        ReceiptOutcome::Indeterminate,
        ReceiptOutcome::Indeterminate,
    ]) {
        report.validate().unwrap();
        assert_eq!(report.outcome, outcome);
    }
}
