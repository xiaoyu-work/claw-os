use super::*;

fn identity(uid: u32, gid: u32) -> LocalIdentity {
    LocalIdentity {
        pid: 4242,
        start_time_ticks: Some(1),
        uid,
        euid: uid,
        gid,
        egid: gid,
        groups: Vec::new(),
        no_new_privs: true,
    }
}

#[test]
fn the_worker_refuses_to_run_a_user_task_with_root_ids() {
    // A task owned by an ordinary account must never end up running
    // with root ids: that means the drop silently failed.
    assert!(identity(0, 0).require_expected_identity(1000).is_err());
    assert!(identity(1000, 0).require_expected_identity(1000).is_err());
    let mut root_euid = identity(1000, 1000);
    root_euid.euid = 0;
    assert!(root_euid.require_expected_identity(1000).is_err());
}

#[test]
fn a_root_owned_task_is_refused_even_if_one_reaches_a_worker() {
    // The supervisor refuses root-owned tasks before spawning, so this
    // is the second line of the same rule.
    let error = identity(0, 0)
        .require_expected_identity(0)
        .expect_err("a root-owned task must never run the model");
    assert_eq!(error, crate::agentd::spawn::ROOT_OWNER_REFUSAL);
    assert!(identity(1000, 1000).require_expected_identity(0).is_err());
}

#[test]
fn the_worker_refuses_to_run_without_no_new_privs() {
    let mut identity = identity(1000, 1000);
    identity.no_new_privs = false;
    let error = identity
        .require_expected_identity(1000)
        .expect_err("NNP is mandatory");
    assert!(error.contains("NO_NEW_PRIVS"), "{error}");
}

#[test]
fn an_unprivileged_worker_is_accepted() {
    assert!(identity(1000, 1000).require_expected_identity(1000).is_ok());
}

#[tokio::test]
async fn routed_curator_context_survives_detached_spawn() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("owner-home");
    std::fs::create_dir_all(&home).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let owner_uid = 4242;
    let context = crate::paths::with_routed_job(crate::paths::with_user_override(
        owner_uid,
        home.clone(),
        async { crate::paths::RoutedPathContext::capture() },
    ))
    .await;
    let owner_root = root.path().join("users").join(owner_uid.to_string());
    let curation_log = owner_root
        .join("agent")
        .join("memory")
        .join("curation_log.json");
    let notes = crate::agent::memory::notes::NotesStore::at(owner_root.join("agent").join("notes"));
    let mut config = crate::config::CosConfig::default();
    config.agent.provider = "openai".into();
    config.agent.model = "gpt-4o-mini".into();
    config.agent.api_key_env = Some("OPENAI_API_KEY".into());
    let config = Arc::new(config);
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().unwrap();
    let curator =
        crate::agent::runtime::auto_curator::AutoCurator::from_snapshot_with_runtime_paths(
            Arc::clone(&config),
            &db,
            notes,
            context.clone(),
            curation_log.clone(),
        )
        .expect("routed curator");
    assert_eq!(curator.log_path(), curation_log);

    let observed = tokio::spawn(crate::agent::runtime::auto_curator::with_detached_context(
        config,
        context,
        None,
        async move {
            curator.save_empty_log().expect("save routed curation log");
            (
                crate::paths::ai_budget_db_path(),
                crate::paths::ai_run_log_path(),
                crate::paths::agent_notes_dir(),
                crate::paths::user_config_path(),
                crate::paths::current_owner_uid_override(),
                crate::paths::is_routed_job(),
                curator.log_path().to_path_buf(),
            )
        },
    ))
    .await
    .unwrap();

    assert_eq!(observed.0, owner_root.join("ai_budget.db"));
    assert_eq!(observed.1, owner_root.join("logs").join("ai.jsonl"));
    assert_eq!(observed.2, owner_root.join("agent").join("notes"));
    assert_eq!(
        observed.3,
        home.join(".config").join("cos").join("config.json")
    );
    assert_eq!(observed.4, Some(owner_uid));
    assert!(observed.5);
    assert_eq!(observed.6, curation_log);
    assert!(curation_log.is_file());
    assert!(crate::paths::current_owner_uid_override().is_none());
    assert!(!crate::paths::is_routed_job());
}

// ---------------------------------------------------------------------------
// Permission mediation
// ---------------------------------------------------------------------------

fn gateway() -> (ChannelApprovalGateway, mpsc::UnboundedReceiver<WorkerFrame>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(ChannelState {
        tx,
        cancelled: Arc::new(AtomicBool::new(false)),
        waiters: Mutex::new(HashMap::new()),
        pending_approvals: Mutex::new(Vec::new()),
        next_correlation: AtomicU64::new(1),
        asks_used: AtomicU32::new(0),
        receipts_used: AtomicU32::new(0),
        app_calls_used: AtomicU32::new(0),
    });
    (
        ChannelApprovalGateway {
            task_id: "task-a".to_string(),
            state,
        },
        rx,
    )
}

fn scope() -> Scope {
    Scope::path("/home/user/notes.txt")
}

#[test]
fn an_ask_names_only_the_denied_verb_and_scope() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    std::thread::spawn(move || {
        let _ = gateway.request(Verb::FS_READ, &scope());
    });
    let frame = loop {
        if let Ok(frame) = rx.try_recv() {
            break frame;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let WorkerFrame::Approval {
        task_id,
        correlation_id,
        ask,
    } = frame
    else {
        panic!("expected an approval frame");
    };
    assert_eq!(task_id, "task-a");
    assert_eq!(ask.verb(), Verb::FS_READ.as_str());
    assert_eq!(ask.scope(), &scope());
    // Nothing about identity travels: the frame has no session, owner,
    // worker or capability field at all.
    let encoded = serde_json::to_string(&ask).expect("encode");
    for forbidden in ["session", "owner", "uid", "caps", "role", "decision"] {
        assert!(
            !encoded.contains(forbidden),
            "an ask must not carry `{forbidden}`: {encoded}"
        );
    }
    state.deliver(correlation_id, ApprovalReply::Granted);
}

#[test]
fn a_refusal_keeps_the_gate_closed_rather_than_opening_it() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope()));
    let correlation_id = loop {
        if let Ok(WorkerFrame::Approval { correlation_id, .. }) = rx.try_recv() {
            break correlation_id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(
        correlation_id,
        ApprovalReply::Refused {
            message: "consent store is unavailable".to_string(),
        },
    );
    let result = handle.join().expect("join");
    assert!(
        result.is_err(),
        "a refusal must surface as an error, never as a grant"
    );
}

#[test]
fn a_pending_request_does_not_grant_anything() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope()));
    let correlation_id = loop {
        if let Ok(WorkerFrame::Approval { correlation_id, .. }) = rx.try_recv() {
            break correlation_id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(correlation_id, ApprovalReply::Pending { request_id: None });
    assert_eq!(handle.join().expect("join"), Ok(false));
}

#[test]
fn a_filed_request_marks_the_worker_for_durable_suspension() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let interrupt = crate::agent::runtime::interrupt::register("task-a");
    let handle = std::thread::spawn(move || gateway.request(Verb::FS_READ, &scope()));
    let correlation_id = loop {
        if let Ok(WorkerFrame::Approval { correlation_id, .. }) = rx.try_recv() {
            break correlation_id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(
        correlation_id,
        ApprovalReply::Pending {
            request_id: Some("approval-a".to_string()),
        },
    );

    let pending = handle.join().expect("join").expect("request");
    assert_eq!(pending.request_id.as_deref(), Some("approval-a"));
    assert_eq!(state.pending_approvals(), vec!["approval-a"]);
    assert!(
        interrupt.check(),
        "filing an approval must interrupt the active runtime turn"
    );
}

#[test]
fn mediation_is_bounded_per_task() {
    let (gateway, _rx) = gateway();
    gateway
        .state
        .asks_used
        .store(protocol::MAX_APPROVAL_ASKS, Ordering::SeqCst);
    let error = gateway
        .consume(Verb::FS_READ, &scope())
        .expect_err("the budget must be enforced");
    assert!(error.contains("budget"), "{error}");
}

#[test]
fn cancellation_stops_mediation_immediately() {
    let (gateway, _rx) = gateway();
    gateway.state.cancelled.store(true, Ordering::SeqCst);
    let error = gateway
        .request(Verb::FS_READ, &scope())
        .expect_err("a cancelled task must not keep asking");
    assert!(error.contains("cancelled"), "{error}");
}

#[test]
fn losing_the_channel_refuses_every_waiter() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope()));
    loop {
        if rx.try_recv().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    state.refuse_all("clawd closed the agent worker channel");
    assert!(handle.join().expect("join").is_err());
}

#[test]
fn a_reply_is_delivered_only_to_its_own_waiter() {
    let (gateway, _rx) = gateway();
    let state = gateway.state.clone();
    let waiter = state.register(7).unwrap();
    // A reply correlated to a different ask must not satisfy this one,
    // so a replayed or mismatched frame cannot open an unrelated gate.
    state.deliver(8, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    state.deliver(7, ApprovalReply::Granted);
    assert_eq!(
        waiter.recv_timeout(Duration::from_millis(50)),
        Ok(ChannelReply::Approval(ApprovalReply::Granted))
    );
    // Replaying the same correlation id finds no waiter at all.
    state.deliver(7, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
}

fn receipt() -> crate::activities::ReceiptReport {
    crate::operations::receipts::capture(
        uuid::Uuid::new_v4().to_string(),
        "demo".into(),
        "read".into(),
        format!("sha256:{}", "a".repeat(64)),
        Ok(Some("reported result".into())),
    )
}

fn receipt_recorder() -> (ChannelReceiptRecorder, mpsc::UnboundedReceiver<WorkerFrame>) {
    let (gateway, rx) = gateway();
    (
        ChannelReceiptRecorder {
            task_id: gateway.task_id,
            state: gateway.state,
        },
        rx,
    )
}

fn next_receipt(rx: &mut mpsc::UnboundedReceiver<WorkerFrame>) -> ReceiptRequest {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let frame = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
    });
    let WorkerFrame::Receipt(request) = frame else {
        panic!("expected a receipt report")
    };
    *request
}

#[test]
fn a_receipt_crosses_the_control_socket_and_matches_only_its_own_acknowledgement() {
    use std::io::Write;

    let (recorder, mut rx) = receipt_recorder();
    let state = recorder.state.clone();
    let report = receipt();
    let expected_id = report.id.clone();
    let waiter = std::thread::spawn(move || recorder.record(report));
    let request = next_receipt(&mut rx);
    assert_eq!(request.task_id, "task-a");
    assert_eq!(request.report.id, expected_id);
    state.deliver(
        request.correlation_id + 1,
        ReceiptReply::Recorded {
            receipt_id: expected_id.clone(),
        },
    );
    assert!(state
        .waiters
        .lock()
        .unwrap()
        .contains_key(&request.correlation_id));
    let reply = protocol::encode(&BrokerFrame::ReceiptReply {
        correlation_id: request.correlation_id,
        reply: ReceiptReply::Recorded {
            receipt_id: expected_id.clone(),
        },
    })
    .unwrap();
    let (mut sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    receiver.set_nonblocking(true).unwrap();
    sender.write_all(reply.as_bytes()).unwrap();
    drop(sender);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let socket = tokio::net::UnixStream::from_std(receiver).unwrap();
        watch_control(
            FrameReader::new(BufReader::new(socket)),
            "task-a".into(),
            state.clone(),
        )
        .await;
    });
    assert_eq!(waiter.join().unwrap(), Ok(expected_id));
    assert!(state.waiters.lock().unwrap().is_empty());
}

#[test]
fn receipt_refusals_wrong_ids_and_permission_replies_never_acknowledge_storage() {
    for reply in [
        ChannelReply::Receipt(ReceiptReply::Recorded {
            receipt_id: uuid::Uuid::new_v4().to_string(),
        }),
        ChannelReply::Receipt(ReceiptReply::Refused {
            message: "store unavailable".into(),
        }),
        ChannelReply::Approval(ApprovalReply::Granted),
    ] {
        let (recorder, mut rx) = receipt_recorder();
        let state = recorder.state.clone();
        let waiter = std::thread::spawn(move || recorder.record(receipt()));
        let request = next_receipt(&mut rx);
        state.deliver(request.correlation_id, reply);
        assert!(waiter.join().unwrap().is_err());
        assert!(state.waiters.lock().unwrap().is_empty());
    }
}

#[test]
fn receipt_reporting_is_bounded_and_disconnects_release_waiters() {
    let (recorder, mut rx) = receipt_recorder();
    recorder
        .state
        .receipts_used
        .store(protocol::MAX_RECEIPT_REPORTS, Ordering::SeqCst);
    assert!(recorder.record(receipt()).unwrap_err().contains("budget"));
    assert!(rx.try_recv().is_err());
    let (recorder, mut rx) = receipt_recorder();
    let state = recorder.state.clone();
    let waiter = std::thread::spawn(move || recorder.record(receipt()));
    let _request = next_receipt(&mut rx);
    state.refuse_all("channel closed after the invocation");
    assert!(waiter
        .join()
        .unwrap()
        .unwrap_err()
        .contains("channel closed"));
    assert!(state.waiters.lock().unwrap().is_empty());
}

#[test]
fn cancellation_does_not_discard_a_late_report_or_create_an_approval() {
    let (recorder, mut rx) = receipt_recorder();
    let state = recorder.state.clone();
    state.cancelled.store(true, Ordering::SeqCst);
    let waiter = std::thread::spawn(move || recorder.record(receipt()));
    let request = next_receipt(&mut rx);
    state.deliver(
        request.correlation_id,
        ReceiptReply::Recorded {
            receipt_id: request.report.id.clone(),
        },
    );
    assert_eq!(waiter.join().unwrap(), Ok(request.report.id));
    assert!(state.pending_approvals().is_empty());
    assert_eq!(state.asks_used.load(Ordering::SeqCst), 0);
}

fn app_gateway() -> (ChannelAppGateway, mpsc::UnboundedReceiver<WorkerFrame>) {
    let (gateway, rx) = gateway();
    (
        ChannelAppGateway {
            task_id: gateway.task_id,
            state: gateway.state,
        },
        rx,
    )
}

fn app_registration() -> crate::clawd::protocol::Request {
    crate::clawd::protocol::Request::build(
        crate::clawd::routes::Command::AppSessionRegister,
        serde_json::json!({
            "app_id":"demo","kind":"operation","operation":"read","args":[]
        }),
    )
}

fn next_app_request(rx: &mut mpsc::UnboundedReceiver<WorkerFrame>) -> protocol::AppHostRequest {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let frame = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
    });
    let WorkerFrame::AppHost(request) = frame else {
        panic!("expected App-host control")
    };
    *request
}

#[test]
fn app_control_carries_retained_context_and_accepts_only_its_own_reply() {
    use crate::clawd::client::BrokerGateway;
    let (gateway, mut rx) = app_gateway();
    let state = gateway.state.clone();
    let request = app_registration();
    let id = request.id.clone();
    let invocation = uuid::Uuid::new_v4().to_string();
    let expected = invocation.clone();
    let waiter = std::thread::spawn(move || gateway.request(request, Some(invocation)));
    let request = next_app_request(&mut rx);
    assert_eq!(request.task_id, "task-a");
    assert_eq!(request.request_id, id);
    let protocol::AppHostCall::Register(registration) = request.call else {
        panic!("registration")
    };
    assert_eq!(registration.invocation_id.as_str(), expected);
    assert_eq!(registration.request.app_id.as_str(), "demo");
    state.deliver(
        request.correlation_id,
        ChannelReply::AppHost(Box::new(crate::clawd::protocol::Response::ok(
            id,
            serde_json::json!({"registered":true}),
        ))),
    );
    assert!(waiter.join().unwrap().unwrap().ok);
}

#[test]
fn app_control_rejects_unscoped_registration_and_unrelated_broker_commands() {
    use crate::clawd::client::BrokerGateway;
    let (gateway, mut rx) = app_gateway();
    assert!(gateway.request(app_registration(), None).is_err());
    assert!(gateway
        .request(
            crate::clawd::protocol::Request::build(
                crate::clawd::routes::Command::DaemonHealth,
                serde_json::json!({}),
            ),
            None,
        )
        .is_err());
    assert!(rx.try_recv().is_err());
    gateway.state.cancelled.store(true, Ordering::SeqCst);
    assert!(gateway
        .request(app_registration(), Some(uuid::Uuid::new_v4().to_string()))
        .is_err());
    assert!(rx.try_recv().is_err());
}

#[test]
fn package_checks_use_the_host_channel_without_a_writable_runtime_store() {
    use crate::clawd::client::BrokerGateway;
    let (gateway, mut rx) = app_gateway();
    let state = gateway.state.clone();
    let waiter = std::thread::spawn(move || {
        gateway.check_app("app-fixture", &format!("sha256:{}", "b".repeat(64)))
    });
    let request = next_app_request(&mut rx);
    assert!(matches!(request.call, protocol::AppHostCall::Check(_)));
    state.deliver(
        request.correlation_id,
        ChannelReply::AppHost(Box::new(crate::clawd::protocol::Response::ok(
            request.request_id,
            serde_json::json!({"live":true,"caps":[]}),
        ))),
    );
    assert_eq!(waiter.join().unwrap(), Ok(crate::caps::CapSet::new()));
}

#[test]
fn app_control_never_treats_receipt_or_mismatched_responses_as_success() {
    use crate::clawd::client::BrokerGateway;
    for wrong_kind in [false, true] {
        let (gateway, mut rx) = app_gateway();
        let state = gateway.state.clone();
        let waiter = std::thread::spawn(move || {
            gateway.request(app_registration(), Some(uuid::Uuid::new_v4().to_string()))
        });
        let request = next_app_request(&mut rx);
        if wrong_kind {
            state.deliver(
                request.correlation_id,
                ReceiptReply::Recorded {
                    receipt_id: "unrelated".into(),
                },
            );
        } else {
            state.deliver(
                request.correlation_id,
                ChannelReply::AppHost(Box::new(crate::clawd::protocol::Response::ok(
                    crate::clawd::protocol::RequestId::generate(),
                    serde_json::json!({}),
                ))),
            );
        }
        assert!(waiter.join().unwrap().is_err());
    }
}

#[tokio::test]
async fn trusted_task_scope_is_rebound_to_the_authenticated_worker_process() {
    let session: crate::proc::SessionInfo = serde_json::from_value(serde_json::json!({
        "session_id":"task-session","pid":1,"command":["task"],"started_at":"now",
        "stdout_path":"","stderr_path":"","caps":[],"role":"worker"
    }))
    .unwrap();
    with_session(Some(session), async {
        let current = crate::proc::current_session_info_for_caps().unwrap();
        assert_eq!(current.session_id, "task-session");
        assert_eq!(current.pid, std::process::id());
        assert_eq!(
            current.start_time_ticks,
            crate::proc::read_start_time_ticks_pub(std::process::id())
        );
        crate::caps::enforcement::require_current_session_identity(
            &current.session_id,
            current.pid,
        )
        .unwrap();
    })
    .await;
}

#[test]
fn a_hand_started_worker_has_no_channel() {
    let _lock = crate::test_env::lock_env();
    let previous = std::env::var_os(protocol::CHANNEL_FD_ENV);
    std::env::remove_var(protocol::CHANNEL_FD_ENV);
    let error = adopt_channel().expect_err("a worker without a channel must refuse to start");
    assert!(error.contains("must be started by clawd"), "{error}");

    // A caller cannot redirect the channel to a descriptor of its own
    // choosing either.
    std::env::set_var(protocol::CHANNEL_FD_ENV, "9");
    let error = adopt_channel().expect_err("only fd 3 is the job channel");
    assert!(error.contains("must be fd"), "{error}");

    match previous {
        Some(value) => std::env::set_var(protocol::CHANNEL_FD_ENV, value),
        None => std::env::remove_var(protocol::CHANNEL_FD_ENV),
    }
}

#[cfg(target_os = "linux")]
mod app_host_process {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/app_host/worker_process.rs"
    ));
}
