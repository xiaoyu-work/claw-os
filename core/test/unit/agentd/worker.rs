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
        dumpable: false,
    }
}

fn extension_binding() -> crate::extension_host::protocol::ExtensionBinding {
    crate::extension_host::protocol::ExtensionBinding {
        protocol: crate::extension_host::protocol::PROTOCOL_VERSION,
        purpose: crate::extension_host::protocol::HostPurpose::Task,
        task_id: "task-a".to_string(),
        session_id: None,
        app_id: None,
        owner_uid: 1000,
        extension_uid: 61_000,
        owner_gid: 1000,
        capability_generation: "a".repeat(16),
        package: None,
        approved_paths: vec![crate::extension_host::protocol::ApprovedPath {
            path: "/home/test".to_string(),
            device: 1,
            inode: 2,
            owner_uid: 1000,
            mode: 0o40755,
        }],
        agent_extensions: Vec::new(),
        controller_uid: 1000,
        controller_gid: 1000,
        controller_pid: 4242,
        controller_start_time_ticks: Some(1),
        host_pid: 4243,
        host_start_time_ticks: Some(2),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        expires_at_ms: u64::MAX,
        control_socket: "/run/cos/extensions/control.sock".to_string(),
        broker_socket: "/run/cos/extensions/broker.sock".to_string(),
    }
}

#[tokio::test]
async fn an_old_assignment_without_receipts_gets_an_explicit_protocol_rejection() {
    let extension = extension_binding();
    let signer = crate::agentd::grant::GrantSigner::from_secret([7u8; 32]);
    let grant = signer.issue(crate::agentd::grant::GrantClaims {
        v: crate::agentd::grant::GRANT_VERSION - 1,
        audience: crate::agentd::grant::GRANT_AUDIENCE.to_string(),
        broker_pid: 1,
        task_id: "task-a".to_string(),
        session_id: None,
        owner_uid: 1000,
        owner_gid: 1000,
        client: crate::session::SessionClient::default(),
        presence: None,
        capability_generation: "a".repeat(16),
        prepare_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        commit_nonce: "fedcba9876543210fedcba9876543210".to_string(),
        extension: Some(extension.clone()),
        worker_pid: 4242,
        worker_start_time_ticks: Some(1),
        issued_at_ms: 1,
        expires_at_ms: u64::MAX,
        routes: protocol::worker_routes(),
    });
    let frame = BrokerFrame::Prepare(Box::new(Assignment {
        protocol: protocol::PROTOCOL_VERSION - 1,
        grant,
        job: protocol::JobSpec {
            id: "task-a".to_string(),
            prompt: "test".to_string(),
            context: None,
            branch_context: None,
            session_id: None,
            max_turns: None,
            requested_model: None,
            use_memory: false,
            owner_uid: 1000,
            owner_home: "/home/test".to_string(),
            workspace: "/home/test".to_string(),
            record_activity_receipts: false,
            activity_capability_checks: false,
            activity_monetary_checks: false,
        },
        consent_context: ConsentContext::Unattended,
        session: None,
        presence: None,
        extension: Some(extension),
    }));
    let mut document = serde_json::to_value(frame).unwrap();
    document["grant"]["claims"]["extension"]
        .as_object_mut()
        .unwrap()
        .remove("agent_extensions");
    document["extension"]
        .as_object_mut()
        .unwrap()
        .remove("agent_extensions");
    let mut bytes = serde_json::to_vec(&document).unwrap();
    bytes.push(b'\n');
    let mut frames = FrameReader::new(BufReader::new(bytes.as_slice()));

    let error = receive_assignment(&mut frames, &identity(1000, 1000))
        .await
        .unwrap_err();
    assert!(error.contains("agentd protocol mismatch"), "{error}");
    assert!(error.contains("reinstall"), "{error}");
}

#[test]
fn the_worker_refuses_to_run_a_user_task_with_root_ids() {
    // A task owned by an ordinary account must never end up running
    // with root ids: that means the drop silently failed.
    assert!(identity(0, 0)
        .require_expected_identity(1000, 1000)
        .is_err());
    assert!(identity(1000, 0)
        .require_expected_identity(1000, 1000)
        .is_err());
    let mut root_euid = identity(1000, 1000);
    root_euid.euid = 0;
    assert!(root_euid.require_expected_identity(1000, 1000).is_err());
}

#[test]
fn a_root_owned_task_is_refused_even_if_one_reaches_a_worker() {
    // The supervisor refuses root-owned tasks before spawning, so this
    // is the second line of the same rule.
    let error = identity(0, 0)
        .require_expected_identity(0, 1000)
        .expect_err("a root-owned task must never run the model");
    assert_eq!(error, crate::agentd::spawn::ROOT_OWNER_REFUSAL);
    assert!(identity(1000, 1000)
        .require_expected_identity(0, 1000)
        .is_err());
}

#[test]
fn the_worker_refuses_to_run_without_no_new_privs() {
    let mut identity = identity(1000, 1000);
    identity.no_new_privs = false;
    let error = identity
        .require_expected_identity(1000, 1000)
        .expect_err("NNP is mandatory");
    assert!(error.contains("NO_NEW_PRIVS"), "{error}");
}

#[test]
fn an_unprivileged_worker_is_accepted() {
    assert!(identity(1000, 1000)
        .require_expected_identity(1000, 1000)
        .is_ok());
}

#[test]
fn the_worker_requires_the_dedicated_gid_and_no_supplementary_groups() {
    assert!(identity(1000, 1000)
        .require_expected_identity(1000, 2000)
        .unwrap_err()
        .contains("isolated execution gid"));
    let mut with_groups = identity(1000, 2000);
    with_groups.groups.push(27);
    assert!(with_groups
        .require_expected_identity(1000, 2000)
        .unwrap_err()
        .contains("supplementary groups"));
}

#[test]
fn adopted_channel_is_cloexec_and_its_bootstrap_hint_is_removed() {
    use std::os::fd::AsRawFd;

    let _lock = crate::test_env::lock_env();
    let _channel_hint = crate::test_env::TestEnvVarGuard::set(protocol::CHANNEL_FD_ENV, "3");
    let (channel, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = channel.as_raw_fd();
    harden_adopted_channel(fd).unwrap();

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    assert!(std::env::var_os(protocol::CHANNEL_FD_ENV).is_none());
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
        receipt_waiters: Mutex::new(HashMap::new()),
        monetary_waiters: Mutex::new(HashMap::new()),
        pending_approvals: Mutex::new(Vec::new()),
        next_correlation: AtomicU64::new(1),
        asks_used: AtomicU32::new(0),
        boundaries_used: AtomicU32::new(0),
        receipts_used: AtomicU32::new(0),
        monetary_used: AtomicU32::new(0),
    });
    (
        ChannelApprovalGateway {
            task_id: "task-a".to_string(),
            consent_context: crate::caps::ConsentContext::Attended,
            state,
            activity_checks: false,
        },
        rx,
    )
}

#[test]
fn cancelled_worker_refuses_reservations_but_can_settle_an_existing_turn() {
    use crate::agent::runtime::monetary_budget::MonetaryBudgetController;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let state = Arc::new(ChannelState {
        tx,
        cancelled: Arc::new(AtomicBool::new(true)),
        waiters: Mutex::new(HashMap::new()),
        receipt_waiters: Mutex::new(HashMap::new()),
        monetary_waiters: Mutex::new(HashMap::new()),
        pending_approvals: Mutex::new(Vec::new()),
        next_correlation: AtomicU64::new(1),
        asks_used: AtomicU32::new(0),
        boundaries_used: AtomicU32::new(0),
        receipts_used: AtomicU32::new(0),
        monetary_used: AtomicU32::new(0),
    });
    let controller = Arc::new(ChannelMonetaryBudgetController {
        task_id: "task-a".into(),
        state: state.clone(),
    });
    assert!(controller
        .reserve(crate::activities::MonetaryReservationRequest {
            call_id: uuid::Uuid::new_v4().to_string(),
            job_id: String::new(),
            session_id: None,
            turn_index: 4,
            input_upper_bound_tokens: 10,
            requested_max_output_tokens: 20,
        })
        .is_err());

    let reservation = crate::activities::MonetaryReservation {
        call_id: uuid::Uuid::new_v4().to_string(),
        activity_id: uuid::Uuid::new_v4().to_string(),
        owner_uid: 1000,
        job_id: "task-a".into(),
        session_id: Some("session-a".into()),
        turn_index: 4,
        policy_revision: 1,
        reserved_microusd: 10,
        input_upper_bound_tokens: 10,
        requested_max_output_tokens: 20,
        policy_max_output_tokens_per_turn: 20,
        max_output_tokens: 20,
        input_microusd_per_million_tokens: 1,
        output_microusd_per_million_tokens: 1,
        reserved_at: "2026-09-13T00:00:00Z".into(),
    };
    let worker = {
        let controller = controller.clone();
        let reservation = reservation.clone();
        std::thread::spawn(move || {
            controller.settle(
                &reservation,
                crate::agent::runtime::monetary_budget::conservative_settlement(
                    "provider".into(),
                    "model".into(),
                ),
            )
        })
    };
    let frame = rx.blocking_recv().unwrap();
    let WorkerFrame::MonetaryBudget(request) = frame else {
        panic!("expected monetary settlement exchange");
    };
    assert_eq!(request.task_id, "task-a");
    assert!(matches!(
        request.operation,
        MonetaryBudgetOperation::Settle {
            ref call_id,
            turn_index: 4,
            ..
        } if call_id == &reservation.call_id
    ));
    state.deliver_monetary(request.correlation_id, MonetaryBudgetReply::Settled);
    worker.join().unwrap().unwrap();
}

fn scope() -> Scope {
    Scope::path("/home/user/notes.txt")
}

fn next_approval(rx: &mut mpsc::UnboundedReceiver<WorkerFrame>) -> (u64, ApprovalExchange) {
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
    let WorkerFrame::Approval {
        correlation_id,
        exchange,
        ..
    } = frame
    else {
        panic!("expected a typed approval exchange");
    };
    (correlation_id, exchange)
}

#[test]
fn activity_denial_precedes_consent_and_standing_caps_do_not_bypass_exact_confirmation() {
    use crate::activities::CapabilityBoundaryDecision as Boundary;
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
    struct ResetGateway;
    impl Drop for ResetGateway {
        fn drop(&mut self) {
            crate::caps::approval_gateway::clear_for_test();
        }
    }
    let _reset = ResetGateway;
    for decision in [Boundary::Deny, Boundary::RequireApproval] {
        let (mut gateway, mut rx) = gateway();
        gateway.activity_checks = true;
        let state = gateway.state.clone();
        crate::caps::approval_gateway::install(Arc::new(gateway));
        let digest = crate::crypto::sha256_hex(b"held-cap exact operation");
        let expected_digest = digest.clone();
        let home = data.path().to_path_buf();
        let caller = std::thread::spawn(move || {
            let mut session: crate::proc::SessionInfo = serde_json::from_value(serde_json::json!({
                "session_id":"task-session","pid":1,"command":["task"],"started_at":"now",
                "stdout_path":"","stderr_path":"","role":"worker"
            }))
            .unwrap();
            session.caps = Some(crate::caps::CapSet::from_caps([crate::caps::Cap::new(
                Verb::FS_READ,
                scope(),
            )]));
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(crate::paths::with_routed_job(
                crate::paths::with_user_override(
                    unsafe { libc::geteuid() },
                    home,
                    with_session(Some(session), async {
                        crate::caps::enforcement::require_for_operation(
                            Verb::FS_READ,
                            scope(),
                            &digest,
                        )
                    }),
                ),
            ))
        });
        let (correlation_id, exchange) = next_approval(&mut rx);
        assert!(matches!(exchange.ask, ApprovalAsk::Boundary { .. }));
        state.deliver(
            correlation_id,
            &exchange,
            ApprovalReply::Boundary { decision },
        );
        if decision == Boundary::RequireApproval {
            let (id, exchange) = next_approval(&mut rx);
            assert!(matches!(exchange.ask, ApprovalAsk::Consume { .. }));
            assert_eq!(
                exchange.ask.operation_digest(),
                Some(expected_digest.as_str())
            );
            state.deliver(id, &exchange, ApprovalReply::Pending { request_id: None });
            let (id, exchange) = next_approval(&mut rx);
            assert!(matches!(exchange.ask, ApprovalAsk::Request { .. }));
            assert_eq!(exchange.ask.scope(), &scope());
            assert_eq!(
                exchange.ask.operation_digest(),
                Some(expected_digest.as_str())
            );
            state.deliver(
                id,
                &exchange,
                ApprovalReply::Pending {
                    request_id: Some("exact-confirmation".into()),
                },
            );
        }
        assert!(
            caller.join().unwrap().is_err(),
            "held caps alone must never open this gate"
        );
        assert_eq!(state.boundaries_used.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.asks_used.load(Ordering::SeqCst),
            if decision == Boundary::Deny { 0 } else { 2 }
        );
        assert!(
            rx.try_recv().is_err(),
            "denial must not escalate or replay consent"
        );
        crate::caps::approval_gateway::clear_for_test();
    }
}

#[test]
fn boundary_budget_is_4096_and_does_not_spend_the_128_consent_budget() {
    let (mut gateway, mut rx) = gateway();
    gateway.activity_checks = true;
    gateway
        .state
        .boundaries_used
        .store(protocol::MAX_BOUNDARY_CHECKS, Ordering::SeqCst);
    assert!(gateway
        .boundary(Verb::FS_READ, &scope())
        .unwrap_err()
        .contains("budget"));
    assert_eq!(gateway.state.asks_used.load(Ordering::SeqCst), 0);
    assert!(rx.try_recv().is_err());
    assert_eq!(protocol::MAX_BOUNDARY_CHECKS, 4096);
    assert_eq!(protocol::MAX_APPROVAL_ASKS, 128);
}

#[tokio::test]
async fn boundary_and_consent_replies_cannot_satisfy_each_others_waiters() {
    use crate::activities::CapabilityBoundaryDecision as Boundary;
    for (operation, reply) in [
        ("boundary", ApprovalReply::Granted),
        (
            "consume",
            ApprovalReply::Boundary {
                decision: Boundary::Normal,
            },
        ),
        (
            "request",
            ApprovalReply::Boundary {
                decision: Boundary::Normal,
            },
        ),
    ] {
        let (mut gateway, mut rx) = gateway();
        gateway.activity_checks = true;
        let state = gateway.state.clone();
        let caller = std::thread::spawn(move || match operation {
            "boundary" => gateway.boundary(Verb::FS_READ, &scope()).map(|_| ()),
            "consume" => gateway.consume(Verb::FS_READ, &scope(), None).map(|_| ()),
            _ => gateway.request(Verb::FS_READ, &scope(), None).map(|_| ()),
        });
        let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let WorkerFrame::Approval {
            correlation_id,
            exchange,
            ..
        } = frame
        else {
            panic!("wrong frame");
        };
        state.deliver(correlation_id, &exchange, reply);
        assert!(caller.join().unwrap().is_err());
        assert!(state.pending_approvals().is_empty());
    }
}

#[tokio::test]
async fn live_boundary_checks_use_their_own_counter_and_never_record_an_approval_wait() {
    use crate::activities::CapabilityBoundaryDecision as Boundary;
    let (mut gateway, mut rx) = gateway();
    gateway.activity_checks = true;
    let state = gateway.state.clone();
    state
        .asks_used
        .store(protocol::MAX_APPROVAL_ASKS, Ordering::SeqCst);
    let caller = std::thread::spawn(move || gateway.boundary(Verb::FS_READ, &scope()));
    let frame = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let WorkerFrame::Approval {
        correlation_id,
        exchange,
        ..
    } = frame
    else {
        panic!("wrong frame");
    };
    assert!(matches!(exchange.ask, ApprovalAsk::Boundary { .. }));
    state.deliver(
        correlation_id,
        &exchange,
        ApprovalReply::Boundary {
            decision: Boundary::RequireApproval,
        },
    );
    assert_eq!(caller.join().unwrap().unwrap(), Boundary::RequireApproval);
    assert_eq!(state.boundaries_used.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.asks_used.load(Ordering::SeqCst),
        protocol::MAX_APPROVAL_ASKS
    );
    assert!(state.pending_approvals().is_empty());
}

#[test]
fn an_ask_names_only_the_denied_capability_and_operation_digest() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let digest = crate::crypto::sha256_hex(b"/usr/bin/printf\0hello");
    let digest_for_worker = digest.clone();
    std::thread::spawn(move || {
        let _ = gateway.request(Verb::FS_READ, &scope(), Some(&digest_for_worker));
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
        exchange,
    } = frame
    else {
        panic!("expected an approval frame");
    };
    let ask = &exchange.ask;
    assert_eq!(task_id, "task-a");
    assert_eq!(ask.verb(), Verb::FS_READ.as_str());
    assert_eq!(ask.scope(), &scope());
    assert_eq!(ask.operation_digest(), Some(digest.as_str()));
    assert!(exchange.is_valid());
    // Nothing about identity travels: the frame has no session, owner,
    // worker or capability field at all.
    let encoded = serde_json::to_string(&ask).expect("encode");
    for forbidden in ["session", "owner", "uid", "caps", "role", "decision"] {
        assert!(
            !encoded.contains(forbidden),
            "an ask must not carry `{forbidden}`: {encoded}"
        );
    }
    state.deliver(correlation_id, &exchange, ApprovalReply::Granted);
}

#[test]
fn a_refusal_keeps_the_gate_closed_rather_than_opening_it() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope(), None));
    let (correlation_id, exchange) = loop {
        if let Ok(WorkerFrame::Approval {
            correlation_id,
            exchange,
            ..
        }) = rx.try_recv()
        {
            break (correlation_id, exchange);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(
        correlation_id,
        &exchange,
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
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope(), None));
    let (correlation_id, exchange) = loop {
        if let Ok(WorkerFrame::Approval {
            correlation_id,
            exchange,
            ..
        }) = rx.try_recv()
        {
            break (correlation_id, exchange);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(
        correlation_id,
        &exchange,
        ApprovalReply::Pending { request_id: None },
    );
    assert_eq!(handle.join().expect("join"), Ok(false));
}

#[test]
fn a_filed_request_marks_the_worker_for_durable_suspension() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let interrupt = crate::agent::runtime::interrupt::register("task-a");
    let handle = std::thread::spawn(move || gateway.request(Verb::FS_READ, &scope(), None));
    let (correlation_id, exchange) = loop {
        if let Ok(WorkerFrame::Approval {
            correlation_id,
            exchange,
            ..
        }) = rx.try_recv()
        {
            break (correlation_id, exchange);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    state.deliver(
        correlation_id,
        &exchange,
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
        .consume(Verb::FS_READ, &scope(), None)
        .expect_err("the budget must be enforced");
    assert!(error.contains("budget"), "{error}");
}

#[test]
fn cancellation_stops_mediation_immediately() {
    let (gateway, _rx) = gateway();
    gateway.state.cancelled.store(true, Ordering::SeqCst);
    let error = gateway
        .request(Verb::FS_READ, &scope(), None)
        .expect_err("a cancelled task must not keep asking");
    assert!(error.contains("cancelled"), "{error}");
}

#[test]
fn losing_the_channel_refuses_every_waiter() {
    let (gateway, mut rx) = gateway();
    let state = gateway.state.clone();
    let handle = std::thread::spawn(move || gateway.consume(Verb::FS_READ, &scope(), None));
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
    let expected = ApprovalExchange {
        nonce: "a".repeat(32),
        ask: ApprovalAsk::Consume {
            verb: Verb::FS_READ.as_str().to_string(),
            scope: scope(),
            operation_digest: Some(crate::crypto::sha256_hex(b"expected")),
        },
    };
    let waiter = state.register(7, expected.clone());
    // Correlation id alone is not enough: nonce and exact request binding
    // must all match before a reply can open the waiter.
    state.deliver(8, &expected, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    let wrong_nonce = ApprovalExchange {
        nonce: "b".repeat(32),
        ask: expected.ask.clone(),
    };
    state.deliver(7, &wrong_nonce, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    let wrong_scope = ApprovalExchange {
        nonce: expected.nonce.clone(),
        ask: ApprovalAsk::Consume {
            verb: Verb::FS_READ.as_str().to_string(),
            scope: Scope::path("/home/user/other.txt"),
            operation_digest: expected.ask.operation_digest().map(str::to_string),
        },
    };
    state.deliver(7, &wrong_scope, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    let wrong_digest = ApprovalExchange {
        nonce: expected.nonce.clone(),
        ask: ApprovalAsk::Consume {
            verb: Verb::FS_READ.as_str().to_string(),
            scope: scope(),
            operation_digest: Some(crate::crypto::sha256_hex(b"substituted")),
        },
    };
    state.deliver(7, &wrong_digest, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    let wrong_kind = ApprovalExchange {
        nonce: expected.nonce.clone(),
        ask: ApprovalAsk::Request {
            verb: Verb::FS_READ.as_str().into(),
            scope: scope(),
            operation_digest: expected.ask.operation_digest().map(str::to_owned),
        },
    };
    state.deliver(7, &wrong_kind, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    state.deliver(7, &expected, ApprovalReply::Granted);
    assert_eq!(
        waiter.recv_timeout(Duration::from_millis(50)),
        Ok(ApprovalReply::Granted)
    );
    // Replaying the same correlation id finds no waiter at all.
    state.deliver(7, &expected, ApprovalReply::Granted);
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
    state.deliver_receipt(
        request.correlation_id + 1,
        ReceiptReply::Recorded {
            receipt_id: expected_id.clone(),
        },
    );
    assert!(state
        .receipt_waiters
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
    assert!(state.receipt_waiters.lock().unwrap().is_empty());
}

#[test]
fn receipt_refusals_wrong_ids_and_permission_replies_never_acknowledge_storage() {
    for reply in [
        ReceiptReply::Recorded {
            receipt_id: uuid::Uuid::new_v4().to_string(),
        },
        ReceiptReply::Refused {
            message: "store unavailable".into(),
        },
    ] {
        let (recorder, mut rx) = receipt_recorder();
        let state = recorder.state.clone();
        let waiter = std::thread::spawn(move || recorder.record(receipt()));
        let request = next_receipt(&mut rx);
        let exchange = ApprovalExchange::new(ApprovalAsk::Consume {
            verb: Verb::FS_READ.as_str().into(),
            scope: scope(),
            operation_digest: None,
        });
        state.deliver(request.correlation_id, &exchange, ApprovalReply::Granted);
        assert!(state
            .receipt_waiters
            .lock()
            .unwrap()
            .contains_key(&request.correlation_id));
        state.deliver_receipt(request.correlation_id, reply);
        assert!(waiter.join().unwrap().is_err());
        assert!(state.receipt_waiters.lock().unwrap().is_empty());
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
    assert!(state.receipt_waiters.lock().unwrap().is_empty());
}

#[test]
fn cancellation_does_not_discard_a_late_report_or_create_an_approval() {
    let (recorder, mut rx) = receipt_recorder();
    let state = recorder.state.clone();
    state.cancelled.store(true, Ordering::SeqCst);
    let waiter = std::thread::spawn(move || recorder.record(receipt()));
    let request = next_receipt(&mut rx);
    state.deliver_receipt(
        request.correlation_id,
        ReceiptReply::Recorded {
            receipt_id: request.report.id.clone(),
        },
    );
    assert_eq!(waiter.join().unwrap(), Ok(request.report.id));
    assert!(state.pending_approvals().is_empty());
    assert_eq!(state.asks_used.load(Ordering::SeqCst), 0);
}

#[test]
fn receipt_monetary_and_consent_waiters_are_separate_even_for_the_same_counter() {
    let (gateway, _rx) = gateway();
    let state = gateway.state;
    let exchange = ApprovalExchange::new(ApprovalAsk::Consume {
        verb: Verb::FS_READ.as_str().into(),
        scope: scope(),
        operation_digest: Some(crate::crypto::sha256_hex(b"exact invocation")),
    });
    let consent = state.register(7, exchange.clone());
    let receipt = state.register_receipt(7).unwrap();
    let monetary = state.register_monetary(7).unwrap();
    state.deliver_receipt(
        7,
        ReceiptReply::Recorded {
            receipt_id: "receipt-a".into(),
        },
    );
    assert_eq!(
        receipt.recv_timeout(Duration::from_millis(50)),
        Ok(ReceiptReply::Recorded {
            receipt_id: "receipt-a".into()
        })
    );
    assert!(consent.try_recv().is_err());
    assert!(monetary.try_recv().is_err());
    assert!(state.waiters.lock().unwrap().contains_key(&7));
    state.deliver(7, &exchange, ApprovalReply::Granted);
    assert_eq!(
        consent.recv_timeout(Duration::from_millis(50)),
        Ok(ApprovalReply::Granted)
    );

    let consent = state.register(8, exchange);
    let receipt = state.register_receipt(8).unwrap();
    let monetary = state.register_monetary(8).unwrap();
    state.refuse_all("closed current worker channel");
    assert!(matches!(consent.recv_timeout(Duration::from_millis(50)),
        Ok(ApprovalReply::Refused { message }) if message.contains("closed")));
    assert!(matches!(receipt.recv_timeout(Duration::from_millis(50)),
        Ok(ReceiptReply::Refused { message }) if message.contains("closed")));
    assert!(matches!(monetary.recv_timeout(Duration::from_millis(50)),
        Ok(MonetaryBudgetReply::Refused { message }) if message.contains("closed")));
    assert!(state.waiters.lock().unwrap().is_empty());
    assert!(state.receipt_waiters.lock().unwrap().is_empty());
    assert!(state.monetary_waiters.lock().unwrap().is_empty());
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
mod activity_process {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/worker/activity_process.rs"
    ));
}
