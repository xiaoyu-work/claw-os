use super::*;

#[test]
fn unknown_identity_has_no_uid_or_home() {
    let id = ClientIdentity::unknown();
    assert!(id.uid.is_none());
    assert!(id.home_dir().is_none());
}

#[test]
fn delegated_identity_keeps_principal_and_kernel_uids_distinct() {
    let identity = ClientIdentity::from_verified_delegation(
        42,
        1000,
        61_184,
        61_183,
        7,
        AuthenticatedExtensionHost {
            purpose: crate::extension_host::protocol::HostPurpose::Task,
            lease_id: "task-a".to_string(),
            authority_session_id: Some("session-a".to_string()),
            host_session_id: Some("host-a".to_string()),
            owner_uid: 1000,
            extension_uid: 61_184,
            capability_generation: "a".repeat(16),
            host_pid: 42,
            host_start_time_ticks: Some(7),
        },
    );
    assert_eq!(identity.uid, Some(1000));
    assert_eq!(identity.execution_uid, Some(61_184));
    assert_eq!(identity.process_uid(), Some(61_184));
}

#[cfg(unix)]
#[test]
fn resolve_home_for_current_uid_matches_passwd() {
    // The current process's uid must resolve to a real passwd
    // entry on any working unix system. Compare against the
    // `HOME` env var as a sanity check (they should normally
    // agree; if HOME has been overridden we just skip).
    let uid = unsafe { libc::getuid() } as u32;
    let resolved = resolve_home(uid);
    assert!(resolved.is_some(), "getpwuid_r returned None for self uid");
    if let (Some(env_home), Some(pwd_home)) = (std::env::var_os("HOME"), resolved.as_ref()) {
        if env_home != pwd_home.as_os_str() {
            // Possible in containers where HOME is set to /root
            // but passwd points elsewhere — log and move on.
            eprintln!(
                "note: HOME ({:?}) differs from passwd entry ({:?})",
                env_home, pwd_home
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn resolve_home_for_bogus_uid_returns_none() {
    // uid 4_000_000_001 is well above any realistic system uid.
    assert!(resolve_home(4_000_000_001).is_none());
}
