use super::*;

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::worker::policy::MountMode;

fn caps(items: Vec<Cap>) -> CapSet {
    CapSet::from_caps(items)
}

#[test]
fn a_segment_glob_binds_each_match_and_not_their_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let documents = dir.path().join("Documents");
    std::fs::create_dir_all(documents.join("public/nested")).unwrap();
    std::fs::create_dir_all(documents.join("private")).unwrap();
    std::fs::write(documents.join("notes.txt"), "x").unwrap();

    let mounts = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/*", documents.to_string_lossy())),
    )]))
    .expect("mounts");

    let sources: Vec<_> = mounts.iter().map(|mount| mount.source.clone()).collect();
    let canonical = documents.canonicalize().unwrap();
    // Every direct child is bound at its own depth …
    assert!(sources.contains(&canonical.join("public")));
    assert!(sources.contains(&canonical.join("private")));
    assert!(sources.contains(&canonical.join("notes.txt")));
    // … and the parent itself is not, so a grandchild the grant does
    // not name is not reachable through it.
    assert!(!sources.contains(&canonical), "the prefix was mounted");
    assert!(!sources.contains(&canonical.join("public/nested")));
}

#[test]
fn a_segment_glob_skips_symlinks_and_special_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("mixed");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("real.txt"), "x").unwrap();
    std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();

    let mounts = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/*", root.to_string_lossy())),
    )]))
    .expect("mounts");
    let canonical = root.canonicalize().unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source, canonical.join("real.txt"));
}

#[test]
fn a_segment_glob_over_etc_never_reaches_a_credential_file() {
    // `/etc/*` is the realistic shape of an over-broad grant. Whatever
    // it enumerates, the forbidden roots must not be among it.
    let Ok(mounts) = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path("/etc/*"),
    )])) else {
        // A host with more than the ceiling of entries under /etc
        // refuses the launch, which is also correct.
        return;
    };
    for mount in &mounts {
        for forbidden in ["/etc/shadow", "/etc/gshadow", "/etc/ssh", "/etc/sudoers"] {
            assert_ne!(
                mount.source,
                std::path::PathBuf::from(forbidden),
                "{forbidden} was mounted"
            );
        }
    }
}

#[test]
fn a_recursive_scope_over_a_home_with_a_credential_store_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".ssh")).unwrap();
    std::fs::create_dir(dir.path().join("Documents")).unwrap();

    let error = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", dir.path().to_string_lossy())),
    )]))
    .unwrap_err();
    assert!(error.contains("credential store"), "{error}");

    // The same tree without the store is fine, and mounts the prefix.
    let clean = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(clean.path().join("Documents")).unwrap();
    let mounts = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", clean.path().to_string_lossy())),
    )]))
    .expect("mounts");
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source, clean.path().canonicalize().unwrap());
}

#[test]
fn a_recursive_scope_that_would_cover_a_kernel_root_is_refused() {
    // `/**` and `/run/**` both reach kernel-owned trees.
    assert!(granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path("/run/**")
    )]))
    .is_err());
}

#[test]
fn a_write_scope_may_not_be_a_glob() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_WRITE,
        Scope::path(format!("{}/*", dir.path().to_string_lossy())),
    )]))
    .unwrap_err();
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn deep_and_partial_globs_grant_nothing_rather_than_a_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
    for pattern in ["a/*/b", "**/b", "a/*.txt"] {
        let mounts = granted_path_mounts(&caps(vec![Cap::new(
            Verb::FS_READ,
            Scope::path(format!("{}/{pattern}", dir.path().to_string_lossy())),
        )]))
        .expect("mounts");
        assert!(mounts.is_empty(), "{pattern} produced {mounts:?}");
    }
}

#[test]
fn the_mount_count_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("many");
    std::fs::create_dir(&root).unwrap();
    for index in 0..(MAX_GLOB_MATCHES + 8) {
        std::fs::write(root.join(format!("file-{index}")), "x").unwrap();
    }
    let error = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/*", root.to_string_lossy())),
    )]))
    .unwrap_err();
    assert!(error.contains("more than"), "{error}");
}

#[test]
fn read_and_write_grants_map_to_the_matching_direction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let readable = dir.path().join("in");
    let writable = dir.path().join("out");
    std::fs::create_dir(&readable).unwrap();
    std::fs::create_dir(&writable).unwrap();

    let mounts = granted_path_mounts(&caps(vec![
        Cap::new(Verb::FS_READ, Scope::path(readable.to_string_lossy())),
        Cap::new(Verb::FS_WRITE, Scope::path(writable.to_string_lossy())),
    ]))
    .expect("mounts");

    let readable = readable.canonicalize().unwrap();
    let writable = writable.canonicalize().unwrap();
    let read_mount = mounts
        .iter()
        .find(|mount| mount.source == readable)
        .expect("read mount");
    let write_mount = mounts
        .iter()
        .find(|mount| mount.source == writable)
        .expect("write mount");
    assert_eq!(read_mount.mode, MountMode::ReadOnly);
    assert_eq!(read_mount.class, MountClass::Input);
    assert_eq!(write_mount.mode, MountMode::ReadWrite);
    assert_eq!(write_mount.class, MountClass::Output);
    // Identity mapping: the granted scope and the sandbox path are the
    // same string, so an argument the App receives still resolves.
    assert_eq!(read_mount.source, read_mount.target);
}

#[test]
fn a_path_granted_both_ways_is_mounted_writable_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let both = dir.path().join("both");
    std::fs::create_dir(&both).unwrap();
    let scope = Scope::path(both.to_string_lossy());

    let mounts = granted_path_mounts(&caps(vec![
        Cap::new(Verb::FS_READ, scope.clone()),
        Cap::new(Verb::FS_WRITE, scope),
    ]))
    .expect("mounts");

    let both = both.canonicalize().unwrap();
    let matching: Vec<_> = mounts.iter().filter(|mount| mount.source == both).collect();
    assert_eq!(matching.len(), 1, "one mount per path");
    assert_eq!(matching[0].mode, MountMode::ReadWrite);
}

#[test]
fn a_recursive_grant_mounts_its_literal_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("docs");
    std::fs::create_dir_all(root.join("nested")).unwrap();

    let mounts = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", root.to_string_lossy())),
    )]))
    .expect("mounts");
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source, root.canonicalize().unwrap());
}

#[test]
fn wildcard_and_root_grants_mount_nothing() {
    for scope in [Scope::Wild, Scope::path("/**"), Scope::path("**")] {
        let mounts =
            granted_path_mounts(&caps(vec![Cap::new(Verb::FS_READ, scope.clone())])).expect("ok");
        assert!(mounts.is_empty(), "{scope:?} must not mount the host");
    }
}

#[test]
fn a_grant_reaching_a_kernel_owned_root_fails_the_launch() {
    for path in ["/proc", "/proc/self", "/sys/kernel", "/run/cos", "/root"] {
        let error = granted_path_mounts(&caps(vec![Cap::new(Verb::FS_READ, Scope::path(path))]))
            .unwrap_err();
        assert!(error.contains("kernel-owned"), "{path} produced `{error}`");
    }
}

#[test]
fn a_grant_reaching_a_credential_store_fails_the_launch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ssh = dir.path().join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    let error = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_READ,
        Scope::path(ssh.to_string_lossy()),
    )]))
    .unwrap_err();
    assert!(error.contains("credential store"), "{error}");
}

#[test]
fn a_socket_is_never_mountable_as_data() {
    #[cfg(unix)]
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("worker.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let error = granted_path_mounts(&caps(vec![Cap::new(
            Verb::FS_READ,
            Scope::path(socket.to_string_lossy()),
        )]))
        .unwrap_err();
        assert!(error.contains("not a directory or regular file"), "{error}");
    }
}

#[test]
fn a_write_grant_for_a_missing_file_mounts_its_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("report.json");
    let mounts = granted_path_mounts(&caps(vec![Cap::new(
        Verb::FS_WRITE,
        Scope::path(target.to_string_lossy()),
    )]))
    .expect("mounts");
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].source, dir.path().canonicalize().unwrap());
    assert_eq!(mounts[0].mode, MountMode::ReadWrite);
}

#[test]
fn non_filesystem_verbs_never_mount_anything() {
    let mounts = granted_path_mounts(&caps(vec![
        Cap::new(Verb::SYS_OBSERVE, Scope::path("/etc")),
        Cap::new(Verb::AGENT_INVOKE, Scope::name("fs")),
    ]))
    .expect("mounts");
    assert!(mounts.is_empty());
}

#[test]
fn only_exact_host_grants_become_endpoints() {
    let network = egress_from_caps(&caps(vec![
        Cap::new(Verb::NET_DIAL, Scope::host("api.example.com:443")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.evil.example")),
        Cap::new(Verb::NET_DIAL, Scope::Wild),
        Cap::new(Verb::NET_DIAL, Scope::host("plain.example.com")),
    ]));
    let NetworkPolicy::Brokered { endpoints } = &network else {
        panic!("expected brokered egress, got {network:?}");
    };
    assert_eq!(endpoints.len(), 2);
    assert!(endpoints.contains(&Endpoint::new("api.example.com", 443)));
    // A host without a port defaults to TLS rather than to "any port".
    assert!(endpoints.contains(&Endpoint::new("plain.example.com", 443)));
}

#[test]
fn no_network_grant_means_denied() {
    let network = egress_from_caps(&caps(vec![Cap::new(Verb::FS_READ, Scope::path("/tmp"))]));
    assert!(matches!(network, NetworkPolicy::Denied));
}

#[test]
fn mcp_servers_get_no_egress_and_no_app_data() {
    let policy = mcp_server(McpServerInput {
        pinned_entries: Vec::new(),
        name: "github",
        program: PathBuf::from("/usr/bin/true"),
        argv: vec!["--stdio".to_string()],
        cwd: None,
        extra_env: BTreeMap::new(),
        session_id: None,
    })
    .expect("policy");
    assert_eq!(policy.tier, TrustTier::McpServer);
    assert!(matches!(policy.network, NetworkPolicy::Denied));
    assert_eq!(policy.seccomp, SeccompProfile::Strict);
    assert!(policy
        .mounts
        .iter()
        .all(|mount| mount.class != MountClass::AppData));
    assert!(!policy.broker, "no session means no authority endpoint");
}

#[test]
fn the_worker_environment_is_a_closed_allowlist() {
    let env = base_env(std::path::Path::new("/var/data/app"));
    for leaked in [
        "COS_PROC_DATA_DIR",
        "COS_CAPS_DATA_DIR",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XAUTHORITY",
        "SSH_AUTH_SOCK",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AWS_SECRET_ACCESS_KEY",
        "GITHUB_TOKEN",
    ] {
        assert!(
            !env.contains_key(leaked),
            "{leaked} leaked into the sandbox"
        );
    }
    assert_eq!(env["PATH"], SANDBOX_PATH);
    assert_eq!(env["HOME"], "/var/data/app");
    assert_eq!(env["COS_PERMS_MODE"], "strict");
    assert_eq!(env[crate::worker::SANDBOX_MARKER_ENV], "1");
}

#[test]
fn brokered_egress_advertises_the_endpoint_to_the_worker() {
    let mut env = base_env(std::path::Path::new("/tmp"));
    apply_egress_env(
        &mut env,
        &NetworkPolicy::Brokered {
            endpoints: vec![
                Endpoint::new("a.example.com", 443),
                Endpoint::new("b.example.com", 8443),
            ],
        },
    );
    assert_eq!(
        env["COS_EGRESS_ENDPOINTS"],
        "a.example.com:443,b.example.com:8443"
    );
    assert_eq!(
        env["COS_EGRESS_SOCKET"],
        crate::worker::linux_egress_socket()
    );

    let mut denied = base_env(std::path::Path::new("/tmp"));
    apply_egress_env(&mut denied, &NetworkPolicy::Denied);
    assert!(!denied.contains_key("COS_EGRESS_SOCKET"));
}
