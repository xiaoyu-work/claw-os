use super::*;

use crate::worker::policy::{
    LaunchPolicy, Limits, Mount, MountClass, NetworkPolicy, SeccompProfile, StdioPlan, TrustTier,
};

fn policy() -> LaunchPolicy {
    LaunchPolicy {
        tier: TrustTier::AppOperation,
        label: "app:mail/send".to_string(),
        program: std::path::PathBuf::from("/usr/bin/python3"),
        argv: vec![
            "-c".to_string(),
            "print(open('/secret').read())".to_string(),
        ],
        workdir: std::path::PathBuf::from("/var/data/mail"),
        mounts: vec![Mount::read_only(
            "/home/user/private/notes.txt",
            "/home/user/private/notes.txt",
            MountClass::Input,
        )],
        network: NetworkPolicy::Brokered {
            endpoints: vec![crate::worker::policy::Endpoint::new(
                "smtp.example.com",
                465,
            )],
        },
        env: std::collections::BTreeMap::from([(
            "COS_SESSION".to_string(),
            "app-9f3c".to_string(),
        )]),
        limits: Limits::operation(),
        seccomp: SeccompProfile::StrictNetwork,
        stdio: StdioPlan::Captured,
        broker: true,
        umask: 0o077,
    }
}

#[test]
fn launch_facts_name_no_path_argument_or_value() {
    let facts = policy().audit_facts().to_string();
    for sensitive in [
        "/home/user/private/notes.txt",
        "smtp.example.com",
        "app-9f3c",
        "print(open",
    ] {
        assert!(
            !facts.contains(sensitive),
            "{sensitive} leaked into {facts}"
        );
    }
}

#[test]
fn launch_facts_keep_the_reconstructable_shape() {
    let policy = policy();
    let facts = policy.audit_facts();
    assert!(facts["policy"]
        .as_str()
        .unwrap_or_default()
        .starts_with("sha256:"));
    assert_eq!(facts["tier"], "app-operation");
    assert_eq!(facts["network"]["mode"], "brokered");
    assert_eq!(facts["network"]["endpoints"], 1);
    assert_eq!(facts["mounts"]["classes"]["input"], 1);
    assert_eq!(facts["seccomp"], "strict-network");
    assert_eq!(facts["limits"]["pids_max"], policy.limits.pids_max);
}

#[test]
fn every_event_name_is_stable() {
    assert_eq!(LAUNCH, "worker.sandbox.launch");
    assert_eq!(REFUSED, "worker.sandbox.refused");
    assert_eq!(EXEMPT, "worker.sandbox.exempt");
    assert_eq!(OUTCOME, "worker.sandbox.outcome");
}
