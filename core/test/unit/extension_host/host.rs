use super::*;

fn state() -> HostState {
    let pid = std::process::id();
    let worker_uid = unsafe { libc::geteuid() };
    let owner_gid = unsafe { libc::getegid() };
    let approved_paths = vec![super::super::protocol::ApprovedPath {
        path: "/home/test".to_string(),
        device: 1,
        inode: 2,
        owner_uid: worker_uid,
        mode: 0o40755,
    }];
    let binding = super::super::protocol::ExtensionBinding {
        protocol: PROTOCOL_VERSION,
        purpose: super::super::protocol::HostPurpose::Task,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        app_id: None,
        owner_uid: worker_uid,
        extension_uid: 61_000,
        owner_gid,
        capability_generation: "a".repeat(16),
        package: None,
        approved_paths: approved_paths.clone(),
        agent_extensions: Vec::new(),
        controller_uid: worker_uid,
        controller_gid: owner_gid,
        controller_pid: pid,
        controller_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        host_pid: pid,
        host_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        expires_at_ms: crate::agentd::grant::now_ms() + 60_000,
        control_socket: "/run/cos/test/control.sock".to_string(),
        broker_socket: "/run/cos/test/broker.sock".to_string(),
    };
    HostState {
        isolation: super::super::child_isolation::IsolationAuthority::for_test(
            worker_uid,
            60_999,
            approved_paths,
        ),
        binding,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        controller_uid: worker_uid,
        controller_gid: owner_gid,
        controller_pid: pid,
        controller_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        recent: Mutex::new(VecDeque::new()),
        active: Mutex::new(HashMap::new()),
        mcp: tokio::sync::Mutex::new(HashMap::new()),
        agent_extensions: tokio::sync::Mutex::new(HashMap::new()),
        active_agent_events: Mutex::new(HashMap::new()),
        agent_extension_spawn: tokio::sync::Mutex::new(()),
        shutting_down: AtomicBool::new(false),
        fatal_shutdown: AtomicBool::new(false),
        shutdown: Notify::new(),
    }
}

fn process() -> peer::PeerProcess {
    peer::PeerProcess {
        pid: std::process::id(),
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()).unwrap(),
    }
}

fn make_request(state: &HostState) -> ControlRequest {
    ControlRequest {
        protocol: PROTOCOL_VERSION,
        id: RequestId::generate(),
        task_id: state.task_id.clone(),
        session_id: state.session_id.clone(),
        lease_nonce: state.lease_nonce.clone(),
        binding_digest: state.binding.digest().unwrap(),
        timeout_ms: 1000,
        action: HostAction::Ping,
    }
}

fn abi_binding(state: &HostState, extension_id: &str) -> super::super::abi::AbiBinding {
    super::super::abi::AbiBinding {
        task_id: state.task_id.clone(),
        session_id: state.session_id.clone().unwrap(),
        owner_uid: state.controller_uid,
        extension_id: extension_id.to_string(),
        extension_version: "1.0.0".to_string(),
        package_digest: format!("sha256:{}", "a".repeat(64)),
        manifest_digest: "b".repeat(64),
        entry_digest: "c".repeat(64),
        capability_generation: state.binding.capability_generation.clone(),
        lease_digest: crate::crypto::sha256_hex(state.lease_nonce.as_bytes()),
        instance_nonce: "d".repeat(64),
        additive: std::collections::BTreeMap::new(),
    }
}

async fn exchange(path: &std::path::Path, request: &ControlRequest) -> ControlResponse {
    let mut stream = UnixStream::connect(path).await.unwrap();
    let body = serde_json::to_vec(request).unwrap();
    crate::clawd::transport::frame::write_request_async(&mut stream, &body)
        .await
        .unwrap();
    let body =
        crate::clawd::transport::frame::read_response_async(&mut stream, MAX_CONTROL_FRAME_BYTES)
            .await
            .unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn route_spoofing_cross_session_and_grant_replay_are_rejected() {
    let state = state();
    let first = make_request(&state);
    assert!(validate_request(&first, process(), &state).is_ok());
    assert!(validate_request(&first, process(), &state)
        .unwrap_err()
        .contains("already used"));

    let mut cross_session = make_request(&state);
    cross_session.session_id = Some("session-b".to_string());
    assert!(validate_request(&cross_session, process(), &state)
        .unwrap_err()
        .contains("host lease"));

    let mut wrong_nonce = make_request(&state);
    wrong_nonce.lease_nonce = "ffffffffffffffffffffffffffffffff".to_string();
    assert!(validate_request(&wrong_nonce, process(), &state)
        .unwrap_err()
        .contains("host lease"));

    let mut substituted_binding = make_request(&state);
    substituted_binding.binding_digest = "f".repeat(64);
    assert!(validate_request(&substituted_binding, process(), &state)
        .unwrap_err()
        .contains("host lease"));
}

#[test]
fn a_v8_ping_is_rejected_before_route_or_replay_state_is_used() {
    let state = state();
    let mut old = make_request(&state);
    old.protocol = PROTOCOL_VERSION - 1;
    let error = validate_request(&old, process(), &state).unwrap_err();
    assert!(error.contains("protocol mismatch"), "{error}");
    assert!(error.contains("v8"), "{error}");
    assert!(error.contains("v9"), "{error}");

    old.protocol = PROTOCOL_VERSION;
    validate_request(&old, process(), &state)
        .expect("version rejection must not consume the request id");
}

#[test]
fn another_process_cannot_drive_the_host_control_socket() {
    let state = state();
    let mut other = process();
    other.pid = other.pid.saturating_add(1);
    assert!(validate_request(&make_request(&state), other, &state)
        .unwrap_err()
        .contains("different controller"));
}

#[test]
fn environment_names_and_arguments_are_bounded() {
    assert!(validate_name("server_1", "server").is_ok());
    assert!(validate_name("../escape", "server").is_err());
    assert!(validate_args(&vec!["x".to_string(); MAX_APP_ARGS]).is_ok());
    assert!(validate_args(&vec!["x".to_string(); MAX_APP_ARGS + 1]).is_err());
}

#[tokio::test]
async fn cancellation_aborts_the_exact_active_request() {
    let state = state();
    let id = RequestId::generate();
    let task = tokio::spawn(async {
        std::future::pending::<()>().await;
    });
    state
        .active
        .lock()
        .unwrap()
        .insert(id.as_str().to_string(), task.abort_handle());
    assert!(cancel_active(&state, &id));
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!cancel_active(&state, &id));
}

#[tokio::test]
async fn saturated_events_and_stalled_readers_cannot_starve_canonical_or_priority_control() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("control.sock");
    let event_path = PathBuf::from(control_socket_for(
        &base.to_string_lossy(),
        ControlLane::AgentEvent,
    ));
    let priority_path = PathBuf::from(control_socket_for(
        &base.to_string_lossy(),
        ControlLane::Priority,
    ));
    let mut test_state = state();
    test_state.binding.control_socket = base.to_string_lossy().into_owned();
    let state = Arc::new(test_state);

    let canonical_actions = Arc::new(Semaphore::new(MAX_CANONICAL_CONTROL_ACTIONS));
    let event_actions = Arc::new(Semaphore::new(MAX_AGENT_EVENT_ACTIONS));
    let priority_actions = Arc::new(Semaphore::new(MAX_PRIORITY_CONTROL_ACTIONS));
    let canonical = tokio::spawn(accept_control(
        bind_control_listener(&base, 0o660).unwrap(),
        ControlLane::Canonical,
        state.clone(),
        Arc::new(Semaphore::new(MAX_CANONICAL_ADMISSIONS)),
        canonical_actions.clone(),
    ));
    let events = tokio::spawn(accept_control(
        bind_control_listener(&event_path, 0o660).unwrap(),
        ControlLane::AgentEvent,
        state.clone(),
        Arc::new(Semaphore::new(MAX_AGENT_EVENT_ADMISSIONS)),
        event_actions.clone(),
    ));
    let priority = tokio::spawn(accept_control(
        bind_control_listener(&priority_path, 0o660).unwrap(),
        ControlLane::Priority,
        state.clone(),
        Arc::new(Semaphore::new(MAX_PRIORITY_ADMISSIONS)),
        priority_actions,
    ));

    let saturation = event_actions
        .clone()
        .acquire_many_owned(MAX_AGENT_EVENT_ACTIONS as u32)
        .await
        .unwrap();
    let mut stalled = Vec::new();
    for _ in 0..MAX_AGENT_EVENT_ADMISSIONS {
        stalled.push(UnixStream::connect(&event_path).await.unwrap());
    }

    let mut app = make_request(&state);
    app.action = HostAction::McpDetach {
        server: "missing-app".to_string(),
    };
    let app_response = exchange(&base, &app).await;
    assert!(app_response.ok);
    assert_eq!(app_response.id, app.id);

    let mut mcp = make_request(&state);
    mcp.action = HostAction::McpDetach {
        server: "missing-mcp".to_string(),
    };
    let mcp_response = exchange(&base, &mcp).await;
    assert!(mcp_response.ok);
    assert_eq!(mcp_response.id, mcp.id);

    let mut detach = make_request(&state);
    detach.action = HostAction::AgentExtensionDetach {
        extension_id: "observer".to_string(),
        binding: abi_binding(&state, "observer"),
        reason: super::super::abi::ShutdownReason::Disabled,
    };
    let detach_response = exchange(&priority_path, &detach).await;
    assert!(detach_response.ok);
    assert_eq!(detach_response.id, detach.id);

    let canonical_saturation = canonical_actions
        .acquire_many_owned(MAX_CANONICAL_CONTROL_ACTIONS as u32)
        .await
        .unwrap();
    let mut wrong_lane = make_request(&state);
    wrong_lane.action = HostAction::AgentExtensionEvent {
        extension_id: "observer".to_string(),
        binding: abi_binding(&state, "observer"),
        event_id: "wrong-lane".to_string(),
        deadline_monotonic_ns: super::super::abi::MonotonicDeadlineNs::after(Duration::from_secs(
            1,
        ))
        .unwrap(),
        payload: super::super::abi::EventPayload::SessionStart {
            source: "test".to_string(),
            attended: false,
            delegated: false,
        },
        capability_refs: Vec::new(),
    };
    let wrong_lane_response = exchange(&base, &wrong_lane).await;
    assert!(!wrong_lane_response.ok);
    assert_eq!(
        wrong_lane_response.error_category,
        Some(ExtensionErrorCategory::Protocol)
    );
    assert!(wrong_lane_response
        .error
        .as_deref()
        .is_some_and(|error| error.contains("wrong control lane")));
    drop(canonical_saturation);

    drop(stalled);
    tokio::time::sleep(CONTROL_READ_TIMEOUT + Duration::from_millis(50)).await;
    let mut event = make_request(&state);
    event.action = HostAction::AgentExtensionEvent {
        extension_id: "observer".to_string(),
        binding: abi_binding(&state, "observer"),
        event_id: "event-1".to_string(),
        deadline_monotonic_ns: super::super::abi::MonotonicDeadlineNs::after(Duration::from_secs(
            1,
        ))
        .unwrap(),
        payload: super::super::abi::EventPayload::SessionStart {
            source: "test".to_string(),
            attended: false,
            delegated: false,
        },
        capability_refs: Vec::new(),
    };
    let event_response = exchange(&event_path, &event).await;
    assert!(!event_response.ok);
    assert_eq!(event_response.id, event.id);
    assert_eq!(
        event_response.error_category,
        Some(ExtensionErrorCategory::Busy)
    );

    drop(saturation);
    canonical.abort();
    events.abort();
    priority.abort();
}

#[test]
fn control_capacity_is_globally_bounded_and_lane_separated() {
    assert_eq!(
        super::super::protocol::MAX_CONTROL_CONNECTIONS,
        MAX_CANONICAL_CONTROL_ACTIONS
            + MAX_PRIORITY_CONTROL_ACTIONS
            + MAX_AGENT_EVENT_ACTIONS
            + MAX_CANONICAL_ADMISSIONS
            + MAX_PRIORITY_ADMISSIONS
            + MAX_AGENT_EVENT_ADMISSIONS
    );
    assert!(MAX_AGENT_EVENT_ACTIONS >= 64);
    for action in [
        HostAction::Ping,
        HostAction::WarmApp {
            app_id: "app".to_string(),
        },
        HostAction::McpDetach {
            server: "mcp".to_string(),
        },
    ] {
        assert_eq!(action.control_lane(), ControlLane::Canonical);
    }
}

#[test]
fn interrupted_detach_acknowledges_only_proven_process_and_package_cleanup() {
    let process_error = detached_after_cleanup(Err("root child survived".to_string()))
        .expect_err("surviving child cannot be acknowledged");
    assert_eq!(
        process_error.category,
        ExtensionErrorCategory::RemoteCallFailure
    );
    assert!(process_error.message.contains("root child survived"));

    let package_error = detached_after_cleanup(Err("materialized package survived".to_string()))
        .expect_err("surviving package cannot be acknowledged");
    assert!(package_error
        .message
        .contains("materialized package survived"));

    assert!(matches!(
        detached_after_cleanup(Ok(())),
        Ok(HostResult::AgentExtensionDetached { detached: true })
    ));
}

#[tokio::test]
async fn task_and_service_hosts_cannot_swap_app_actions() {
    let task = Arc::new(state());
    let error = dispatch(
        HostAction::WarmApp {
            app_id: "notes".to_string(),
        },
        task,
    )
    .await
    .expect_err("task host must not warm a persistent service");
    assert!(error.contains("App service"), "{error}");

    let mut service = state();
    service.binding.purpose = super::super::protocol::HostPurpose::AppService;
    service.binding.app_id = Some("notes".to_string());
    service.binding.package = Some(crate::provenance::runtime::PackageRef {
        kind: crate::provenance::PackageKind::App,
        id: "notes".to_string(),
        content_digest: "a".repeat(64),
        publisher_key_id: None,
        tier: "system".to_string(),
    });
    let error = dispatch(
        HostAction::RunApp {
            app_id: "notes".to_string(),
            command: "show".to_string(),
            args: Vec::new(),
        },
        Arc::new(service),
    )
    .await
    .expect_err("service host must not run task actions");
    assert!(error.contains("cannot run App"), "{error}");
}
