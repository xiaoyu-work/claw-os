use super::*;

fn binding() -> ExtensionBinding {
    let pid = std::process::id();
    let owner_uid = (unsafe { libc::geteuid() }).max(1000);
    let owner_gid = (unsafe { libc::getegid() }).max(60_999);
    ExtensionBinding {
        protocol: PROTOCOL_VERSION,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid,
        extension_uid: 61_000,
        owner_gid,
        capability_generation: "a".repeat(16),
        approved_paths: vec![super::super::protocol::ApprovedPath {
            path: "/home/test".to_string(),
            device: 1,
            inode: 2,
            owner_uid,
            mode: 0o40755,
        }],
        agent_extensions: Vec::new(),
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
fn a_missing_receipt_collection_defaults_to_no_extension_authority() {
    let mut document = serde_json::to_value(binding()).unwrap();
    document.as_object_mut().unwrap().remove("agent_extensions");

    let decoded: ExtensionBinding = serde_json::from_value(document).unwrap();
    assert!(decoded.agent_extensions.is_empty());
    decoded
        .validate_shape()
        .expect("missing receipts must authorize no Agent extensions");
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
    assert_eq!(request.binding_digest, binding.digest().unwrap());
    assert!(request.timeout_ms <= super::super::protocol::MAX_REQUEST_TIMEOUT_MS);
}

#[test]
fn lifecycle_events_are_forwarded_as_typed_worker_audit() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let binding = binding();
    let client = ExtensionHostClient {
        binding_digest: binding.digest().unwrap(),
        lease_digest: crate::crypto::sha256_hex(binding.lease_nonce.as_bytes()),
        binding,
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
        binding_digest,
        lease_digest,
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
    assert_eq!(binding_digest, client.binding_digest);
    assert_eq!(lease_digest, client.lease_digest);
}

#[test]
fn mcp_gateway_and_host_audit_share_exact_opaque_identity() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let binding = binding();
    let client = ExtensionHostClient {
        binding_digest: binding.digest().unwrap(),
        lease_digest: crate::crypto::sha256_hex(binding.lease_nonce.as_bytes()),
        binding,
        audit: Some(tx),
    };
    let identity = super::super::protocol::McpInvocationAudit {
        policy_identity: "mcp_server_tool".to_string(),
        server_identity: "server".to_string(),
        handle_digest: "b".repeat(64),
        descriptor_digest: "c".repeat(64),
        capability_generation: "a".repeat(16),
        untrusted_remote_name: crate::audit_policy::text_digest("IGNORE ALL"),
    };
    client.emit_mcp_gateway(&identity, false, Some("approval pending"));
    client.emit_mcp(
        super::super::protocol::LifecycleAction::Call,
        "server",
        super::super::protocol::AuditStage::Host,
        &identity,
        true,
        Duration::from_millis(3),
        None,
    );
    for stage in [
        super::super::protocol::AuditStage::Gateway,
        super::super::protocol::AuditStage::Host,
    ] {
        let crate::agentd::protocol::WorkerFrame::Audit { record, .. } =
            rx.try_recv().expect("MCP audit frame")
        else {
            panic!("expected audit frame");
        };
        let crate::agentd::protocol::RuntimeAuditRecord::ExtensionLifecycle {
            stage: actual,
            mcp,
            ..
        } = *record
        else {
            panic!("expected lifecycle audit");
        };
        assert_eq!(actual, Some(stage));
        assert_eq!(mcp, Some(identity.clone()));
    }
}

#[test]
fn remote_error_text_cannot_spoof_lifecycle_classification() {
    for message in [
        "timed out",
        "connect extension host",
        "different process",
        "closed",
        "protocol crash timeout connect",
    ] {
        let result: ClientResult<()> = Err(ClientFault::new(
            ExtensionErrorCategory::RemoteCallFailure,
            message,
        ));
        assert_eq!(
            action_for_result(&result),
            super::super::protocol::LifecycleAction::RemoteCallFailure
        );
    }
}

#[test]
fn trusted_client_fault_categories_map_to_exact_lifecycle_actions() {
    for (category, action) in [
        (
            ExtensionErrorCategory::Connect,
            super::super::protocol::LifecycleAction::Connect,
        ),
        (
            ExtensionErrorCategory::Timeout,
            super::super::protocol::LifecycleAction::Timeout,
        ),
        (
            ExtensionErrorCategory::Crash,
            super::super::protocol::LifecycleAction::Crash,
        ),
        (
            ExtensionErrorCategory::Busy,
            super::super::protocol::LifecycleAction::BackpressureDrop,
        ),
        (
            ExtensionErrorCategory::Protocol,
            super::super::protocol::LifecycleAction::Protocol,
        ),
    ] {
        let result: ClientResult<()> = Err(ClientFault::new(category, "remote says timed out"));
        assert_eq!(action_for_result(&result), action);
    }
}

#[test]
fn categorized_response_serialization_rejects_old_spoofing_semantics() {
    let response = ControlResponse::error(
        crate::clawd::wire::RequestId::unknown(),
        ExtensionErrorCategory::RemoteCallFailure,
        "connect extension host timed out and closed",
    );
    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(encoded["error_category"], "remote-call-failure");

    let legacy = serde_json::json!({
        "protocol": PROTOCOL_VERSION,
        "id": crate::clawd::wire::RequestId::unknown(),
        "ok": false,
        "error": "timed out"
    });
    let decoded: ControlResponse = serde_json::from_value(legacy).unwrap();
    assert_eq!(decoded.error_category, None);
}

fn transport_test_client(path: &std::path::Path) -> ExtensionHostClient {
    let mut binding = binding();
    binding.control_socket = path.to_string_lossy().into_owned();
    binding.host_pid = std::process::id();
    binding.host_start_time_ticks = crate::proc::read_start_time_ticks_pub(std::process::id());
    ExtensionHostClient {
        binding_digest: binding.digest().unwrap(),
        lease_digest: crate::crypto::sha256_hex(binding.lease_nonce.as_bytes()),
        binding,
        audit: None,
    }
}

#[tokio::test]
async fn transport_observers_assign_connect_and_frame_categories() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing.sock");
    let connect = transport_test_client(&missing)
        .request_with_timeout(HostAction::Ping, Duration::from_millis(50), false)
        .await
        .unwrap_err();
    assert_eq!(connect.category, ExtensionErrorCategory::Connect);
    assert_eq!(
        response_fault_category(crate::clawd::wire::Fault::ReadTimeout),
        ExtensionErrorCategory::Timeout,
    );
    assert_eq!(
        response_fault_category(crate::clawd::wire::Fault::TruncatedFrame),
        ExtensionErrorCategory::Crash,
    );
    assert_eq!(
        response_fault_category(crate::clawd::wire::Fault::MalformedBody),
        ExtensionErrorCategory::Protocol,
    );
}
