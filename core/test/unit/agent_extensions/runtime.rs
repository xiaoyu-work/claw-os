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
