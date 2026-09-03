use super::*;

fn manifest() -> Manifest {
    Manifest::from_json(
        r#"{
          "schema_version": 2,
          "id": "email",
          "version": "1.0.0",
          "name": {"en": "Email"},
          "mcp": {
            "access": {
              "system_agent": false,
              "apps": ["crm"],
              "external_agents": true
            },
            "tools": [{
              "name": "email.search",
              "summary": {"en": "Search mail."}
            }]
          }
        }"#,
    )
    .unwrap()
}

fn principal(kind: McpPrincipalKind, app_id: Option<&str>) -> McpPrincipal {
    McpPrincipal {
        kind,
        id: match kind {
            McpPrincipalKind::App | McpPrincipalKind::AppAgent => "app-session",
            McpPrincipalKind::ExternalAgent => "spiffe://claw.test/agent/client",
            McpPrincipalKind::Cli => "cli-session",
            McpPrincipalKind::SystemAgent => "agent-session",
        }
        .to_string(),
        owner_uid: 1000,
        app_id: app_id.map(str::to_string),
    }
}

fn binding() -> ExtensionBinding {
    ExtensionBinding {
        protocol: crate::extension_host::protocol::PROTOCOL_VERSION,
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        app_id: None,
        owner_uid: 1000,
        extension_uid: 61_000,
        owner_gid: 1000,
        capability_generation: "a".repeat(16),
        approved_paths: vec![crate::extension_host::protocol::ApprovedPath {
            path: "/usr/lib/cos".to_string(),
            device: 1,
            inode: 1,
            owner_uid: 0,
            mode: 0o755,
        }],
        worker_pid: std::process::id(),
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        host_pid: std::process::id().saturating_add(1),
        host_start_time_ticks: Some(1),
        lease_nonce: "a".repeat(32),
        expires_at_ms: crate::agentd::grant::now_ms() + 60_000,
        control_socket: "/tmp/control.sock".to_string(),
        broker_socket: "/tmp/broker.sock".to_string(),
    }
}

#[test]
fn invoke_scope_is_exact_to_app_and_tool() {
    let requested = invoke_cap("email", "email.search").unwrap();
    assert_eq!(requested.scope, Scope::name("email/email.search"));
    assert!(!Cap::new(Verb::AGENT_INVOKE, Scope::name("email")).covers(&requested));
    assert!(!Cap::new(Verb::AGENT_INVOKE, Scope::name("email/email.send")).covers(&requested));
    assert!(Cap::new(Verb::AGENT_INVOKE, Scope::name("email/*")).covers(&requested));
    assert!(invoke_cap("Bad/App", "email.search").is_err());
    assert!(invoke_cap("email", "../search").is_err());
}

#[test]
fn manifest_access_restricts_each_authenticated_principal_class() {
    let manifest = manifest();
    assert!(
        authorize_manifest(&manifest, &principal(McpPrincipalKind::SystemAgent, None)).is_err()
    );
    assert!(authorize_manifest(&manifest, &principal(McpPrincipalKind::Cli, None)).is_err());
    assert!(authorize_manifest(&manifest, &principal(McpPrincipalKind::App, Some("crm"))).is_ok());
    assert!(authorize_manifest(
        &manifest,
        &principal(McpPrincipalKind::AppAgent, Some("notes"))
    )
    .is_err());
    assert!(
        authorize_manifest(&manifest, &principal(McpPrincipalKind::ExternalAgent, None)).is_ok()
    );
}

#[test]
fn call_context_is_bound_to_extension_lease() {
    let binding = binding();
    let context = McpCallContext {
        wire_version: CALL_CONTEXT_WIRE_VERSION,
        call_id: "call-a".to_string(),
        trace_id: "call-a".to_string(),
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 1_000),
        session_id: Some("session-a".to_string()),
        task_id: Some("task-a".to_string()),
        caller: McpPrincipal {
            kind: McpPrincipalKind::SystemAgent,
            id: "session-a".to_string(),
            owner_uid: 1000,
            app_id: None,
        },
    };
    context.validate_extension_binding(&binding).unwrap();

    let invocation = crate::extension_host::protocol::AppInvocationAudit::new(
        "email",
        "email.search",
        binding.capability_generation.clone(),
        context.clone(),
    )
    .unwrap();
    invocation.validate_live_binding(&binding).unwrap();

    let mut forged = context;
    forged.caller.owner_uid = forged.caller.owner_uid.saturating_add(1);
    assert!(forged.validate_extension_binding(&binding).is_err());

    let mut substituted = invocation;
    substituted.invoke_target = "email/email.send".to_string();
    assert!(substituted.validate_live_binding(&binding).is_err());
}

#[test]
fn app_agent_context_requires_the_broker_bound_app_identity() {
    let mut binding = binding();
    binding.app_id = Some("crm".to_string());
    let context = McpCallContext {
        wire_version: CALL_CONTEXT_WIRE_VERSION,
        call_id: "call-app".to_string(),
        trace_id: "call-app".to_string(),
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 1_000),
        session_id: Some("session-a".to_string()),
        task_id: Some("task-a".to_string()),
        caller: McpPrincipal {
            kind: McpPrincipalKind::AppAgent,
            id: "session-a".to_string(),
            owner_uid: 1000,
            app_id: Some("crm".to_string()),
        },
    };
    context.validate_extension_binding(&binding).unwrap();

    let mut substituted = context.clone();
    substituted.caller.app_id = Some("notes".to_string());
    assert!(substituted.validate_extension_binding(&binding).is_err());

    let mut downgraded = context;
    downgraded.caller.kind = McpPrincipalKind::SystemAgent;
    downgraded.caller.app_id = None;
    assert!(downgraded.validate_extension_binding(&binding).is_err());
}

#[test]
fn call_context_rejects_unbounded_or_inconsistent_identity() {
    let mut context = McpCallContext {
        wire_version: CALL_CONTEXT_WIRE_VERSION,
        call_id: "call-a".to_string(),
        trace_id: "trace-a".to_string(),
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(crate::agentd::grant::now_ms() + 1_000),
        session_id: Some("session-a".to_string()),
        task_id: Some("task-a".to_string()),
        caller: principal(McpPrincipalKind::App, Some("crm")),
    };
    context.validate().unwrap();

    context.depth = MAX_CALL_DEPTH + 1;
    assert!(context.validate().is_err());
    context.depth = 0;
    context.call_id = "call-a\nforged".to_string();
    assert!(context.validate().is_err());
    context.call_id = "call-a".to_string();
    context.caller.kind = McpPrincipalKind::SystemAgent;
    assert!(context.validate().is_err());

    context.caller = principal(McpPrincipalKind::App, Some("crm"));
    context.deadline_unix_ms = Some(1);
    assert!(context.remaining(Duration::from_secs(1)).is_err());
}
