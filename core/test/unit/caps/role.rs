use super::*;
use crate::caps::cap::Cap;
use crate::caps::scope::Scope;

#[test]
fn names_round_trip() {
    for r in ALL_ROLES {
        assert_eq!(Role::parse(r.name()), Some(*r));
    }
}

#[test]
fn observer_cannot_write() {
    assert!(!Role::Observer.verbs().contains(&Verb::FS_WRITE));
    assert!(!Role::Observer.verbs().contains(&Verb::FS_DELETE));
    assert!(!Role::Observer.verbs().contains(&Verb::NET_DIAL));
    assert!(Role::Observer.verbs().contains(&Verb::SYS_OBSERVE));
}

#[test]
fn worker_can_write_but_not_delete() {
    assert!(Role::Worker.verbs().contains(&Verb::FS_WRITE));
    assert!(!Role::Worker.verbs().contains(&Verb::FS_DELETE));
    assert!(!Role::Worker.verbs().contains(&Verb::NET_DIAL));
}

#[test]
fn curator_can_delete() {
    assert!(Role::Curator.verbs().contains(&Verb::FS_DELETE));
}

#[test]
fn connector_is_read_plus_net_not_write() {
    assert!(Role::Connector.verbs().contains(&Verb::NET_DIAL));
    assert!(!Role::Connector.verbs().contains(&Verb::FS_WRITE));
    assert!(!Role::Connector.verbs().contains(&Verb::FS_DELETE));
}

#[test]
fn automator_can_exec_and_dial_and_delete() {
    assert!(Role::Automator.verbs().contains(&Verb::FS_EXEC));
    assert!(Role::Automator.verbs().contains(&Verb::NET_DIAL));
    assert!(Role::Automator.verbs().contains(&Verb::FS_DELETE));
    // But not full secret write/grant.
    assert!(!Role::Automator.verbs().contains(&Verb::SECRET_WRITE));
    assert!(!Role::Automator.verbs().contains(&Verb::SECRET_GRANT));
}

#[test]
fn agent_host_can_delegate() {
    assert!(Role::AgentHost.verbs().contains(&Verb::AGENT_DELEGATE));
    assert!(Role::AgentHost.verbs().contains(&Verb::AGENT_SPAWN));
}

#[test]
fn admin_can_install_packages() {
    assert!(Role::Admin.verbs().contains(&Verb::SYS_PACKAGE));
    assert!(Role::Admin.verbs().contains(&Verb::SECRET_GRANT));
    assert!(Role::Admin.verbs().contains(&Verb::CLIPBOARD_READ));
    assert!(Role::Admin.verbs().contains(&Verb::CLIPBOARD_WRITE));
}

#[test]
fn clipboard_access_is_not_granted_below_admin() {
    for role in [
        Role::Observer,
        Role::Worker,
        Role::Curator,
        Role::Connector,
        Role::Automator,
        Role::AgentHost,
    ] {
        assert!(!role.verbs().contains(&Verb::CLIPBOARD_READ));
        assert!(!role.verbs().contains(&Verb::CLIPBOARD_WRITE));
    }
}

#[test]
fn kernel_role_has_every_verb() {
    for v in super::super::verb::ALL_VERBS {
        assert!(
            Role::Kernel.verbs().contains(v),
            "kernel missing {}",
            v.as_str()
        );
    }
}

#[test]
fn caps_with_scopes_uses_supplied_paths_and_hosts() {
    let caps = Role::Connector.caps_with_scopes(
        Some(Scope::path("/home/jay/docs/**")),
        Some(Scope::host("*.github.com:443")),
        Some(Scope::name("github/*")),
    );
    // Should cover fs.read inside the path.
    assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/jay/docs/x.md"))));
    // Should cover net.dial for github.
    assert!(caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host("api.github.com:443"))));
    // Must NOT cover fs.write (connector is read-only).
    assert!(!caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path("/home/jay/docs/x.md")
    )));
    // Must NOT cover dial outside the host scope.
    assert!(!caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host("evil.com:443"))));
}

#[test]
fn missing_path_scope_drops_path_caps() {
    let caps = Role::Worker.caps_with_scopes(None, None, None);
    // worker without a path scope keeps only unscoped caps.
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/anything"))));
    // ui.notify is unscoped and should be present.
    assert!(caps.covers(&Cap::unscoped(Verb::UI_NOTIFY)));
}

#[test]
fn net_manage_uses_category_name_scope() {
    let caps = Role::Admin.caps_with_scopes(
        None,
        Some(Scope::host("example.com:443")),
        Some(Scope::name("wifi")),
    );
    assert!(caps.covers(&Cap::new(Verb::NET_MANAGE, Scope::name("wifi"))));
    assert!(!caps.covers(&Cap::new(Verb::NET_MANAGE, Scope::name("vpn"))));

    let host_only =
        Role::Admin.caps_with_scopes(None, Some(Scope::host("example.com:443")), None);
    assert!(!host_only.covers(&Cap::new(Verb::NET_MANAGE, Scope::name("wifi"))));
}

#[test]
fn user_selectable_excludes_kernel() {
    let names: Vec<_> = user_selectable().map(|r| r.name()).collect();
    assert!(!names.contains(&"kernel"));
    assert!(names.contains(&"worker"));
}

#[test]
fn child_role_must_be_subset_of_parent() {
    // Spawning a child with elevated role from a worker parent fails.
    let parent = Role::Worker.caps_with_scopes(Some(Scope::path("/home/jay/**")), None, None);
    let child_request = Role::Automator.caps_with_scopes(
        Some(Scope::path("/home/jay/**")),
        Some(Scope::host("*")),
        Some(Scope::Wild),
    );
    // Automator requests verbs Worker never had.
    assert!(!parent.covers_all(&child_request));
    // The intersect filter yields only the worker-allowed subset.
    let allowed = parent.intersect(&child_request);
    assert!(allowed.covers(&Cap::new(Verb::FS_READ, Scope::path("/home/jay/x"))));
    assert!(!allowed.covers(&Cap::new(Verb::FS_EXEC, Scope::path("/home/jay/x"))));
}
