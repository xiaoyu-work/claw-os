use super::*;

/// The broker flag is process-wide, so these tests take the shared
/// env lock and always restore the default before returning.
struct BrokerFlagGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl BrokerFlagGuard {
    fn engaged() -> Self {
        let lock = crate::test_env::lock_env();
        mark_broker_process();
        Self { _lock: lock }
    }
}

impl Drop for BrokerFlagGuard {
    fn drop(&mut self) {
        clear_broker_process_for_test();
    }
}

#[test]
fn the_agent_runtime_is_available_outside_the_broker() {
    let _lock = crate::test_env::lock_env();
    clear_broker_process_for_test();
    assert!(!is_broker_process());
    assert!(ensure_agent_runtime_allowed("model provider construction").is_ok());
}

#[test]
fn the_model_and_tool_runtime_is_refused_inside_the_broker() {
    let _guard = BrokerFlagGuard::engaged();
    assert!(is_broker_process());

    // Every surface that would pull provider transport, MCP, dynamic
    // App execution or a model-visible tool loop into the root process.
    let provider = crate::ai::gate::build_system_provider(&crate::config::AgentConfig::default());
    assert!(
        provider.is_err(),
        "the broker must not be able to build a model provider"
    );

    let worker = crate::agent::service::run_worker_loop(
        crate::agent::service::WorkerOptions {
            once: true,
            poll_ms: 1,
            max_jobs: Some(1),
        },
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    assert!(
        worker.is_err(),
        "the broker must not be able to run the in-process agent loop"
    );

    // A real signed package, so the refusal comes from the broker guard
    // rather than from provenance failing first — the point of the test
    // is that even a perfectly valid App cannot be launched here.
    let dir = crate::test_env::secure_scratch_dir("agentd-guard-app");
    std::fs::write(
        dir.join("app.json"),
        r#"{"id":"guarded","version":"1","name": {"en": "Guarded"},"operations":{}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.py"),
        "def run(command, args):\n    return {}\n",
    )
    .unwrap();
    let launch = crate::test_env::app_launch(&dir, "guarded");

    let app = crate::bridge::run_python_app(
        &launch,
        "noop",
        &[],
        "/nonexistent-data",
        "/nonexistent-apps",
    );
    assert!(
        app.is_err(),
        "the broker must not be able to launch a Python App"
    );
    let message = app.unwrap_err();
    assert!(
        message.contains("claw-agentd"),
        "the refusal should name the worker: {message}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
