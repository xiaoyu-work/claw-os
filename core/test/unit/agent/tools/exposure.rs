use super::*;

fn cap(verb: Verb, scope: crate::caps::Scope) -> CapSet {
    CapSet::from_caps([Cap::new(verb, scope)])
}

#[test]
fn exact_capability_scope_is_recomputed_per_context() {
    let rule = ToolExposure::always().requiring_caps([Cap::new(
        Verb::FS_READ,
        crate::caps::Scope::path("/home/alice/**"),
    )]);
    let alice = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("alice-session", 1000, SessionSource::BrokerTask)
        .with_capabilities(cap(
            Verb::FS_READ,
            crate::caps::Scope::path("/home/alice/**"),
        ));
    let bob = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("bob-session", 1001, SessionSource::BrokerTask)
        .with_capabilities(cap(
            Verb::FS_READ,
            crate::caps::Scope::path("/home/bob/**"),
        ));

    assert_eq!(rule.decide(&alice), ExposureDecision::Visible);
    assert!(matches!(
        rule.decide(&bob),
        ExposureDecision::Hidden(_)
    ));
    assert_ne!(alice.capability_generation(), bob.capability_generation());
}

#[test]
fn any_exact_capability_requires_one_compatible_scope() {
    let rule = ToolExposure::always().requiring_any_cap([
        Cap::new(Verb::AI_AUDIO_TTS, crate::caps::Scope::name("provider-a")),
        Cap::new(Verb::AI_AUDIO_TTS, crate::caps::Scope::name("provider-b")),
    ]);
    let provider_a = ToolExposureContext::isolated(Guardrails::permissive())
        .with_capabilities(cap(
            Verb::AI_AUDIO_TTS,
            crate::caps::Scope::name("provider-a"),
        ));
    let provider_c = ToolExposureContext::isolated(Guardrails::permissive())
        .with_capabilities(cap(
            Verb::AI_AUDIO_TTS,
            crate::caps::Scope::name("provider-c"),
        ));

    assert_eq!(rule.decide(&provider_a), ExposureDecision::Visible);
    assert!(matches!(
        rule.decide(&provider_c),
        ExposureDecision::Hidden(_)
    ));
    assert!(matches!(
        ToolExposure::always()
            .requiring_any_cap([])
            .decide(&provider_a),
        ExposureDecision::Hidden(_)
    ));
}

#[test]
fn source_presence_and_transport_are_independent_requirements() {
    let rule = ToolExposure::always()
        .from_sources([SessionSource::LocalCli])
        .requiring_attended_local()
        .requiring_transport(ToolTransport::AppSession);
    let visible = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("cli", 1000, SessionSource::LocalCli)
        .with_presence(true, true)
        .with_transport(ToolTransport::AppSession, true);
    let wrong_source = visible
        .clone()
        .with_identity("mcp", 1000, SessionSource::ExternalMcp);
    let unattended = visible.clone().with_presence(false, true);
    let unreachable = visible
        .clone()
        .with_transport(ToolTransport::AppSession, false);

    assert_eq!(rule.decide(&visible), ExposureDecision::Visible);
    assert!(matches!(
        rule.decide(&wrong_source),
        ExposureDecision::Hidden(_)
    ));
    assert!(matches!(
        rule.decide(&unattended),
        ExposureDecision::Hidden(_)
    ));
    assert!(matches!(
        rule.decide(&unreachable),
        ExposureDecision::Hidden(_)
    ));
}

#[test]
fn delegated_context_drops_attended_authorization() {
    let parent = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("parent", 1000, SessionSource::LocalCli)
        .with_presence(true, true);
    let child = parent.delegated(Guardrails::permissive());

    assert_eq!(child.client().source, SessionSource::DelegatedAgent);
    assert!(!child.client().attended);
    assert!(!child.has_transport(ToolTransport::InteractiveAuthorization));
}

#[test]
fn process_environment_cannot_promote_an_exposure_context() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let previous_session = std::env::var_os("COS_SESSION");
    let previous_source = std::env::var_os("COS_AGENT_SOURCE");
    std::env::set_var("COS_SESSION", "forged-session");
    std::env::set_var("COS_AGENT_SOURCE", "local-cli");

    let context = ToolExposureContext::isolated(Guardrails::permissive());
    let rule = ToolExposure::always()
        .from_sources([SessionSource::LocalCli])
        .requiring_attended_local();
    assert!(matches!(
        rule.decide(&context),
        ExposureDecision::Hidden(_)
    ));
    assert_eq!(context.client().source, SessionSource::Unknown);

    match previous_session {
        Some(value) => std::env::set_var("COS_SESSION", value),
        None => std::env::remove_var("COS_SESSION"),
    }

    match previous_source {
        Some(value) => std::env::set_var("COS_AGENT_SOURCE", value),
        None => std::env::remove_var("COS_AGENT_SOURCE"),
    }
}

#[tokio::test]
async fn verified_worker_override_supplies_context_without_process_metadata() {
    let capabilities = cap(Verb::MEMORY_READ, crate::caps::Scope::Wild);
    let session = crate::proc::SessionInfo {
        session_id: "worker-session".to_string(),
        pid: 1,
        command: vec!["claw-agentd".to_string()],
        started_at: "2026-01-01T00:00:00Z".to_string(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("clawd".to_string()),
        parent: None,
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: Some("clawd-task".to_string()),
        priority: None,
        caps: Some(capabilities),
        transient_caps: None,
        role: None,
        app_id: None,
        pending_bind: false,
        start_time_ticks: None,
        client: SessionClient::new(SessionSource::BrokerTask, true, true),
    };
    let context = crate::proc::with_trusted_session_override(session, async {
        ToolExposureContext::from_current_session(
            Some("conversation"),
            Some("task"),
            ExecutionHost::AgentWorker,
            Guardrails::permissive(),
        )
        .unwrap()
    })
    .await;

    assert_eq!(context.authority_session_id(), "worker-session");
    assert_eq!(context.conversation_session_id(), Some("conversation"));
    assert_eq!(context.task_id(), Some("task"));
    assert_eq!(context.client().source, SessionSource::BrokerTask);
    assert_eq!(context.host(), ExecutionHost::AgentWorker);
    assert!(!context.has_transport(ToolTransport::AppSession));
}

#[test]
#[cfg(target_os = "linux")]
fn presence_requires_a_live_matching_process_and_unexpired_lease() {
    let pid = std::process::id();
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid).unwrap();
    let owner_uid = unsafe { libc::geteuid() as u32 };
    let future = now_ms().saturating_add(60_000);
    let base = ToolExposureContext::isolated(Guardrails::permissive())
        .with_identity("worker", owner_uid, SessionSource::BrokerTask)
        .with_presence(false, true);

    let live = base.clone().with_presence_lease(SessionPresence {
        owner_uid,
        pid,
        start_time_ticks,
        expires_at_ms: future,
    });
    assert!(live.is_attended_local());

    let exited = base.clone().with_presence_lease(SessionPresence {
        owner_uid,
        pid,
        start_time_ticks: start_time_ticks.saturating_add(1),
        expires_at_ms: future,
    });
    assert!(!exited.is_attended_local());

    let expired = base.with_presence_lease(SessionPresence {
        owner_uid,
        pid,
        start_time_ticks,
        expires_at_ms: now_ms().saturating_sub(1),
    });
    assert!(!expired.is_attended_local());
}
