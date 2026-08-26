use super::*;
use crate::caps::Verb;

#[test]
fn system_agent_caps_follow_catalog_risk() {
    let caps = system_agent_caps(Some(Path::new("/home/test")));
    assert!(caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/hosts"))));
    assert!(caps.covers(&Cap::new(Verb::SYS_OBSERVE, Scope::name("packages"))));
    assert!(caps.covers(&Cap::new(Verb::AI_CHAT, Scope::name("claude"))));
    assert!(caps.covers(&Cap::new(
        Verb::FS_WRITE,
        Scope::path("/home/test/work/x")
    )));
    assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/x"))));
    assert!(caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host("github.com"))));
    assert!(caps.covers(&Cap::new(Verb::NET_RESOLVE, Scope::host("github.com"))));
    assert!(!caps.covers(&Cap::new(Verb::FS_DELETE, Scope::path("/tmp/x"))));
    assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("git"))));
    assert!(!caps.covers(&Cap::new(Verb::SYS_SERVICE, Scope::name("sshd"))));
    assert!(!caps.covers(&Cap::new(Verb::SYS_CRASH, Scope::name("system"))));
    assert!(!caps.covers(&Cap::new(Verb::SECRET_READ, Scope::name("OPENAI_API_KEY"))));
}

#[test]
fn system_agent_caps_include_exactly_low_and_medium_risk_verbs() {
    let caps = system_agent_caps(Some(Path::new("/home/test")));
    for meta in crate::caps::catalog::CATALOG {
        assert_eq!(
            caps.verbs().contains(&meta.verb),
            meta.risk <= Risk::Medium,
            "unexpected default policy for {} ({:?})",
            meta.verb.as_str(),
            meta.risk
        );
    }
}
