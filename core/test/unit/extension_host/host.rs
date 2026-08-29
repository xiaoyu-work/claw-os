use super::*;

fn state() -> HostState {
    let pid = std::process::id();
    HostState {
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: unsafe { libc::geteuid() },
        owner_gid: unsafe { libc::getegid() },
        worker_pid: pid,
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        recent: Mutex::new(VecDeque::new()),
        active: Mutex::new(HashMap::new()),
        mcp: tokio::sync::Mutex::new(HashMap::new()),
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
