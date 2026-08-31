use super::*;

fn binding() -> ExtensionBinding {
    let pid = std::process::id();
    ExtensionBinding {
        protocol: PROTOCOL_VERSION,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: unsafe { libc::geteuid() },
        extension_uid: 61_184,
        owner_gid: unsafe { libc::getegid() },
        worker_pid: pid,
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
        host_pid: pid.saturating_add(1),
        host_start_time_ticks: Some(42),
        lease_nonce: "0123456789abcdef0123456789abcdef".to_string(),
        expires_at_ms: crate::agentd::grant::now_ms() + 60_000,
        control_socket: "/run/cos/test/control.sock".to_string(),
        broker_socket: "/run/cos/test/broker.sock".to_string(),
    }
}

#[test]
fn binding_rejects_replay_against_another_worker() {
    let binding = binding();
    assert!(binding
        .validate_worker(binding.worker_pid, binding.worker_start_time_ticks,)
        .is_ok());
    let error = binding
        .validate_worker(
            binding.worker_pid.saturating_add(1),
            binding.worker_start_time_ticks,
        )
        .expect_err("another worker must not reuse the binding");
    assert!(error.contains("different worker"), "{error}");
}

#[test]
fn control_requests_carry_the_exact_task_session_and_lease() {
    let binding = binding();
    let request = ControlRequest::new(&binding, HostAction::Ping, 5000);
    assert_eq!(request.task_id, binding.task_id);
    assert_eq!(request.session_id, binding.session_id);
    assert_eq!(request.lease_nonce, binding.lease_nonce);
    assert!(request.timeout_ms <= super::super::protocol::MAX_REQUEST_TIMEOUT_MS);
}

#[test]
fn lifecycle_events_are_forwarded_as_typed_worker_audit() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let client = ExtensionHostClient {
        binding: binding(),
        audit: Some(tx),
    };
    client.emit(
        super::super::protocol::ExtensionKind::App,
        super::super::protocol::LifecycleAction::Call,
        "echo-app",
        Some("abcd"),
        false,
        Duration::from_millis(7),
        Some("untrusted failure"),
    );
    let frame = rx.try_recv().expect("audit frame");
    let crate::agentd::protocol::WorkerFrame::Audit { record, .. } = frame else {
        panic!("expected extension audit");
    };
    let crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
        kind,
        action,
        extension_id,
        manifest_digest,
        success,
        latency_ms,
        error,
        ..
    } = *record
    else {
        panic!("expected extension lifecycle record");
    };
    assert_eq!(kind, super::super::protocol::ExtensionKind::App);
    assert_eq!(action, super::super::protocol::LifecycleAction::Call);
    assert_eq!(extension_id, "echo-app");
    assert_eq!(manifest_digest.as_deref(), Some("abcd"));
    assert!(!success);
    assert_eq!(latency_ms, 7);
    assert!(error.is_some());
}
