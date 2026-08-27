use super::*;

#[test]
fn every_routed_command_carries_an_audit_policy() {
    // A route with no policy would be audited as `<unrecognized>` with
    // no arguments — safe, but silent. Fail loudly instead so a new
    // command has to classify its own fields.
    for command in USER_COMMANDS.iter().chain(ROOT_COMMANDS.iter()) {
        assert!(
            audit_policy::command_policy(command).is_some(),
            "clawd routes `{command}` with no audit policy"
        );
    }
}

#[test]
fn the_canonical_route_list_and_the_policy_table_agree() {
    for command in audit_policy::known_commands() {
        assert!(
            is_dispatchable(command),
            "audit policy names `{command}`, which clawd does not route"
        );
    }
}

#[test]
fn root_only_commands_are_not_reachable_by_a_user_peer() {
    let user = ClientIdentity {
        pid: Some(42),
        uid: Some(1000),
        gid: Some(1000),
    };
    for command in ROOT_COMMANDS {
        assert!(authorize_command(command, &user).is_err(), "{command}");
    }
    assert!(authorize_command("daemon.health", &user).is_ok());
}

#[test]
fn unrouted_commands_are_not_dispatchable() {
    assert!(!is_dispatchable("vendor.debug.dump"));
    assert!(is_dispatchable("context.update"));
    assert!(is_dispatchable("scheduler.run"));
}
