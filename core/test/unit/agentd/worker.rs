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
fn adopted_channel_is_cloexec_and_bootstrap_hints_are_removed() {
    use std::os::fd::AsRawFd;

    let _lock = crate::test_env::lock_env();
    let _channel_hint = crate::test_env::TestEnvVarGuard::set(protocol::CHANNEL_FD_ENV, "3");
    let _task_hint = crate::test_env::TestEnvVarGuard::set(protocol::TASK_HINT_ENV, "task-secret");
    let (channel, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let fd = channel.as_raw_fd();
    harden_adopted_channel(fd).unwrap();

    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    assert!(std::env::var_os(protocol::CHANNEL_FD_ENV).is_none());
    assert!(std::env::var_os(protocol::TASK_HINT_ENV).is_none());
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
        app_waiters: Mutex::new(HashMap::new()),
        next_correlation: AtomicU64::new(1),
        asks_used: AtomicU32::new(0),
    });
    (
        ChannelApprovalGateway {
            task_id: "task-a".to_string(),
            consent_context: crate::caps::ConsentContext::Attended,
            state,
        },
        rx,
    )
}

#[tokio::test]
async fn app_gateway_reply_requires_exact_correlation_and_exchange() {
    let (gateway, _frames) = gateway();
    let state = gateway.state;
    let request = AppGatewayRequest {
        app_id: "email".to_string(),
        tool: "email.search".to_string(),
        arguments: serde_json::json!({"query": "Acme"}),
        timeout_ms: 1_000,
    };
    let exchange = AppGatewayExchange::new(request);
    let mut waiter = state.register_app(7, exchange.clone());
    state.deliver_app(
        8,
        &exchange,
        AppGatewayReply::Failed {
            message: "wrong correlation".to_string(),
        },
    );
    assert!(matches!(
        waiter.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    let mut substituted = exchange.clone();
    substituted.nonce = "f".repeat(32);
    state.deliver_app(
        7,
        &substituted,
        AppGatewayReply::Failed {
            message: "wrong exchange".to_string(),
        },
    );
    assert!(matches!(
        waiter.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    state.deliver_app(
        7,
        &exchange,
        AppGatewayReply::Failed {
            message: "exact".to_string(),
        },
    );
    assert!(matches!(
        waiter.await.unwrap(),
        AppGatewayReply::Failed { message } if message == "exact"
    ));
}

#[tokio::test]
async fn dropping_an_app_call_sends_exact_cancellation() {
    let (gateway, mut frames) = gateway();
    let state = gateway.state;
    let request = AppGatewayRequest {
        app_id: "email".to_string(),
        tool: "email.search".to_string(),
        arguments: serde_json::json!({}),
        timeout_ms: 1_000,
    };
    let exchange = AppGatewayExchange::new(request);
    let waiter = state.register_app(9, exchange.clone());
    drop(AppGatewayCancellation {
        task_id: "task-a".to_string(),
        correlation_id: 9,
        nonce: exchange.nonce.clone(),
        state,
        armed: true,
    });
    let WorkerFrame::AppGatewayCancel {
        task_id,
        correlation_id,
        nonce,
    } = frames.try_recv().unwrap()
    else {
        panic!("expected App Gateway cancellation");
    };
    assert_eq!(task_id, "task-a");
    assert_eq!(correlation_id, 9);
    assert_eq!(nonce, exchange.nonce);
    assert!(waiter.await.is_err());
}

fn scope() -> Scope {
    Scope::path("/home/user/notes.txt")
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
    state.deliver(7, &expected, ApprovalReply::Granted);
    assert_eq!(
        waiter.recv_timeout(Duration::from_millis(50)),
        Ok(ApprovalReply::Granted)
    );
    // Replaying the same correlation id finds no waiter at all.
    state.deliver(7, &expected, ApprovalReply::Granted);
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
