// Root's process fixture must launch this exact test as the signed worker,
// provide PREPARE/COMMIT and a real isolated task Host, and install the real
// owner/App service manager. No test trust override or Host success stub is used.
use super::*;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::agent::tools::app_gateway::{
    McpCallContext, McpPrincipal, McpPrincipalKind, CALL_CONTEXT_WIRE_VERSION,
};
use serde::Deserialize;
use serde_json::json;

const CONTEXT_ENV: &str = "COS_ACTIVITY_EXTENSION_PROCESS_CONTEXT";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessContext {
    owner_uid: u32,
    execution_gid: u32,
    owner_home: PathBuf,
    app_id: String,
    package_digest: String,
    case: Case,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "case", rename_all = "snake_case", deny_unknown_fields)]
enum Case {
    OneShot {
        operation: String,
        args: Vec<String>,
    },
    Stateful {
        tool: String,
        first_arguments: Value,
        second_arguments: Value,
    },
    Denied {
        tool: String,
        arguments: Value,
    },
    WaitForStop {
        expected_turns: u32,
        ready_file: PathBuf,
    },
}

fn context_for_call(binding: &crate::extension_host::protocol::ExtensionBinding) -> McpCallContext {
    let call_id = uuid::Uuid::new_v4().to_string();
    McpCallContext {
        wire_version: CALL_CONTEXT_WIRE_VERSION,
        trace_id: call_id.clone(),
        call_id,
        deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 30_000),
        session_id: binding.session_id.clone(),
        task_id: Some(binding.task_id.clone()),
        caller: McpPrincipal {
            kind: McpPrincipalKind::SystemAgent,
            id: binding.session_id.clone().expect("root assigned session"),
            owner_uid: binding.owner_uid,
        },
    }
}

#[test]
#[ignore = "child entry: requires Root/private-/run controller, signed PREPARE/COMMIT, real isolated task/service Hosts and verified App fixture"]
fn activity_extension_worker_child() {
    crate::storage::set_private_umask();
    crate::agentd::spawn::set_process_undumpable().unwrap();
    let path = PathBuf::from(std::env::var_os(CONTEXT_ENV).expect("root process context"));
    let metadata = std::fs::symlink_metadata(&path).expect("inspect context");
    assert!(metadata.is_file() && !metadata.file_type().is_symlink());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.mode() & 0o022, 0);
    assert!(metadata.len() <= 64 * 1024);
    let context: ProcessContext = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    std::env::remove_var(CONTEXT_ENV);
    assert_ne!(context.owner_uid, 0);
    assert!(!crate::agentd::guard::is_broker_process());
    let identity = LocalIdentity::read().unwrap();
    identity
        .require_expected_identity(context.owner_uid, context.execution_gid)
        .unwrap();
    let io = ChannelIo::start(adopt_channel().unwrap(), identity.clone()).unwrap();
    // ChannelIo returns only after the exact authenticated COMMIT, not PREPARE.
    let assignment = io.handshake().unwrap();
    assert_eq!(assignment.protocol, 11);
    assert_eq!(assignment.job.owner_uid, context.owner_uid);
    assert_eq!(
        assignment.job.owner_home,
        context.owner_home.to_string_lossy()
    );
    assert!(assignment.job.record_activity_receipts);
    assert!(assignment.job.activity_capability_checks);
    let binding = assignment
        .extension
        .clone()
        .expect("Root-signed isolated task Host");
    assert_eq!(binding.protocol, 9);
    assert_eq!(
        binding.purpose,
        crate::extension_host::protocol::HostPurpose::Task
    );
    assert_eq!(binding.owner_uid, context.owner_uid);
    assert_ne!(binding.extension_uid, context.owner_uid);
    assert_ne!(binding.extension_uid, 0);
    assert_ne!(binding.host_pid, identity.pid);
    assert_eq!(binding.controller_pid, identity.pid);
    assert_eq!(
        binding.controller_start_time_ticks,
        identity.start_time_ticks
    );
    let task_id = assignment.job.id.clone();
    crate::caps::approval_gateway::install(Arc::new(ChannelApprovalGateway {
        task_id: task_id.clone(),
        consent_context: assignment.consent_context,
        state: io.state.clone(),
        activity_checks: true,
    }));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let host_guard = runtime
        .block_on(crate::extension_host::client::install_for_worker(
            binding.clone(),
            io.state.tx.clone(),
        ))
        .unwrap();
    let client = crate::extension_host::client::current().expect("installed task Host client");
    let channel = ChannelReceiptRecorder {
        task_id: task_id.clone(),
        state: io.state.clone(),
    };
    let max_turns = assignment.job.max_turns;
    let outcome = runtime.block_on(crate::paths::with_routed_job(
        crate::paths::with_user_override(
            context.owner_uid,
            context.owner_home.clone(),
            with_session(assignment.session, async {
                match context.case {
                    Case::OneShot { operation, args } => {
                        let output = client
                            .run_app(context.app_id.clone(), operation.clone(), args)
                            .await
                            .expect("real isolated one-shot App");
                        let report = crate::operations::receipts::capture(
                            uuid::Uuid::new_v4().to_string(),
                            context.app_id,
                            operation,
                            context.package_digest,
                            Ok(output.clone()),
                        );
                        let first = channel
                            .record(report.clone())
                            .expect("record actual output");
                        assert_eq!(
                            channel.record(report).unwrap(),
                            first,
                            "retry only reporting, never the App"
                        );
                        WorkerOutcome::Ok(Box::new(protocol::CompletedRun {
                            response:
                                json!({"output":output,"receipt_id":first,"worker_uid":identity.uid,
                                "extension_uid":binding.extension_uid})
                                .to_string(),
                            turns_used: 1,
                            provider: "real-host-fixture".into(),
                            model: "no-model".into(),
                            evidence: None,
                            fallback: None,
                        }))
                    }
                    Case::Stateful {
                        tool,
                        first_arguments,
                        second_arguments,
                    } => {
                        let first = client
                            .call_app(
                                context.app_id.clone(),
                                tool.clone(),
                                first_arguments,
                                context_for_call(&binding),
                                Duration::from_secs(30),
                            )
                            .await
                            .expect("first real owner/App service call");
                        let second = client
                            .call_app(
                                context.app_id,
                                tool,
                                second_arguments,
                                context_for_call(&binding),
                                Duration::from_secs(30),
                            )
                            .await
                            .expect("second real owner/App service call");
                        assert_ne!(
                            first.is_error,
                            Some(true),
                            "first App call reported an error"
                        );
                        assert_ne!(
                            second.is_error,
                            Some(true),
                            "second App call reported an error"
                        );
                        // Root captures these two App-service results. Reporting
                        // them again on the worker channel would double record.
                        assert_eq!(io.state.receipts_used.load(Ordering::SeqCst), 0);
                        WorkerOutcome::Ok(Box::new(protocol::CompletedRun {
                            response:
                                json!({"first":first,"second":second,"worker_uid":identity.uid,
                                "extension_uid":binding.extension_uid})
                                .to_string(),
                            turns_used: 1,
                            provider: "real-service-fixture".into(),
                            model: "no-model".into(),
                            evidence: None,
                            fallback: None,
                        }))
                    }
                    Case::Denied { tool, arguments } => {
                        let result = client
                            .call_app(
                                context.app_id,
                                tool,
                                arguments,
                                context_for_call(&binding),
                                Duration::from_secs(30),
                            )
                            .await;
                        let error =
                            result.expect_err("denied service call must not become App success");
                        assert!(
                            error.contains("Activity") && error.contains("denies"),
                            "{error}"
                        );
                        assert!(
                            io.state.pending_approvals().is_empty(),
                            "deny cannot escalate"
                        );
                        WorkerOutcome::Error { message: error }
                    }
                    Case::WaitForStop {
                        expected_turns,
                        ready_file,
                    } => {
                        assert_eq!(max_turns, Some(expected_turns));
                        std::fs::write(
                            ready_file,
                            json!({
                                "worker_pid":identity.pid,"worker_uid":identity.uid,
                                "extension_uid":binding.extension_uid,"max_turns":max_turns,
                                "protocol":protocol::PROTOCOL_VERSION
                            })
                            .to_string(),
                        )
                        .unwrap();
                        let end = tokio::time::Instant::now() + Duration::from_secs(45);
                        while !io.state.cancelled.load(Ordering::SeqCst) {
                            assert!(
                                tokio::time::Instant::now() < end,
                                "policy/expiry did not stop the leased worker"
                            );
                            io.state
                                .tx
                                .send(WorkerFrame::Heartbeat {
                                    task_id: task_id.clone(),
                                })
                                .unwrap();
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        WorkerOutcome::Cancelled
                    }
                }
            }),
        ),
    ));
    drop(host_guard);
    io.state
        .tx
        .send(WorkerFrame::Result {
            task_id,
            outcome: Box::new(outcome),
        })
        .unwrap();
    io.finish();
}
