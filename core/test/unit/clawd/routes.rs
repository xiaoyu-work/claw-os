use super::*;
use serde_json::json;

/// Expected non-root route surface. Changes here are intentional API and
/// authority changes rather than incidental registry drift.
const EXPECTED_USER_COMMANDS: &[&str] = &[
    "daemon.health",
    "daemon.status",
    "task.submit",
    "task.list",
    "task.get",
    "task.status",
    "task.cancel",
    "task.retry",
    "task.stream",
    "task.result",
    "task.count",
    "memory.history",
    "memory.sessions",
    "agent.usage",
    "credential.oauth-refresh",
    "system.audio.control",
    "system.accessibility.control",
    "system.backup.control",
    "system.bluetooth.control",
    "system.camera.control",
    "system.clipboard.control",
    "system.container.control",
    "system.config.control",
    "system.crash.inspect",
    "system.desktop.control",
    "system.display.control",
    "system.events.control",
    "system.firewall.control",
    "system.hardware.inspect",
    "system.location.query",
    "system.network.control",
    "system.package.install",
    "system.package.control",
    "system.package.restore",
    "system.power.control",
    "system.printer.control",
    "system.security.inspect",
    "system.service.control",
    "system.service.restore",
    "system.snapshot.control",
    "system.storage.control",
    "system.usb.control",
    "system.users.control",
    "scheduler.run",
    "app_session.register",
    "app_session.register_native",
    "mcp_session.register",
    "app_session.bind",
    "app_session.set_transient",
    "app_session.deregister",
    // The launcher's relay: addressed by a grant `clawd` issued to that
    // exact process, and refused for every route a session may not
    // reach. It is `Access::User` because the launcher is unprivileged.
    "app_session.relay",
    "permission.pending",
    "permission.recent",
    "permission.status",
    "permission.request",
    "permission.decide",
    "context.snapshot",
    "context.sources",
    "context.event.append",
    "context.event.query",
    "system.operations",
    "notification.publish",
    "notification.list",
    "notification.subscribe",
    "notification.read",
    "notification.acknowledge",
    "notification.dismiss",
    "notification.preferences.get",
    "notification.preferences.set",
    "notification.delivery.claim",
    "notification.delivery.complete",
    "transaction.begin",
    "transaction.list",
    "transaction.commit",
    "transaction.rollback",
    // Read-only view of the caller's own session-journal partitions:
    // health, head sequence and the mutations whose outcome is still
    // unknown. It resolves no grant and reads nobody else's chain.
    "journal.status",
];

/// Root-only routes. `permission.revoke` joins `context.update`: it
/// retires standing authority and its `owner_uid` names whose, so a
/// non-root peer must not reach it in either direction.
/// `journal.mutation.resolve` joins them because recording what
/// happened to an unresolved privileged mutation is an administrative
/// statement, and it is what ends that operation's replay refusal.
const EXPECTED_ROOT_COMMANDS: &[&str] = &[
    "context.update",
    "journal.mutation.resolve",
    "permission.revoke",
];

const EXPECTED_PRIVATE_TASK_HOST_COMMANDS: &[&str] = &["app_service.call"];

#[test]
fn the_table_and_the_command_enum_cannot_drift() {
    assert_eq!(ROUTES.len(), Command::ALL.len());
    for (index, command) in Command::ALL.iter().enumerate() {
        assert_eq!(ROUTES[index].command, *command);
        assert_eq!(command.route().name, ROUTES[index].name);
        assert_eq!(Command::parse(command.as_str()), Some(*command));
    }
}

#[test]
fn route_names_are_unique_and_loggable() {
    let mut seen = std::collections::BTreeSet::new();
    for route in ROUTES {
        assert!(seen.insert(route.name), "duplicate route {}", route.name);
        assert!(
            crate::audit_policy::is_token(route.name),
            "route name must be safe to store verbatim: {}",
            route.name
        );
    }
}

#[test]
fn the_access_allowlist_is_exactly_what_it_was() {
    let user: std::collections::BTreeSet<_> = user_commands().collect();
    let root: std::collections::BTreeSet<_> = root_commands().collect();
    let private_host: std::collections::BTreeSet<_> = private_task_host_commands().collect();
    assert_eq!(
        user,
        EXPECTED_USER_COMMANDS.iter().copied().collect(),
        "the set of routes a non-root peer may reach changed"
    );
    assert_eq!(
        root,
        EXPECTED_ROOT_COMMANDS.iter().copied().collect(),
        "the set of root-only routes changed"
    );
    assert_eq!(
        private_host,
        EXPECTED_PRIVATE_TASK_HOST_COMMANDS
            .iter()
            .copied()
            .collect(),
        "the set of private task Host routes changed"
    );
}

#[test]
fn every_route_owns_its_audit_metadata() {
    let payload_free: std::collections::BTreeSet<_> = [
        "daemon.health",
        "daemon.status",
        "task.count",
        "context.snapshot",
        "context.sources",
        "notification.preferences.get",
        "transaction.begin",
        "transaction.list",
    ]
    .into_iter()
    .collect();
    let mut observed_payload_free = std::collections::BTreeSet::new();
    for route in ROUTES {
        let facts = crate::audit_policy::request_facts_for_route(
            route.name,
            route.audit_fields,
            &json!({}),
        );
        assert_eq!(facts.command, route.name);
        assert!(facts.command_text.is_none());
        let mut fields = std::collections::BTreeSet::new();
        for (field, _) in route.audit_fields {
            assert!(
                fields.insert(*field),
                "{} declares audit field {field} twice",
                route.name
            );
        }
        if route.audit_fields.is_empty() {
            observed_payload_free.insert(route.name);
        }
    }
    assert_eq!(observed_payload_free, payload_free);
}

#[test]
fn every_route_uses_the_typed_stable_error_policy() {
    for route in ROUTES {
        let execution = route.errors.response(
            crate::clawd::protocol::RequestId::unknown(),
            BrokerError::from("provider failed".to_string()),
        );
        assert_eq!(
            execution.error.expect("error body").code,
            "execution_failed",
            "{}",
            route.name
        );
        assert_eq!(route.errors, ErrorPolicy::Typed);
    }
}

#[test]
fn unrouted_commands_are_not_dispatchable() {
    assert!(Command::parse("vendor.debug.dump").is_none());
    assert!(Command::parse("").is_none());
    assert!(Command::parse("DAEMON.HEALTH").is_none());
    assert!(Command::parse("context.update").is_some());
    assert!(Command::parse("scheduler.run").is_some());
}

#[test]
fn root_only_commands_are_not_reachable_by_a_user_peer() {
    let user = ClientIdentity {
        pid: Some(42),
        uid: Some(1000),
        gid: Some(1000),
        execution_uid: None,
        start_time_ticks: Some(1),
        attended_local: false,
        extension_host: None,
    };
    let root = ClientIdentity {
        pid: Some(42),
        uid: Some(0),
        gid: Some(0),
        execution_uid: None,
        start_time_ticks: Some(1),
        attended_local: false,
        extension_host: None,
    };
    for command in EXPECTED_ROOT_COMMANDS {
        let route = Command::parse(command).unwrap().route();
        assert_eq!(
            route.authorize(&user),
            Err(Fault::NotAuthorized),
            "{command}"
        );
        assert_eq!(route.authorize(&root), Ok(()), "{command}");
    }
    let health = Command::DaemonHealth.route();
    assert_eq!(health.authorize(&user), Ok(()));

    let private = Command::AppServiceCall.route();
    assert_eq!(private.authorize(&user), Err(Fault::NotAuthorized));
    assert_eq!(private.authorize(&root), Err(Fault::NotAuthorized));
    let mut task_host = user;
    task_host.execution_uid = Some(61_000);
    task_host.extension_host = Some(crate::clawd::client_identity::AuthenticatedExtensionHost {
        purpose: crate::extension_host::protocol::HostPurpose::Task,
        lease_id: "task-a".to_string(),
        authority_session_id: Some("session-a".to_string()),
        host_session_id: Some("host-a".to_string()),
        owner_uid: 1000,
        extension_uid: 61_000,
        capability_generation: "a".repeat(16),
        host_pid: 42,
        host_start_time_ticks: Some(1),
    });
    assert_eq!(private.authorize(&task_host), Ok(()));
    task_host.extension_host.as_mut().unwrap().purpose =
        crate::extension_host::protocol::HostPurpose::AppService;
    assert_eq!(private.authorize(&task_host), Err(Fault::NotAuthorized));
}

#[test]
fn a_peer_without_credentials_reaches_no_route_at_all() {
    let unknown = ClientIdentity::unknown();
    for route in ROUTES {
        assert_eq!(
            route.authorize(&unknown),
            Err(Fault::MissingCredentials),
            "{}",
            route.name
        );
    }
}

#[test]
fn every_route_has_a_finite_concurrency_budget() {
    for route in ROUTES {
        assert!(
            route.budget.max_in_flight > 0,
            "{} may never run",
            route.name
        );
        assert!(
            route.budget.max_in_flight <= 64,
            "{} has an unbounded-looking budget",
            route.name
        );
        if let Deadline::Interruptible(limit) = route.budget.deadline {
            assert!(limit.as_secs() > 0, "{}", route.name);
        }
    }
}

#[test]
fn mutating_routes_are_never_cancelled_mid_flight() {
    // Dropping a privileged mutation at an await point can leave a
    // package half-installed. Those routes bound themselves instead.
    for route in ROUTES {
        if route.kind == Kind::Mutation {
            assert_eq!(
                route.budget.deadline,
                Deadline::Uninterruptible,
                "{} is a mutation and must not be cancelled by the broker",
                route.name
            );
        }
    }
}

#[test]
fn read_only_routes_are_classified_as_queries() {
    for name in [
        "daemon.health",
        "daemon.status",
        "task.list",
        "task.get",
        "task.result",
        "memory.history",
        "agent.usage",
        "context.snapshot",
        "context.event.query",
        "permission.status",
        "system.operations",
        "system.hardware.inspect",
        "transaction.list",
    ] {
        assert_eq!(
            Command::parse(name).unwrap().route().kind,
            Kind::Query,
            "{name}"
        );
    }
}

#[test]
fn state_changing_routes_are_classified_as_mutations() {
    for name in [
        "task.submit",
        "task.cancel",
        "task.retry",
        "context.update",
        "context.event.append",
        "permission.request",
        "permission.decide",
        "transaction.begin",
        "transaction.commit",
        "transaction.rollback",
        "scheduler.run",
        "app_session.register",
        "app_session.bind",
        "system.package.install",
        "system.service.restore",
        "credential.oauth-refresh",
    ] {
        assert_eq!(
            Command::parse(name).unwrap().route().kind,
            Kind::Mutation,
            "{name}"
        );
    }
}

#[test]
fn every_route_decodes_through_its_own_typed_body() {
    // `null` params stand in for "no arguments". Routes with required
    // fields must refuse it; routes without any must accept it and
    // produce an empty object rather than a null.
    for route in ROUTES {
        match (route.decode)(Value::Null) {
            Ok(params) => assert_eq!(params, json!({}), "{}", route.name),
            Err(fault) => assert_eq!(fault, Fault::InvalidParams, "{}", route.name),
        }
    }
}

#[test]
fn no_route_accepts_an_undeclared_field() {
    for route in ROUTES {
        let smuggled = json!({"__clawd_unexpected__": true});
        assert_eq!(
            (route.decode)(smuggled),
            Err(Fault::InvalidParams),
            "{} accepted an undeclared field",
            route.name
        );
    }
}

#[test]
fn no_route_accepts_a_non_object_body() {
    for route in ROUTES {
        for body in [json!("text"), json!(7), json!([1, 2]), json!(true)] {
            assert_eq!(
                (route.decode)(body),
                Err(Fault::InvalidParams),
                "{} accepted a non-object body",
                route.name
            );
        }
    }
}

#[test]
fn a_decoded_body_carries_only_declared_fields() {
    let route = Command::SystemPackageControl.route();
    let params = (route.decode)(json!({
        "session": "sess-1",
        "action": "remove",
        "package": "nano",
    }))
    .unwrap();
    assert_eq!(
        params,
        json!({"session": "sess-1", "action": "remove", "package": "nano"})
    );
}
