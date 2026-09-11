use super::super::*;

use crate::agent::tools::{cos_apps::CosAppRun, Tool};
use crate::operations::reporting::{self, ReceiptRecorder};
use serde_json::{json, Value};

mod support {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/process_support.rs"
    ));
}

use support::{ProcessContext, APP_ID, BODY, CHILD_TEST, CONTEXT_ENV, OPERATION};

mod stateful {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/worker_stateful.rs"
    ));
}

struct CapturedReceipt {
    channel: Arc<ChannelReceiptRecorder>,
    report: Arc<Mutex<Vec<crate::activities::ReceiptReport>>>,
}

impl ReceiptRecorder for CapturedReceipt {
    fn record(&self, report: crate::activities::ReceiptReport) -> Result<String, String> {
        let id = self.channel.record(report.clone())?;
        let mut captured = self
            .report
            .lock()
            .map_err(|_| "test receipt lock poisoned")?;
        assert!(
            captured.iter().all(|saved| saved.id != report.id),
            "the App invocation was reported more than once"
        );
        captured.push(report);
        Ok(id)
    }
}

#[test]
#[ignore = "child entry; only the root controlled-host process regression may launch it"]
fn controlled_host_child() {
    assert!(std::env::args().any(|arg| arg == CHILD_TEST));
    let context_path = std::env::var_os(CONTEXT_ENV).expect("root-owned process fixture context");
    let context: ProcessContext =
        serde_json::from_slice(&std::fs::read(context_path).unwrap()).unwrap();
    context.install_test_trust();
    assert_eq!(std::env::current_dir().unwrap(), context.home());
    assert_eq!(
        std::fs::read_link("/proc/self/ns/mnt")
            .unwrap()
            .to_str()
            .unwrap(),
        context.mount_namespace,
    );
    assert_eq!(
        std::env::var_os("COS_APPS_DIR").map(std::path::PathBuf::from),
        Some(context.apps()),
    );
    assert!(!crate::agentd::guard::is_broker_process());
    let identity = LocalIdentity::read().unwrap();
    identity.require_expected_identity(context.uid).unwrap();
    assert_eq!((identity.gid, identity.egid), (context.gid, context.gid));
    assert!(identity.groups.is_empty());
    assert!(identity.no_new_privs);
    assert!(identity.start_time_ticks.is_some());
    let availability = crate::worker::availability();
    assert!(availability.is_available(), "{availability:?}");

    let io = ChannelIo::start(adopt_channel().unwrap(), identity.clone()).unwrap();
    let assignment = io.handshake().unwrap();
    assert_eq!(assignment.job.owner_uid, context.uid);
    assert_eq!(assignment.job.owner_home, context.home().to_str().unwrap());
    assert!(assignment.job.record_activity_receipts);
    assert!(assignment.session.is_some());
    let task_id = assignment.job.id.clone();
    crate::caps::approval_gateway::install(Arc::new(ChannelApprovalGateway {
        task_id: task_id.clone(),
        state: io.state.clone(),
    }));
    crate::clawd::client::install_gateway(Arc::new(ChannelAppGateway {
        task_id: task_id.clone(),
        state: io.state.clone(),
    }))
    .unwrap();
    let channel = Arc::new(ChannelReceiptRecorder {
        task_id: task_id.clone(),
        state: io.state.clone(),
    });
    let captured = Arc::new(Mutex::new(Vec::new()));
    let recorder: Arc<dyn ReceiptRecorder> = Arc::new(CapturedReceipt {
        channel: channel.clone(),
        report: captured.clone(),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(crate::paths::with_routed_job(
        crate::paths::with_user_override(
            context.uid,
            context.home(),
            with_session(
                assignment.session,
                reporting::with_recorder(Some(recorder), async {
                    let session = crate::proc::current_session_info_for_caps().unwrap();
                    assert_eq!(session.pid, std::process::id());
                    if context.stateful {
                        stateful::execute(&context).await
                    } else {
                        CosAppRun
                            .exec(json!({
                                "app": APP_ID,
                                "command": OPERATION,
                                "args": [context.input()],
                            }))
                            .await
                    }
                }),
            ),
        ),
    ));
    let outcome = if result.is_error {
        WorkerOutcome::Error {
            message: result.content,
        }
    } else {
        let output: Value = serde_json::from_str(&result.content).expect("real App JSON output");
        let expected_calls = if context.stateful { 2 } else { 1 };
        if !context.stateful {
            assert_eq!(output["body"], BODY);
            assert_eq!(output["invocations"], 1);
        }
        assert_eq!(
            std::fs::read_to_string(context.app_data().join("counter")).unwrap(),
            expected_calls.to_string(),
        );
        let reports = captured.lock().unwrap().clone();
        assert_eq!(reports.len(), expected_calls);
        let report = reports[0].clone();
        let receipt_ids: Vec<_> = reports.iter().map(|report| report.id.clone()).collect();
        let first_id = report.id.clone();
        // Retry only the original report over the real channel, not CosAppRun.
        let retried_id = channel.record(report).expect("record-only retry");
        assert_eq!(first_id, retried_id);
        assert_eq!(
            io.state.receipts_used.load(Ordering::SeqCst),
            expected_calls as u32 + 1
        );
        assert!(io.state.pending_approvals().is_empty());
        WorkerOutcome::Ok(Box::new(protocol::CompletedRun {
            response: json!({
                "output": output,
                "receipt_id": retried_id,
                "receipt_ids": receipt_ids,
                "receipt_calls": io.state.receipts_used.load(Ordering::SeqCst),
                "app_controls": io.state.app_calls_used.load(Ordering::SeqCst),
                "worker_pid": identity.pid,
                "worker_start_time_ticks": identity.start_time_ticks,
                "worker_uid": identity.uid,
                "worker_gid": identity.gid,
                "worker_nnp": identity.no_new_privs,
                "worker_mount_namespace": context.mount_namespace,
            })
            .to_string(),
            turns_used: 1,
            provider: "test-orchestration".to_string(),
            model: "no-model".to_string(),
            evidence: None,
            fallback: None,
        }))
    };
    io.state
        .tx
        .send(WorkerFrame::Result {
            task_id,
            outcome: Box::new(outcome),
        })
        .unwrap();
    io.finish();
    // Keep the process alive briefly after flushing Result so the pump
    // observes the terminal protocol frame rather than only child exit.
    std::thread::sleep(Duration::from_millis(500));
}
