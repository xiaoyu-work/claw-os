use super::*;

use crate::worker::policy::{Endpoint, Limits, Mount, MountClass, SeccompProfile, StdioPlan};
use std::collections::BTreeMap;

fn policy(network: NetworkPolicy) -> LaunchPolicy {
    LaunchPolicy {
        tier: crate::worker::policy::TrustTier::AppOperation,
        label: "app:test/run".to_string(),
        program: PathBuf::from("/usr/bin/python3"),
        argv: vec!["-c".to_string(), "pass".to_string()],
        workdir: PathBuf::from("/var/data/app"),
        mounts: vec![
            Mount::read_only("/opt/app", "/opt/app", MountClass::Package),
            Mount::read_write("/var/data/app", "/var/data/app", MountClass::AppData),
        ],
        network,
        env: BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
        limits: Limits::operation(),
        seccomp: SeccompProfile::Strict,
        stdio: StdioPlan::Captured,
        broker: true,
        umask: 0o077,
    }
}

fn args(policy: &LaunchPolicy) -> Vec<String> {
    build_bwrap_args(
        policy,
        &policy.mounts,
        1000,
        1000,
        &HostLayout {
            bin_is_symlink: true,
            sbin_is_symlink: true,
            lib_is_symlink: true,
            lib64_is_symlink: true,
            lib64_exists: true,
        },
    )
}

fn pair_index(args: &[String], flag: &str, value: &str) -> Option<usize> {
    args.windows(2)
        .position(|window| window[0] == flag && window[1] == value)
}

#[test]
fn private_account_records_bind_only_runtime_identity_and_private_home() {
    let records = account_records(4242, 4343, "fixture-user", "/private/app", "/bin/sh").unwrap();
    assert_eq!(
        records,
        [
            (
                "/etc/passwd",
                "fixture-user:x:4242:4343::/private/app:/bin/sh\n".into()
            ),
            ("/etc/group", "worker:x:4343:\n".into()),
        ]
    );
    for (name, home, shell) in [
        ("bad:root", "/private/app", "/bin/sh"),
        (
            "fixture-user",
            "/private/app\nroot:x:0:0::/:/bin/sh",
            "/bin/sh",
        ),
        ("fixture-user", "relative", "/bin/sh"),
        ("fixture-user", "/private/app", "relative"),
    ] {
        assert!(account_records(4242, 4343, name, home, shell).is_err());
    }
    assert!(account_records(0, 0, "root", "/private/app", "/bin/sh").is_err());
    let virtual_account = view_account_records(4242, 4343, "/private/app", None).unwrap();
    assert_eq!(
        virtual_account[0].1,
        "worker-4242:x:4242:4343::/private/app:/bin/sh\n"
    );
    let namespaced = view_account_records(
        4242,
        4343,
        "/private/app",
        Some(WorkerAccount {
            gid: 9999,
            name: "fixture-user".into(),
            shell: "/bin/sh".into(),
        })
    ).unwrap();
    assert_eq!(namespaced[0].1, "fixture-user:x:4242:4343::/private/app:/bin/sh\n");
}

#[test]
fn private_account_views_are_sealed_readonly_descriptor_mounts() {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let mut pinned = PinnedSources {
        mounts: vec![Mount::read_only(
            "/explicit-account",
            "/etc/passwd",
            MountClass::Input,
        )],
        files: Vec::new(),
        data: Vec::new(),
    };
    for (target, bytes) in
        account_records(4242, 4343, "fixture-user", "/private/app", "/bin/sh").unwrap()
    {
        pinned
            .add_system_data(target, bytes.as_bytes(), false)
            .unwrap();
    }
    let args = build_bwrap_args_with_seccomp(
        &policy(NetworkPolicy::Denied),
        &pinned.mounts,
        4242,
        4343,
        &HostLayout::default(),
        SECCOMP_FD,
        &pinned.data,
    );
    assert!(args
        .windows(3)
        .any(|args| args == ["--ro-bind-data", "200", "/etc/passwd"]));
    assert!(args
        .windows(3)
        .any(|args| args == ["--ro-bind-data", "201", "/etc/group"]));
    let base = args
        .windows(3)
        .position(|args| args == ["--ro-bind-data", "200", "/etc/passwd"])
        .unwrap();
    let explicit = args
        .windows(3)
        .position(|args| args == ["--ro-bind", "/explicit-account", "/etc/passwd"])
        .unwrap();
    assert!(
        base < explicit,
        "the default account view cannot mask an explicitly authorized file"
    );
    let file = &mut pinned.files[0];
    assert_eq!(
        file.stream_position().unwrap(),
        0,
        "bwrap consumes data from the current offset"
    );
    assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o444);
    assert!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } & libc::F_SEAL_WRITE != 0);
    assert!(file.write_all(b"root:x:0:0::/:/bin/sh\n").is_err());
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = String::new();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "fixture-user:x:4242:4343::/private/app:/bin/sh\n");
}

#[test]
fn every_namespace_is_named_explicitly() {
    let args = args(&policy(NetworkPolicy::Denied));
    for flag in [
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
    ] {
        assert!(args.contains(&flag.to_string()), "missing {flag}");
    }
    // `--unshare-all` implies `--unshare-user-try`, which would
    // silently continue without a user namespace.
    assert!(!args.contains(&"--unshare-all".to_string()));
    assert!(!args.contains(&"--share-net".to_string()));
}

#[test]
fn brokered_egress_still_runs_in_a_private_network_namespace() {
    let args = args(&policy(NetworkPolicy::Brokered {
        endpoints: vec![Endpoint::new("api.example.com", 443)],
    }));
    assert!(args.contains(&"--unshare-net".to_string()));
    assert!(!args.contains(&"--share-net".to_string()));
}

#[test]
fn nested_user_namespaces_are_disabled_and_asserted() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(args.contains(&"--disable-userns".to_string()));
    assert!(args.contains(&"--assert-userns-disabled".to_string()));
}

#[test]
fn all_capabilities_are_dropped_and_the_session_is_new() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(pair_index(&args, "--cap-drop", "ALL").is_some());
    assert!(args.contains(&"--new-session".to_string()));
    assert!(args.contains(&"--die-with-parent".to_string()));
}

#[test]
fn proc_and_dev_are_private_instances() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(pair_index(&args, "--proc", "/proc").is_some());
    assert!(pair_index(&args, "--dev", "/dev").is_some());
    // A bind of the host's /proc or a device-enabling bind would defeat
    // the pid namespace and the device policy.
    assert!(pair_index(&args, "--ro-bind", "/proc").is_none());
    assert!(!args.contains(&"--dev-bind".to_string()));
}

#[test]
fn tmp_and_run_are_private_and_size_bounded() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(pair_index(&args, "--tmpfs", "/tmp").is_some());
    assert!(pair_index(&args, "--tmpfs", "/run").is_some());
    assert!(pair_index(&args, "--tmpfs", "/var/tmp").is_some());
    assert!(args.contains(&"--size".to_string()));
}

#[test]
fn the_root_is_remounted_read_only_after_every_bind() {
    let args = args(&policy(NetworkPolicy::Denied));
    let remount = pair_index(&args, "--remount-ro", "/").expect("root remount");
    let package = pair_index(&args, "--ro-bind", "/opt/app").expect("package mount");
    let data = pair_index(&args, "--bind", "/var/data/app").expect("data mount");
    assert!(package < remount, "binds must precede the root remount");
    assert!(data < remount, "binds must precede the root remount");
}

#[test]
fn mount_direction_follows_the_policy() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(pair_index(&args, "--ro-bind", "/opt/app").is_some());
    assert!(pair_index(&args, "--bind", "/opt/app").is_none());
    assert!(pair_index(&args, "--bind", "/var/data/app").is_some());
}

#[test]
fn no_home_or_credential_root_is_bound() {
    let args = args(&policy(NetworkPolicy::Denied));
    assert!(pair_index(&args, "--ro-bind", "/home").is_none());
    assert!(pair_index(&args, "--bind", "/home").is_none());
    assert!(pair_index(&args, "--ro-bind-try", "/etc").is_none());
    // /etc is exposed only as a fixed list of harmless files.
    assert!(pair_index(&args, "--ro-bind-try", "/etc/ssl").is_some());
    assert!(pair_index(&args, "--ro-bind-try", "/etc/shadow").is_none());
}

#[test]
fn the_seccomp_descriptor_is_passed_to_bwrap() {
    let args = args(&policy(NetworkPolicy::Denied));
    let index = args
        .iter()
        .position(|value| value == "--seccomp")
        .expect("seccomp flag");
    assert_eq!(args[index + 1], SECCOMP_FD.to_string());
}

#[test]
fn the_command_is_last_and_separated() {
    let policy = policy(NetworkPolicy::Denied);
    let args = args(&policy);
    let separator = args
        .iter()
        .rposition(|value| value == "--")
        .expect("argv separator");
    assert_eq!(args[separator + 1], "/usr/bin/python3");
    assert_eq!(&args[separator + 2..], &policy.argv[..]);
}

#[test]
fn a_non_merged_usr_host_binds_instead_of_symlinking() {
    let policy = policy(NetworkPolicy::Denied);
    let args = build_bwrap_args(
        &policy,
        &policy.mounts,
        1000,
        1000,
        &HostLayout {
            bin_is_symlink: false,
            sbin_is_symlink: false,
            lib_is_symlink: false,
            lib64_is_symlink: false,
            lib64_exists: false,
        },
    );
    assert!(pair_index(&args, "--ro-bind-try", "/bin").is_some());
    assert!(pair_index(&args, "--symlink", "usr/bin").is_none());
    // A host without /lib64 gets neither a symlink nor a bind for it.
    assert!(pair_index(&args, "--ro-bind-try", "/lib64").is_none());
}

#[test]
fn the_worker_identity_is_pinned_in_the_user_namespace() {
    let policy = policy(NetworkPolicy::Denied);
    let args = build_bwrap_args(&policy, &policy.mounts, 4242, 4343, &HostLayout::default());
    assert!(pair_index(&args, "--uid", "4242").is_some());
    assert!(pair_index(&args, "--gid", "4343").is_some());
}

#[test]
fn availability_reports_each_missing_facility() {
    let availability = LinuxSandbox.availability();
    if availability.is_available() {
        assert!(availability.missing.is_empty());
        assert!(availability.governor.is_some());
    } else {
        assert!(!availability.refusal().is_empty());
    }
}
