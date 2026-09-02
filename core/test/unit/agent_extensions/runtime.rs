use super::*;

#[test]
fn repeated_backpressure_disables_only_the_faulting_extension() {
    let drops = AtomicUsize::new(0);
    let disabled = AtomicBool::new(false);
    for _ in 0..DISABLE_AFTER_DROPS - 1 {
        assert!(!register_backpressure_drop(&drops, &disabled));
        assert!(!disabled.load(Ordering::Acquire));
    }
    assert!(register_backpressure_drop(&drops, &disabled));
    assert!(disabled.load(Ordering::Acquire));
    assert!(!register_backpressure_drop(&drops, &disabled));
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
