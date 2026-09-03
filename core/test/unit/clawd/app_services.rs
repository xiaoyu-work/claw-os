use super::*;

fn context() -> McpCallContext {
    McpCallContext {
        wire_version: crate::agent::tools::app_gateway::CALL_CONTEXT_WIRE_VERSION,
        call_id: "call-a".to_string(),
        trace_id: "trace-a".to_string(),
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 60_000),
        session_id: Some("session-a".to_string()),
        task_id: Some("task-a".to_string()),
        caller: crate::agent::tools::app_gateway::McpPrincipal {
            kind: crate::agent::tools::app_gateway::McpPrincipalKind::SystemAgent,
            id: "session-a".to_string(),
            owner_uid: 1000,
            app_id: None,
        },
    }
}

#[test]
fn action_digest_binds_every_executable_call_field() {
    let context = context();
    let arguments = serde_json::json!({"folder": "inbox", "limit": 10});
    let expected =
        app_call_action_digest("mail", "messages.list", &arguments, &context, &[]).expect("digest");

    assert_eq!(
        expected,
        app_call_action_digest("mail", "messages.list", &arguments, &context, &[])
            .expect("same digest")
    );
    assert_ne!(
        expected,
        app_call_action_digest("mail", "messages.delete", &arguments, &context, &[])
            .expect("tool digest")
    );
    assert_ne!(
        expected,
        app_call_action_digest(
            "mail",
            "messages.list",
            &serde_json::json!({"folder": "trash", "limit": 10}),
            &context,
            &[],
        )
        .expect("argument digest")
    );
    let mut other_context = context.clone();
    other_context.call_id = "call-b".to_string();
    assert_ne!(
        expected,
        app_call_action_digest("mail", "messages.list", &arguments, &other_context, &[])
            .expect("context digest")
    );
    let mount = crate::worker::AuthorizedMount {
        source: "/tmp/inbox".into(),
        target: "/tmp/inbox".into(),
        mode: crate::worker::MountMode::ReadOnly,
        class: crate::worker::MountClass::Input,
        device: 1,
        inode: 2,
    };
    assert_ne!(
        expected,
        app_call_action_digest("mail", "messages.list", &arguments, &context, &[mount],)
            .expect("mount digest")
    );
}

#[test]
fn restart_budget_is_bounded_and_recovers_after_the_window() {
    let mut slot = ServiceSlot::new(McpLifecycle::AlwaysOn);
    for _ in 0..RESTART_LIMIT {
        slot.record_failure();
    }
    assert!(!slot.may_restart());
    slot.failures = VecDeque::from([Instant::now() - RESTART_WINDOW - Duration::from_secs(1)]);
    assert!(slot.may_restart());
}

#[test]
fn unexpected_host_exits_consume_the_restart_budget() {
    let mut slot = ServiceSlot::new(McpLifecycle::AlwaysOn);
    for _ in 0..RESTART_LIMIT {
        slot.record_host_exit();
    }
    assert!(!slot.may_restart());
}

#[test]
fn host_startup_failures_but_not_admission_refusals_consume_the_restart_budget() {
    let mut slot = ServiceSlot::new(McpLifecycle::AlwaysOn);
    slot.record_start_failure(&RuntimeStartError::admission("package changed"));
    assert!(slot.failures.is_empty());

    for _ in 0..RESTART_LIMIT {
        slot.record_start_failure(&RuntimeStartError::host("controller connect failed"));
    }
    assert!(!slot.may_restart());
}

#[test]
fn only_controller_failures_retire_a_service_host() {
    use crate::extension_host::protocol::ExtensionErrorCategory;

    assert!(!host_fault_requires_retirement(
        ExtensionErrorCategory::RemoteCallFailure
    ));
    for category in [
        ExtensionErrorCategory::Connect,
        ExtensionErrorCategory::Timeout,
        ExtensionErrorCategory::Crash,
        ExtensionErrorCategory::Protocol,
    ] {
        assert!(host_fault_requires_retirement(category));
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn zombie_service_hosts_are_detected_and_reaped() {
    let mut child = tokio::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn child");
    let pid = child.id().expect("child pid");
    let start_time = crate::proc::read_start_time_ticks_pub(pid).expect("child start time");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                let tail = stat.get(stat.rfind(')')? + 1..)?.trim();
                tail.split_whitespace().next()?.chars().next()
            });
        if state == Some('Z') {
            break;
        }
        assert!(Instant::now() < deadline, "child did not become a zombie");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    assert_eq!(
        crate::proc::read_start_time_ticks_pub(pid),
        Some(start_time),
        "a zombie still has the original pid/start-time identity"
    );
    assert!(child_process_exited(&mut child, pid, Some(start_time)));
    assert!(child.try_wait().expect("poll reaped child").is_some());
}
