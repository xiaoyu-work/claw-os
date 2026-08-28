use super::*;

fn sample() -> LaunchPolicy {
    LaunchPolicy {
        tier: TrustTier::AppOperation,
        label: "app:fs/read".to_string(),
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), "pass".to_string()],
        workdir: PathBuf::from("/data"),
        mounts: vec![
            Mount::read_only("/usr/lib/app", "/usr/lib/app", MountClass::Package),
            Mount::read_write("/var/data/app", "/var/data/app", MountClass::AppData),
        ],
        network: NetworkPolicy::Denied,
        env: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
        limits: Limits::operation(),
        seccomp: SeccompProfile::Strict,
        stdio: StdioPlan::Captured,
        broker: true,
        umask: 0o077,
    }
}

#[test]
fn only_the_native_host_tier_escapes_the_sandbox() {
    for tier in [
        TrustTier::AppOperation,
        TrustTier::McpServer,
        TrustTier::AgentExec,
        TrustTier::DesktopSurface,
    ] {
        assert!(tier.is_sandboxed(), "{tier:?} must be sandboxed");
    }
    assert!(!TrustTier::TrustedNativeHost.is_sandboxed());
}

#[test]
fn display_is_reserved_for_desktop_and_native_tiers() {
    assert!(!TrustTier::AppOperation.allows_display());
    assert!(!TrustTier::McpServer.allows_display());
    assert!(!TrustTier::AgentExec.allows_display());
    assert!(TrustTier::DesktopSurface.allows_display());
    assert!(TrustTier::TrustedNativeHost.allows_display());
}

#[test]
fn headless_tier_cannot_carry_a_display_mount() {
    let mut policy = sample();
    policy.mounts.push(Mount::read_write(
        "/run/user/1000/wayland-0",
        "/run/user/1000/wayland-0",
        MountClass::Display,
    ));
    let error = policy.validate().unwrap_err();
    assert!(error.contains("display"), "{error}");

    policy.tier = TrustTier::DesktopSurface;
    policy.validate().expect("desktop tier may hold a display");
}

#[test]
fn sandboxed_tier_cannot_share_the_host_network() {
    let mut policy = sample();
    policy.network = NetworkPolicy::HostShared;
    let error = policy.validate().unwrap_err();
    assert!(error.contains("host network"), "{error}");
}

#[test]
fn duplicate_mount_targets_are_refused() {
    let mut policy = sample();
    policy.mounts.push(Mount::read_write(
        "/elsewhere",
        "/usr/lib/app",
        MountClass::Output,
    ));
    let error = policy.validate().unwrap_err();
    assert!(error.contains("duplicate"), "{error}");
}

#[test]
fn relative_paths_are_refused() {
    let mut policy = sample();
    policy.program = PathBuf::from("python3");
    assert!(policy.validate().unwrap_err().contains("absolute"));

    let mut policy = sample();
    policy
        .mounts
        .push(Mount::read_only("relative/src", "/dest", MountClass::Input));
    assert!(policy.validate().unwrap_err().contains("absolute"));
}

#[test]
fn environment_names_cannot_smuggle_a_second_variable() {
    let mut policy = sample();
    policy.env.insert("EVIL=OTHER".to_string(), "x".to_string());
    assert!(policy.validate().unwrap_err().contains("environment name"));
}

#[test]
fn digest_changes_with_every_isolation_relevant_field() {
    let base = sample();
    let baseline = base.digest();

    let mut writable = sample();
    writable.mounts[0].mode = MountMode::ReadWrite;
    assert_ne!(baseline, writable.digest());

    let mut networked = sample();
    networked.network = NetworkPolicy::Brokered {
        endpoints: vec![Endpoint::new("api.example.com", 443)],
    };
    assert_ne!(baseline, networked.digest());

    let mut loosened = sample();
    loosened.limits.pids_max = 4096;
    assert_ne!(baseline, loosened.digest());

    assert_eq!(baseline, sample().digest(), "digest must be stable");
}

#[test]
fn audit_facts_carry_no_paths_or_values() {
    let policy = sample();
    let facts = policy.audit_facts();
    let rendered = facts.to_string();
    assert!(!rendered.contains("/usr/lib/app"), "{rendered}");
    assert!(!rendered.contains("/var/data/app"), "{rendered}");
    assert!(!rendered.contains("/usr/bin"), "{rendered}");
    assert_eq!(facts["mounts"]["total"], 2);
    assert_eq!(facts["mounts"]["writable"], 1);
    assert_eq!(facts["network"]["mode"], "denied");
    // Names are safe and useful; values are not present.
    assert_eq!(facts["env_names"][0], "PATH");
}

#[test]
fn endpoints_must_be_exact() {
    for bad in [
        Endpoint::new("*.example.com", 443),
        Endpoint::new("example.com", 0),
        Endpoint::new("localhost", 443),
        Endpoint::new("", 443),
        Endpoint::new("-bad.example.com", 443),
    ] {
        assert!(validate_endpoint(&bad).is_err(), "{bad:?} must be rejected");
    }
    validate_endpoint(&Endpoint::new("API.Example.com", 443)).expect("exact host");
    validate_endpoint(&Endpoint::new("93.184.216.34", 443)).expect("literal address");
}

#[test]
fn server_limits_have_no_wall_clock_deadline() {
    assert!(Limits::server().deadline().is_none());
    assert_eq!(
        Limits::operation().deadline(),
        Some(std::time::Duration::from_secs(300))
    );
}
