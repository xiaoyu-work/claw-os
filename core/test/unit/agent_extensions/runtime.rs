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
    assert!(matches!(
        observer.pre_turn(&context),
        HookOutcome::Continue
    ));
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
