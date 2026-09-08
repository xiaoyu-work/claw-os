use super::*;

#[test]
fn provider_mode_classification() {
    assert_eq!(classify_provider(Some("ollama"), None), AgentMode::Local);
    assert_eq!(
        classify_provider(Some("llama_local"), None),
        AgentMode::Local
    );
    assert_eq!(
        classify_provider(Some("openai_compat"), Some("http://127.0.0.1:11434/v1")),
        AgentMode::Local,
    );
    assert_eq!(
        classify_provider(Some("openai_compat"), Some("http://localhost:8080/v1")),
        AgentMode::Local,
    );
    assert_eq!(classify_provider(Some("openai"), None), AgentMode::Cloud);
    assert_eq!(classify_provider(Some("copilot"), None), AgentMode::Cloud);
    assert_eq!(
        classify_provider(Some("none"), None),
        AgentMode::Unconfigured
    );
    assert_eq!(classify_provider(None, None), AgentMode::Unconfigured);
}

/// `cos agent show` envelope. We only require the fields we
/// actually render — extra fields in the kernel response are
/// allowed (forward-compat for new info the kernel may add).
#[test]
fn parses_real_kernel_show_response() {
    let raw = r#"{
        "id": "ses_0019e2566eb1f_e71a8d6a8ca4",
        "purpose": "rebuild reports",
        "status": "running",
        "role": "worker",
        "parent_session": null,
        "creator_runtime": "cos-agent",
        "budget": {},
        "created_at": "2025-01-01T00:00:00Z",
        "ended_at": null,
        "lease": null,
        "turns": {"count": 7, "first_at": "2025-01-01T00:00:00Z", "last_at": "2025-01-01T00:01:30Z"},
        "mutations": {"count": 3, "by_kind": {"fs.write": 2, "fs.rename": 1}},
        "stop_requested": false
    }"#;
    let detail: TaskDetail = serde_json::from_str(raw).unwrap();
    assert_eq!(detail.turns.count, 7);
    assert_eq!(detail.mutations.count, 3);
    assert!(!detail.stop_requested);
}
