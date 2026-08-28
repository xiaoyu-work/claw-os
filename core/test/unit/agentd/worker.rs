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
    let notes = crate::agent::memory::notes::NotesStore::at(
        owner_root.join("agent").join("notes"),
    );
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
    assert_eq!(observed.3, home.join(".config").join("cos").join("config.json"));
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
        next_correlation: AtomicU64::new(1),
        asks_used: AtomicU32::new(0),
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
    let waiter = state.register(7);
    // A reply correlated to a different ask must not satisfy this one,
    // so a replayed or mismatched frame cannot open an unrelated gate.
    state.deliver(8, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
    state.deliver(7, ApprovalReply::Granted);
    assert_eq!(
        waiter.recv_timeout(Duration::from_millis(50)),
        Ok(ApprovalReply::Granted)
    );
    // Replaying the same correlation id finds no waiter at all.
    state.deliver(7, ApprovalReply::Granted);
    assert!(waiter.recv_timeout(Duration::from_millis(50)).is_err());
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
