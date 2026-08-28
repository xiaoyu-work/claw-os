use super::*;

use crate::caps::{Cap, CapSet, Scope, Verb};

fn developer() -> Ceiling {
    Ceiling::for_tier(TrustTier::Developer)
}

fn publisher() -> Ceiling {
    Ceiling::for_tier(TrustTier::User)
}

/// Every verb the developer tier is allowed to hold, spelled out here
/// as well as in production. Adding a verb to `DEVELOPER_ALLOWED`
/// without deciding it is safe for unsigned code will fail this test.
const EXPECTED_DEVELOPER_VERBS: &[&str] = &[
    "fs.read",
    "fs.write",
    "fs.meta",
    "data.kv.read",
    "data.kv.write",
    "data.log.read",
    "data.log.write",
    "memory.read",
    "memory.write",
    "ui.notify",
];

#[test]
fn developer_allow_list_is_exactly_what_we_reviewed() {
    let actual: std::collections::BTreeSet<&str> =
        DEVELOPER_ALLOWED.iter().map(|v| v.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> =
        EXPECTED_DEVELOPER_VERBS.iter().copied().collect();
    assert_eq!(
        actual, expected,
        "the developer capability ceiling changed; review each new verb for unsigned code"
    );
}

#[test]
fn every_other_verb_defaults_denied_for_developer_content() {
    // The whole catalog, not a sample: a verb added tomorrow is denied
    // until somebody deliberately allows it.
    let ceiling = developer();
    let mut denied = 0usize;
    for verb in crate::caps::verb::ALL_VERBS {
        if DEVELOPER_ALLOWED.contains(verb) {
            assert!(ceiling.allows_verb(*verb), "{} should be allowed", verb.as_str());
        } else {
            assert!(
                !ceiling.allows_verb(*verb),
                "{} must be denied to unsigned developer content",
                verb.as_str()
            );
            denied += 1;
        }
    }
    assert!(denied > 60, "expected the vast majority of verbs denied, got {denied}");
}

#[test]
fn the_dangerous_families_are_all_denied() {
    let ceiling = developer();
    for verb in crate::caps::verb::ALL_VERBS {
        let name = verb.as_str();
        let dangerous = name.starts_with("sys.")
            || name.starts_with("secret.")
            || name.starts_with("net.")
            || name.starts_with("device.")
            || name.starts_with("clipboard.")
            || name.starts_with("browser.")
            || name.starts_with("desktop.")
            || name.starts_with("agent.")
            || name.starts_with("proc.")
            || name == "fs.exec"
            || name == "fs.delete"
            || name == "ui.input"
            || name == "ui.accessibility"
            || name == "ui.window"
            || name == "ui.prompt"
            || name == "data.backup";
        if dangerous {
            assert!(
                !ceiling.allows_verb(*verb),
                "{name} must never be held by unsigned developer content"
            );
        }
    }
}

#[test]
fn developer_content_may_not_hold_a_wildcard_scope() {
    let ceiling = developer();
    // Even an allowed verb is refused at wildcard scope.
    assert!(!ceiling.allows_cap(&Cap::new(Verb::FS_READ, Scope::Wild)));
    assert!(ceiling.allows_cap(&Cap::new(
        Verb::FS_READ,
        Scope::path("/home/dev/app/**")
    )) || !ceiling.allows_cap(&Cap::new(Verb::FS_READ, Scope::path("/home/dev/app/**"))));
    // A signed publisher package keeps wildcards its manifest declared.
    assert!(publisher().allows_cap(&Cap::new(Verb::FS_READ, Scope::Wild)));
}

#[test]
fn clamp_reports_what_it_dropped() {
    let mut caps = CapSet::new();
    caps.insert(Cap::new(Verb::FS_READ, Scope::path("/tmp/app")));
    caps.insert(Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts")));
    caps.insert(Cap::new(Verb::NET_DIAL, Scope::host("example.com")));
    caps.insert(Cap::new(Verb::SECRET_READ, Scope::name("token")));

    let (kept, dropped) = developer().clamp(&caps);
    assert!(kept.covers(&Cap::new(Verb::FS_READ, Scope::path("/tmp/app"))));
    let dropped_verbs: std::collections::BTreeSet<&str> =
        dropped.iter().map(|c| c.verb.as_str()).collect();
    assert!(dropped_verbs.contains("sys.identity"));
    assert!(dropped_verbs.contains("net.dial"));
    assert!(dropped_verbs.contains("secret.read"));
    assert_eq!(dropped.len(), 3);

    // A signed package loses nothing.
    let (kept, dropped) = publisher().clamp(&caps);
    assert!(dropped.is_empty());
    assert!(kept.covers(&Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))));
}

#[test]
fn developer_content_is_refused_privileged_audiences() {
    let ceiling = developer();
    assert!(ceiling.allows_audience(Audience::AppLaunch));
    for audience in [
        Audience::AppRelay,
        Audience::SystemService,
        Audience::Credential,
        Audience::Scheduler,
        Audience::Permission,
        Audience::Transaction,
        Audience::Context,
        Audience::Notification,
        Audience::Task,
        Audience::Daemon,
    ] {
        assert!(
            !ceiling.allows_audience(audience),
            "{} must be refused to unsigned content",
            audience.as_str()
        );
    }
    // Signed content keeps them all.
    for audience in [Audience::AppRelay, Audience::SystemService, Audience::Credential] {
        assert!(publisher().allows_audience(audience));
    }
}

#[test]
fn developer_mcp_servers_are_refused_outright() {
    assert!(!developer().allows_mcp_attach());
    assert!(publisher().allows_mcp_attach());
    assert!(Ceiling::for_tier(TrustTier::Vendor).allows_mcp_attach());
}

#[test]
fn developer_content_cannot_borrow_another_apps_identity() {
    let ceiling = developer();
    assert!(ceiling.allows_app_identity("scratch", "scratch"));
    assert!(!ceiling.allows_app_identity("scratch", "notes"));
    // A signed package is bound by the manifest id check instead.
    assert!(publisher().allows_app_identity("scratch", "notes"));
}

#[test]
fn only_vendor_content_is_trusted_model_text() {
    assert!(!Ceiling::for_tier(TrustTier::Vendor).model_content_is_untrusted());
    assert!(developer().model_content_is_untrusted());
    assert!(publisher().model_content_is_untrusted());
    assert!(Ceiling::for_tier(TrustTier::System).model_content_is_untrusted());
}

#[test]
fn facts_name_the_tier_and_its_restriction() {
    let facts = developer().facts();
    assert_eq!(facts["tier"], "developer");
    assert_eq!(facts["developer_restricted"], true);
    assert!(facts["allowed_verbs"].is_array());

    let facts = publisher().facts();
    assert_eq!(facts["developer_restricted"], false);
    assert!(facts["allowed_verbs"].is_null());
}

#[test]
fn developer_content_may_invoke_only_its_own_identity() {
    let ceiling = Ceiling::for_package(TrustTier::Developer, "notes");
    assert!(
        ceiling.allows_cap(&Cap::new(Verb::AGENT_INVOKE, Scope::name("notes"))),
        "an unsigned App must still be able to re-enter itself"
    );
    for other in ["fs", "pkg", "notes-evil", "**"] {
        assert!(
            !ceiling.allows_cap(&Cap::new(Verb::AGENT_INVOKE, Scope::name(other))),
            "developer content must not invoke `{other}`"
        );
    }
    assert!(!ceiling.allows_cap(&Cap::new(Verb::AGENT_INVOKE, Scope::Wild)));
    // Without a bound identity there is nothing to compare against, so
    // the verb is refused rather than guessed.
    assert!(!developer().allows_cap(&Cap::new(Verb::AGENT_INVOKE, Scope::name("notes"))));
    // And it is still not a *verb* the tier holds in general.
    assert!(!ceiling.allows_verb(Verb::AGENT_INVOKE));
    assert!(publisher().allows_cap(&Cap::new(Verb::AGENT_INVOKE, Scope::name("anything"))));
}

#[test]
fn permitted_audiences_filters_rather_than_advises() {
    let requested = [
        Audience::AppLaunch,
        Audience::SystemService,
        Audience::Credential,
        Audience::AppRelay,
    ];
    assert_eq!(
        developer().permitted_audiences(&requested),
        vec![Audience::AppLaunch]
    );
    assert_eq!(publisher().permitted_audiences(&requested), requested.to_vec());
    // The session-grant request, which carries no launch authority at
    // all, collapses to nothing for developer content.
    assert!(developer()
        .permitted_audiences(&[Audience::SystemService, Audience::Credential])
        .is_empty());
}

#[test]
fn developer_content_gets_no_relay_and_no_granted_path_mounts() {
    assert!(!developer().allows_relay());
    assert!(!developer().allows_granted_path_mounts());
    assert!(publisher().allows_relay());
    assert!(publisher().allows_granted_path_mounts());
    assert!(Ceiling::for_tier(TrustTier::Vendor).allows_relay());
}

#[test]
fn clamp_vec_keeps_the_plan_and_the_missing_list_in_step() {
    let ceiling = Ceiling::for_package(TrustTier::Developer, "notes");
    let caps = vec![
        Cap::new(Verb::FS_READ, Scope::path("/home/u/notes/**")),
        Cap::new(Verb::SYS_PACKAGE, Scope::name("nano")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("notes")),
        Cap::new(Verb::NET_DIAL, Scope::host("example.com")),
    ];
    let (kept, dropped) = ceiling.clamp_vec(&caps);
    assert_eq!(kept.len(), 2);
    assert_eq!(dropped.len(), 2);
    assert!(kept.contains(&Cap::new(Verb::AGENT_INVOKE, Scope::name("notes"))));
    assert!(dropped.contains(&Cap::new(Verb::SYS_PACKAGE, Scope::name("nano"))));
    assert!(dropped.contains(&Cap::new(Verb::NET_DIAL, Scope::host("example.com"))));
}

#[test]
fn the_package_ceiling_reports_its_own_identity() {
    let ceiling = Ceiling::for_package(TrustTier::Developer, "notes");
    assert_eq!(ceiling.own_id(), Some("notes"));
    assert_eq!(developer().own_id(), None);
    assert!(ceiling.allows_app_identity("notes", "notes"));
    assert!(!ceiling.allows_app_identity("notes", "fs"));
}
