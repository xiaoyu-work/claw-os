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
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: worker_uid,
        extension_uid: 61_000,
        owner_gid,
        capability_generation: "a".repeat(16),
        approved_paths: approved_paths.clone(),
        agent_extensions: Vec::new(),
        worker_pid: pid,
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
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
        worker_uid,
        owner_gid,
        worker_pid: pid,
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        recent: Mutex::new(VecDeque::new()),
        active: Mutex::new(HashMap::new()),
        mcp: tokio::sync::Mutex::new(HashMap::new()),
        agent_extensions: tokio::sync::Mutex::new(HashMap::new()),
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
        .contains("task lease"));

    let mut wrong_nonce = make_request(&state);
    wrong_nonce.lease_nonce = "ffffffffffffffffffffffffffffffff".to_string();
    assert!(validate_request(&wrong_nonce, process(), &state)
        .unwrap_err()
        .contains("task lease"));

    let mut substituted_binding = make_request(&state);
    substituted_binding.binding_digest = "f".repeat(64);
    assert!(validate_request(&substituted_binding, process(), &state)
        .unwrap_err()
        .contains("task lease"));
}

#[test]
fn another_process_cannot_drive_the_host_control_socket() {
    let state = state();
    let mut other = process();
    other.pid = other.pid.saturating_add(1);
    assert!(validate_request(&make_request(&state), other, &state)
        .unwrap_err()
        .contains("different worker"));
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
