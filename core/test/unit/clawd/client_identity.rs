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

#[cfg(target_os = "linux")]
fn thread_credentials() -> (i32, i32, Vec<libc::gid_t>) {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(count >= 0);
    let mut groups = vec![0; count as usize];
    assert_eq!(
        unsafe { libc::getgroups(count, groups.as_mut_ptr()) },
        count
    );
    (
        unsafe { libc::setfsuid(!0) },
        unsafe { libc::setfsgid(!0) },
        groups,
    )
}

#[cfg(target_os = "linux")]
#[test]
fn filesystem_owner_groups_are_account_derived_and_unknown_owners_fail_closed() {
    let uid = unsafe { libc::geteuid() };
    let (gid, groups) = owner_groups(uid).unwrap();
    assert!(groups.contains(&gid));
    let id = ClientIdentity::from_verified_delegation(
        std::process::id(),
        uid,
        61184,
        61183,
        1,
        AuthenticatedExtensionHost {
            purpose: crate::extension_host::protocol::HostPurpose::Task,
            lease_id: "test".into(),
            authority_session_id: None,
            host_session_id: None,
            owner_uid: uid,
            extension_uid: 61184,
            capability_generation: "test".into(),
            host_pid: std::process::id(),
            host_start_time_ticks: None,
        },
    );
    let before = thread_credentials();
    {
        let _guard = FsIdentityGuard::enter(id.require_uid().unwrap()).unwrap();
        assert_eq!(thread_credentials(), (uid as i32, gid as i32, groups));
        assert_eq!(id.gid, Some(61183)); // execution GID is never filesystem authority
    }
    assert_eq!(thread_credentials(), before);
    assert!(FsIdentityGuard::enter(4_000_000_001).is_err());
    assert_eq!(thread_credentials(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn filesystem_distinct_primary_and_supplementary_groups_restore_on_success_and_error() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("requires root with CAP_SETUID/CAP_SETGID for synthetic credential fixture");
        return;
    }
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let primary = root.path().join("primary");
    let supplementary = root.path().join("supplementary");
    for (path, gid) in [(&primary, 62002), (&supplementary, 62003)] {
        std::fs::create_dir(path).unwrap();
        let file = std::fs::File::open(path).unwrap();
        assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 0, gid) }, 0);
        file.set_permissions(std::fs::Permissions::from_mode(0o770))
            .unwrap();
    }
    let source = supplementary.join("document");
    std::fs::write(&source, "group-owned content").unwrap();
    let file = std::fs::File::open(&source).unwrap();
    assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 0, 62003) }, 0);
    file.set_permissions(std::fs::Permissions::from_mode(0o660))
        .unwrap();
    let before = thread_credentials();
    let (ready, start) = std::sync::mpsc::channel();
    let (done, observed) = std::sync::mpsc::channel();
    let other = std::thread::spawn(move || {
        start.recv().unwrap();
        done.send(thread_credentials()).unwrap();
    });
    {
        let _guard = FsIdentityGuard::enter_groups(62001, 62002, &[62002, 62003]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "group-owned content"
        );
        let created = primary.join("created");
        std::fs::write(&created, "new").unwrap();
        assert_eq!(std::fs::metadata(created).unwrap().gid(), 62002);
        ready.send(()).unwrap();
        assert_eq!(
            observed.recv().unwrap(),
            before,
            "other broker thread changed credentials"
        );
    }
    other.join().unwrap();
    assert_eq!(thread_credentials(), before);
    let denied = || -> Result<(), String> {
        let _guard = FsIdentityGuard::enter_groups(62001, 62002, &[62002]).unwrap();
        std::fs::read(&source).map_err(|error| error.to_string())?;
        Ok(())
    };
    assert!(
        denied().is_err(),
        "supplementary-group access must be necessary"
    );
    assert_eq!(thread_credentials(), before);
}
