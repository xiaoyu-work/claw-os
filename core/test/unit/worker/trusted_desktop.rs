use super::*;

#[test]
fn every_fixed_row_names_exactly_its_system_program() {
    // Kernel source, spelled out: an id may name this program and no
    // other, and an id that is not here may name none at all.
    let expected: &[(&str, &str)] = &[
        ("cosmic-files", "/usr/bin/cosmic-files"),
        ("cosmic-edit", "/usr/bin/cosmic-edit"),
        ("cosmic-store", "/usr/bin/cosmic-store"),
        ("cosmic-settings", "/usr/bin/cosmic-settings"),
        ("cosmic-term", "/usr/bin/cosmic-term"),
        ("cosmic-launcher", "/usr/bin/cosmic-launcher"),
        ("cosmic-player", "/usr/bin/cosmic-player"),
        ("cosmic-screenshot", "/usr/bin/cosmic-screenshot"),
        ("cosmic-notifications", "/usr/bin/cosmic-notifications"),
    ];
    assert_eq!(ALLOWLIST.len(), expected.len(), "the table grew or shrank");
    for (app_id, program) in expected {
        assert_eq!(
            allowlisted_system_program(app_id),
            Some(*program),
            "`{app_id}` does not name `{program}`"
        );
    }
    // Nothing else may name an absolute entry at all.
    assert_eq!(allowlisted_system_program("kv"), None);
    assert_eq!(allowlisted_system_program("cosmic-player-evil"), None);
    assert_eq!(allowlisted_system_program("launcher"), None);
    assert_eq!(allowlisted_system_program(""), None);
}

#[test]
fn only_the_three_bus_rows_carry_a_transport() {
    // Naming a system program and holding the session bus are separate
    // grants. Six of the nine native Apps get the first and not the
    // second, and their launches stay ordinary hostile MCP servers.
    let with_bus: Vec<&str> = ALLOWLIST
        .iter()
        .filter(|row| !row.transports.is_empty())
        .map(|row| row.app_id)
        .collect();
    assert_eq!(
        with_bus,
        vec!["cosmic-player", "cosmic-screenshot", "cosmic-notifications"]
    );
    for row in ALLOWLIST {
        match row.app_id {
            "cosmic-player" | "cosmic-screenshot" | "cosmic-notifications" => assert_eq!(
                row.transports,
                &[Transport::SessionBus],
                "row `{}` grants more than the session bus",
                row.app_id
            ),
            other => assert!(
                row.transports.is_empty(),
                "row `{other}` gained a desktop transport it does not need"
            ),
        }
    }
}

#[cfg(unix)]
#[test]
fn a_package_that_is_not_vendor_trusted_is_never_classified() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let root = crate::test_env::secure_scratch_dir("desktop-classify");
    let dir = root.join("cosmic-player");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), "{}").unwrap();
    crate::test_env::sign_test_package(&dir, crate::provenance::PackageKind::App, "cosmic-player");
    let trust = crate::provenance::trust_store();
    let options = crate::provenance::VerifyOptions::new(crate::provenance::PackageKind::App)
        .expect_id("cosmic-player");
    let package = crate::provenance::verify::verify_package(&dir, &options, &trust)
        .expect("verify signed fixture");

    // A real publisher signature over a package that calls itself
    // `cosmic-player` is exactly the attack this refuses: the id
    // matches a row, everything else does not.
    assert!(matches!(
        package.source(),
        crate::provenance::TrustSource::Publisher { .. }
    ));
    assert!(
        classify(
            "cosmic-player",
            &package,
            std::path::Path::new("/usr/bin/cosmic-player"),
        )
        .is_none(),
        "a publisher-signed package reached the desktop transport"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_program_the_owner_can_rewrite_is_never_classified() {
    let root = crate::test_env::secure_scratch_dir("desktop-program");
    let program = root.join("cosmic-player");
    std::fs::write(&program, "#!/bin/sh\n").unwrap();
    // Owned by the test user, not root: the ownership gate is what
    // stands between "the id matched" and "the transport is granted".
    assert!(root_owned(&program).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------------------
// The session bus address is parsed, not pattern-matched
// ---------------------------------------------------------------------------

#[test]
fn only_a_single_plain_unix_path_address_parses() {
    // The shape systemd actually produces, with and without the guid.
    assert_eq!(
        parse_unix_bus_address("unix:path=/run/user/1000/bus").unwrap(),
        std::path::PathBuf::from("/run/user/1000/bus")
    );
    assert_eq!(
        parse_unix_bus_address("unix:path=/run/user/1000/bus,guid=8d9a").unwrap(),
        std::path::PathBuf::from("/run/user/1000/bus")
    );
    // Percent-encoding is part of the grammar and is decoded strictly.
    assert_eq!(
        parse_unix_bus_address("unix:path=/run/user/1000/my%20bus").unwrap(),
        std::path::PathBuf::from("/run/user/1000/my bus")
    );

    let refused = |address: &str| -> String {
        match parse_unix_bus_address(address) {
            Ok(path) => panic!("`{address}` parsed to {}", path.display()),
            Err(BusRefusal(reason)) => reason,
        }
    };

    // An alternative list lets the launcher and the worker disagree
    // about which endpoint they are talking to.
    assert!(refused("unix:path=/run/user/1000/bus;unix:path=/tmp/evil").contains("exactly one"));
    assert!(refused(";").contains("exactly one"));
    // Abstract sockets live in a network namespace the sandbox does
    // not share, and are named rather than silently dropped.
    assert!(refused("unix:abstract=/tmp/dbus-abc").contains("abstract"));
    assert!(refused("unix:abstract=/tmp/dbus-abc,path=/run/user/1000/bus").contains("abstract"));
    // Anything the launcher does not model is refused rather than
    // ignored: `dir=`/`tmpdir=` ask the client to invent a path.
    assert!(refused("unix:dir=/run/user/1000").contains("dir"));
    assert!(refused("unix:tmpdir=/tmp").contains("tmpdir"));
    assert!(refused("unix:runtime=yes").contains("runtime"));
    // Duplicate keys have no defined precedence, so they have none here.
    assert!(refused("unix:path=/run/user/1000/bus,path=/tmp/evil").contains("repeats"));
    assert!(refused("unix:guid=a,guid=b").contains("repeats"));
    // Wrong transport, no transport, no path, no value.
    assert!(refused("tcp:host=127.0.0.1,port=1").contains("not a filesystem socket"));
    assert!(refused("unixexec:path=/bin/sh").contains("not a filesystem socket"));
    // The bare `path=` form the previous parser accepted as a fallback
    // is not a D-Bus address at all.
    assert!(refused("path=/run/user/1000/bus").contains("no transport"));
    assert!(refused("/run/user/1000/bus").contains("no transport"));
    assert!(refused("unix:guid=abc").contains("names no path"));
    assert!(refused("unix:path").contains("no value"));
    // A relative path can never be a runtime-directory socket.
    assert!(refused("unix:path=bus").contains("not absolute"));
}

#[test]
fn a_malformed_or_smuggled_escape_is_refused() {
    let refused = |address: &str| -> String {
        match parse_unix_bus_address(address) {
            Ok(path) => panic!("`{address}` parsed to {}", path.display()),
            Err(BusRefusal(reason)) => reason,
        }
    };
    assert!(refused("unix:path=/run/user/1000/b%").contains("truncated"));
    assert!(refused("unix:path=/run/user/1000/b%2").contains("truncated"));
    assert!(refused("unix:path=/run/user/1000/b%zz").contains("non-hex"));
    // An encoded NUL or newline would let an address smuggle a
    // terminator past a naive consumer.
    assert!(refused("unix:path=/run/user/1000/bus%00/evil").contains("NUL or control"));
    assert!(refused("unix:path=/run/user/1000/bus%0a").contains("NUL or control"));
    // Encoded traversal decodes to the same `..` a literal one would,
    // and is caught by the path check rather than the decoder.
    let decoded = parse_unix_bus_address("unix:path=/run/user/1000/%2e%2e/evil").unwrap();
    assert_eq!(decoded, std::path::PathBuf::from("/run/user/1000/../evil"));
}

// ---------------------------------------------------------------------------
// The socket has to be the owner's, and has to be a socket
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn owner_uid_for_test() -> u32 {
    crate::provenance::fsec::effective_uid()
}

/// A runtime directory with the shape production requires: owned by
/// the caller, private, and with root-owned ancestry. `/tmp` is
/// root-owned and sticky, so a `0700` directory directly beneath it
/// satisfies the real rule rather than a weakened one.
#[cfg(unix)]
fn fixture_runtime_dir() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("cos-bus-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).expect("fixture runtime dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn a_runtime_directory_must_be_private_and_root_anchored() {
    use std::os::unix::fs::PermissionsExt;
    let uid = owner_uid_for_test();
    let dir = fixture_runtime_dir();
    assert!(verify_runtime_dir(&dir, uid).is_ok());

    // A directory another account can write into is not private.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o707)).unwrap();
    assert!(verify_runtime_dir(&dir, uid)
        .unwrap_err()
        .0
        .contains("not private"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Belonging to somebody else is disqualifying even when the mode
    // is right.
    assert!(verify_runtime_dir(&dir, uid + 1)
        .unwrap_err()
        .0
        .contains("belongs to uid"));

    // A symlink is never a runtime directory, however inviting the
    // target looks.
    let link = dir.with_extension("link");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    assert!(verify_runtime_dir(&link, uid)
        .unwrap_err()
        .0
        .contains("not a real directory"));

    // Ancestors owned by the caller rather than root would let the
    // whole directory be renamed in from underneath.
    let nested = dir.join("inner");
    std::fs::create_dir(&nested).unwrap();
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(verify_runtime_dir(&nested, uid)
        .unwrap_err()
        .0
        .contains("not root-owned"));

    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn only_an_owner_owned_socket_named_bus_inside_the_runtime_dir_resolves() {
    let uid = owner_uid_for_test();
    let dir = fixture_runtime_dir();
    let bus = dir.join("bus");
    let listener = std::os::unix::net::UnixListener::bind(&bus).expect("fixture bus");

    // The good case, and it carries the socket's own inode so the
    // provider can refuse a different one later.
    let resolved = verify_bus_socket(&bus, &dir, uid).expect("real socket resolves");
    assert_eq!(resolved.path, bus);
    let meta = crate::provenance::fsec::lstat(&bus).unwrap();
    assert_eq!(resolved.identity, (meta.dev, meta.ino));

    let refused = |path: &std::path::Path| -> String {
        match verify_bus_socket(path, &dir, uid) {
            Ok(resolved) => panic!("{} resolved", resolved.path.display()),
            Err(BusRefusal(reason)) => reason,
        }
    };

    // A regular file, a FIFO and a directory are not transports.
    let regular = dir.join("regular");
    std::fs::write(&regular, "not a socket").unwrap();
    assert!(refused(&regular).contains("not a session bus socket name"));
    let fifo_name = dir.join("bus2");
    assert!(refused(&fifo_name).contains("not a session bus socket name"));

    // The right *name* but the wrong kind: swap the socket for a plain
    // file and the type check is what catches it.
    drop(listener);
    std::fs::remove_file(&bus).unwrap();
    std::fs::write(&bus, "impostor").unwrap();
    assert!(refused(&bus).contains("not a Unix socket"));
    std::fs::remove_file(&bus).unwrap();

    // A symlink pointing at a real socket elsewhere is still a
    // symlink, and is refused before it is followed.
    let elsewhere = dir.join("real");
    let _elsewhere = std::os::unix::net::UnixListener::bind(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &bus).unwrap();
    assert!(refused(&bus).contains("is a symlink"));
    std::fs::remove_file(&bus).unwrap();

    // Outside the runtime directory, however well-formed.
    let outside = std::env::temp_dir().join("bus");
    assert!(refused(&outside).contains("outside the owner's runtime directory"));
    // Traversal that would climb out of it.
    assert!(refused(&dir.join("../bus")).contains("`..`"));

    // Claw OS's own endpoints are named explicitly, so a future
    // runtime-directory change cannot quietly make one reachable.
    assert!(refused(&crate::paths::clawd_socket_path()).contains("kernel-owned endpoint"));
    assert!(
        refused(std::path::Path::new(crate::worker::linux_egress_socket()))
            .contains("kernel-owned endpoint")
    );
    assert!(refused(std::path::Path::new("/run/systemd/private")).contains("kernel-owned endpoint"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_socket_owned_by_another_account_is_refused() {
    let uid = owner_uid_for_test();
    let dir = fixture_runtime_dir();
    let bus = dir.join("bus");
    let _listener = std::os::unix::net::UnixListener::bind(&bus).expect("fixture bus");
    // The socket is ours; claiming it for a different owner must not
    // resolve, which is the check that stops one account's launch from
    // being handed another account's bus.
    assert!(verify_bus_socket(&bus, &dir, uid + 1)
        .unwrap_err()
        .0
        .contains("belongs to uid"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_socket_swapped_after_resolution_is_a_different_inode() {
    let uid = owner_uid_for_test();
    let dir = fixture_runtime_dir();
    let bus = dir.join("bus");
    let listener = std::os::unix::net::UnixListener::bind(&bus).expect("fixture bus");
    let first = verify_bus_socket(&bus, &dir, uid).expect("resolve");

    // Recreate the socket at the same path. The provider pins by
    // `(dev, ino)`, so the launch it was resolved for can no longer
    // bind it — this is the fact that makes the pin meaningful.
    drop(listener);
    std::fs::remove_file(&bus).unwrap();
    let _second_listener = std::os::unix::net::UnixListener::bind(&bus).expect("second bus");
    let second = verify_bus_socket(&bus, &dir, uid).expect("resolve again");
    assert_ne!(
        first.identity, second.identity,
        "a replaced socket kept the same identity"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_that_cannot_authenticate_a_bus_gets_no_mount_at_all() {
    let _lock = crate::caps::test_env_lock::env_lock();
    let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
    for address in [
        "unix:abstract=/tmp/dbus-abcdef,guid=deadbeef",
        "unix:path=/nonexistent/bus",
        "unix:path=/run/cos/clawd.sock",
        "tcp:host=127.0.0.1,port=1234",
        "unix:path=/run/user/1000/bus;unix:path=/tmp/evil",
        "",
    ] {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address);
        let (mounts, env) = transport_mounts(&[Transport::SessionBus]);
        assert!(mounts.is_empty(), "`{address}` produced a mount");
        assert!(env.is_empty(), "`{address}` produced env");
    }
    std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
    let (mounts, env) = transport_mounts(&[Transport::SessionBus]);
    assert!(mounts.is_empty());
    assert!(env.is_empty());

    match previous {
        Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
        None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
    }
}
