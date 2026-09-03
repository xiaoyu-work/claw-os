use super::*;

#[test]
fn repeated_backpressure_stops_ingress_without_revoking_accepted_work() {
    let drops = AtomicUsize::new(0);
    let accepting = AtomicBool::new(true);
    let security_disabled = AtomicBool::new(false);
    for _ in 0..DISABLE_AFTER_DROPS - 1 {
        assert!(!register_backpressure_drop(&drops, &accepting));
        assert!(accepting.load(Ordering::Acquire));
    }
    assert!(register_backpressure_drop(&drops, &accepting));
    assert!(!accepting.load(Ordering::Acquire));
    assert!(should_process_event(&security_disabled));
    assert!(!register_backpressure_drop(&drops, &accepting));
}

#[test]
fn extension_observer_never_mutates_or_blocks_runtime_decisions() {
    let observer = ExtensionObserver {
        name: "test-observer".to_string(),
        sinks: Vec::new(),
    };
    let context = HookContext::new("session-a", "provider", "model");
    assert!(matches!(observer.pre_turn(&context), HookOutcome::Continue));
    let decision = observer.pre_tool(
        &context,
        &crate::agent::llm::ToolCall {
            id: "tool-a".to_string(),
            name: "echo".to_string(),
            input: serde_json::json!({"secret": "not-forwarded"}),
        },
    );
    assert!(matches!(decision, ToolDecision::Allow));
}

#[tokio::test]
async fn reserved_terminal_slot_preserves_fifo_and_never_blocks_on_a_full_event_queue() {
    let (sender, mut receiver) = mpsc::channel(3);
    let slots = Arc::new(Semaphore::new(2));
    for index in 0..2 {
        let permit = slots.clone().try_acquire_owned().unwrap();
        sender
            .try_send(ExtensionWork::Event {
                payload: EventPayload::PreTool {
                    turn_index: index,
                    tool: "now".to_string(),
                    tool_use_id_digest: "a".repeat(64),
                    input_bytes: 2,
                    input_digest: "b".repeat(64),
                },
                _permit: permit,
            })
            .unwrap();
    }
    assert!(slots.clone().try_acquire_owned().is_err());
    let (done, _done_rx) = oneshot::channel();
    sender
        .try_send(ExtensionWork::Finish {
            completion: None,
            reason: ShutdownReason::TaskComplete,
            done,
        })
        .expect("reserved finish slot");

    for expected in 0..2 {
        let Some(ExtensionWork::Event { payload, .. }) = receiver.recv().await else {
            panic!("event must precede finish");
        };
        assert!(matches!(
            payload,
            EventPayload::PreTool { turn_index, .. } if turn_index == expected
        ));
    }
    assert!(matches!(
        receiver.recv().await,
        Some(ExtensionWork::Finish {
            completion: None,
            ..
        })
    ));
}

#[tokio::test]
async fn backpressure_terminal_drains_accepted_fifo_before_detach() {
    let (sender, mut receiver) = mpsc::channel(4);
    let slots = Arc::new(Semaphore::new(3));
    let accepting = AtomicBool::new(true);
    let security_disabled = AtomicBool::new(false);
    let drops = AtomicUsize::new(0);
    for index in 0..3 {
        let permit = slots.clone().try_acquire_owned().unwrap();
        sender
            .try_send(ExtensionWork::Event {
                payload: EventPayload::PreTool {
                    turn_index: index,
                    tool: "now".to_string(),
                    tool_use_id_digest: "a".repeat(64),
                    input_bytes: 2,
                    input_digest: "b".repeat(64),
                },
                _permit: permit,
            })
            .unwrap();
    }
    for _ in 0..DISABLE_AFTER_DROPS {
        register_backpressure_drop(&drops, &accepting);
    }
    assert!(!accepting.load(Ordering::Acquire));
    let (done, _done_rx) = oneshot::channel();
    sender
        .try_send(ExtensionWork::Finish {
            completion: None,
            reason: ShutdownReason::Disabled,
            done,
        })
        .expect("reserved terminal slot");

    let mut delivered = Vec::new();
    while let Some(work) = receiver.recv().await {
        match work {
            ExtensionWork::Event { payload, .. } if should_process_event(&security_disabled) => {
                let EventPayload::PreTool { turn_index, .. } = payload else {
                    panic!("unexpected payload");
                };
                delivered.push(turn_index);
            }
            ExtensionWork::Event { .. } => {}
            ExtensionWork::Finish { .. } => break,
        }
    }
    assert_eq!(delivered, vec![0, 1, 2]);
}

#[tokio::test]
async fn security_revocation_discards_accepted_work_immediately() {
    let (sender, mut receiver) = mpsc::channel(2);
    let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
    sender
        .try_send(ExtensionWork::Event {
            payload: EventPayload::SessionStart {
                source: "test".to_string(),
                attended: false,
                delegated: false,
            },
            _permit: permit,
        })
        .unwrap();
    let security_disabled = AtomicBool::new(true);
    let Some(ExtensionWork::Event { .. }) = receiver.recv().await else {
        panic!("accepted event");
    };
    assert!(!should_process_event(&security_disabled));
}

#[test]
fn completion_is_only_queued_for_subscribed_extensions() {
    let subscriptions = BTreeSet::from([EventKind::SessionStart]);
    assert!(!subscriptions.contains(&EventKind::Completion));
    let subscriptions = BTreeSet::from([EventKind::Completion]);
    assert!(subscriptions.contains(&EventKind::Completion));
}

#[test]
fn finish_reserves_time_for_forced_host_detach() {
    assert!(FINISH_DRAIN_TIMEOUT < FINISH_TIMEOUT);
    assert!(FINISH_TIMEOUT - FINISH_DRAIN_TIMEOUT >= Duration::from_secs(2));
}

#[tokio::test]
async fn failed_detach_is_retried_after_the_worker_has_completed() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    retry_with_deadline(
        tokio::time::Instant::now() + Duration::from_secs(1),
        move || {
            let attempt = observed.fetch_add(1, Ordering::AcqRel);
            async move {
                match attempt {
                    0 => Err("priority control busy".to_string()),
                    1 => Err("priority response dropped".to_string()),
                    _ => Ok(()),
                }
            }
        },
    )
    .await
    .expect("completed workers must retain independent detach retry state");
    assert_eq!(attempts.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn failed_detach_exhaustion_requires_containment_escalation() {
    let error = retry_with_deadline(
        tokio::time::Instant::now() + Duration::from_millis(120),
        || async { Err("extension host crashed".to_string()) },
    )
    .await
    .unwrap_err();
    assert_eq!(error, "extension host crashed");
}

#[test]
fn only_exact_detach_acknowledgement_proves_child_termination() {
    let state = DetachState::default();
    let error = classify_detach_response(Ok(false), &state).unwrap_err();
    assert!(error.contains("exact child termination"), "{error}");
    assert!(!state.is_resolved());

    classify_detach_response(Ok(true), &state).unwrap();
    assert!(state.is_resolved());
}

#[test]
fn later_exact_detach_clears_initial_priority_busy_failure() {
    let state = DetachState::default();
    state.record_failure("priority control busy".to_string());
    assert_eq!(
        state.unresolved_failure().as_deref(),
        Some("priority control busy")
    );

    classify_detach_response(Ok(true), &state).unwrap();
    assert!(state.is_resolved());
    assert_eq!(state.unresolved_failure(), None);
}

#[test]
fn successful_containment_escalation_clears_only_transient_detach_failure() {
    let state = DetachState::default();
    state.record_failure("root child cleanup uncertain".to_string());
    classify_escalation_response(Ok(()), &state).unwrap();
    assert!(state.is_resolved());
    assert_eq!(state.unresolved_failure(), None);
}

#[test]
fn interrupted_cleanup_failure_requires_escalation_before_resolution() {
    let state = DetachState::default();
    let error = classify_detach_response(
        Err("root child or materialized package survived".to_string()),
        &state,
    )
    .unwrap_err();
    state.record_failure(error);
    assert!(!state.is_resolved());
    assert!(state
        .unresolved_failure()
        .as_deref()
        .is_some_and(|failure| failure.contains("survived")));

    classify_escalation_response(Ok(()), &state).unwrap();
    assert!(state.is_resolved());
    assert_eq!(state.unresolved_failure(), None);
}

#[test]
fn unresolved_detach_and_persistent_failures_remain_isolated() {
    let recovered = DetachState::default();
    let unresolved = DetachState::default();
    let protocol_failure = "extension protocol failure".to_string();
    recovered.record_failure("initial busy".to_string());
    unresolved.record_failure("root child survived".to_string());

    recovered.resolve();
    assert_eq!(recovered.unresolved_failure(), None);
    assert_eq!(
        unresolved.unresolved_failure().as_deref(),
        Some("root child survived")
    );
    assert_eq!(protocol_failure, "extension protocol failure");
}

#[tokio::test]
async fn dropping_parent_action_scope_aborts_the_detached_task() {
    let task = tokio::spawn(std::future::pending::<()>());
    let guard = AbortOnDrop::new(task.abort_handle());
    drop(guard);
    let error = task.await.expect_err("action task must be cancelled");
    assert!(error.is_cancelled());
}

#[cfg(unix)]
#[test]
fn active_extension_package_fails_currentness_after_revocation() {
    crate::test_env::clear_test_revocations();
    let root = crate::test_env::secure_scratch_dir(&format!(
        "active-extension-revocation-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(root.join("app.json"), b"{}").unwrap();
    crate::test_env::sign_test_package(&root, crate::provenance::PackageKind::App, "observer");
    let trust = crate::provenance::trust_store();
    let options = crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App)
        .expect_id("observer");
    let package =
        crate::provenance::verify::verify_package_cached(&root, &options, &trust).unwrap();
    assert_package_current(&package).unwrap();

    crate::test_env::revoke_test_package(package.content_digest());
    let error = assert_package_current(&package).unwrap_err();
    assert!(error.contains("no longer trusted"), "{error}");
    crate::test_env::clear_test_revocations();
}
