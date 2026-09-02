use super::*;

#[cfg(unix)]
mod fixtures {
    use std::path::{Path, PathBuf};

    /// A private owner data root that goes away with the test.
    pub struct Root {
        path: PathBuf,
    }

    impl Root {
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cos-migrate-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("owner root");
            Self { path }
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let target = self.path.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(&target, contents).expect("write");
            target
        }

        pub fn partition(&self, app_id: &str) -> PathBuf {
            let partition = self.path.join("apps").join(app_id);
            std::fs::create_dir_all(&partition).expect("partition");
            partition
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
fn no_table_entry_can_select_kernel_state() {
    // The guard runs over the *effective source path* of both mapping
    // shapes, so an entry that names a shared root outright and one
    // that reaches a protected file inside a shared directory are
    // caught by the same rule.
    for (app_id, entries) in LEGACY_APP_STATE {
        assert!(!app_id.is_empty());
        for entry in *entries {
            check_entry(app_id, entry).unwrap_or_else(|error| panic!("{error}"));
            for (relative, _) in effective_sources(entry) {
                assert!(!relative.starts_with('/'), "{app_id}: {relative}");
                assert!(!relative.contains(".."), "{app_id}: {relative}");
            }
        }
    }
}

#[test]
fn the_guard_refuses_the_kernel_registry_however_it_is_named() {
    // Directly, as a whole file...
    assert!(check_entry("exec", &Legacy::File("proc/registry.json")).is_err());
    // ...as the shared directory around it...
    assert!(check_entry("exec", &Legacy::Dir("proc")).is_err());
    // ...as an explicitly named file inside it...
    assert!(check_entry(
        "exec",
        &Legacy::FilesIn {
            dir: "proc",
            names: &["registry.json"],
            prefixes: &[],
        }
    )
    .is_err());
    // ...through its lock sentinel or rename staging file...
    for entry in [
        Legacy::FilesIn {
            dir: "proc",
            names: &["registry.json.lock"],
            prefixes: &[],
        },
        Legacy::FilesIn {
            dir: "proc",
            names: &["registry.tmp"],
            prefixes: &[],
        },
    ] {
        assert!(
            check_entry("exec", &entry).is_err(),
            "{entry:?} was accepted"
        );
    }
    // ...and by a prefix wide enough to reach it.
    assert!(check_entry(
        "exec",
        &Legacy::FilesIn {
            dir: "proc",
            names: &[],
            prefixes: &["registry"],
        }
    )
    .is_err());
    assert!(check_entry(
        "exec",
        &Legacy::FilesIn {
            dir: "proc",
            names: &[],
            prefixes: &[""],
        }
    )
    .is_err());
    // Every other kernel root is refused as a whole directory too.
    for root in SHARED_KERNEL_ROOTS {
        assert!(
            check_entry("anything", &Legacy::Dir(root)).is_err(),
            "{root} was accepted"
        );
    }
    // And the migration's own marker cannot be moved over.
    assert!(check_entry(
        "gateway-discord",
        &Legacy::FilesIn {
            dir: "apps/gateway-discord",
            names: &[MARKER],
            prefixes: &[],
        }
    )
    .is_err());
    // The shipped exec prefixes stay legal, which is the point.
    assert!(check_entry(
        "exec",
        &Legacy::FilesIn {
            dir: "proc",
            names: &[],
            prefixes: &["stdout.", "stderr."],
        }
    )
    .is_ok());
}

#[cfg(unix)]
#[test]
fn a_directory_and_a_file_move_into_the_partition() {
    let root = fixtures::Root::new("dir-and-file");
    root.write("calendar/events.db", "rows");
    let partition = root.partition("calendar");

    migrate_legacy_state(root.path(), &partition, "calendar").expect("migrate");

    assert_eq!(
        std::fs::read_to_string(partition.join("calendar/events.db")).unwrap(),
        "rows"
    );
    assert!(!root.path().join("calendar").exists());
    assert_eq!(marker_version(&partition), CURRENT_VERSION);

    // Second launch is a no-op, not a second decision.
    migrate_legacy_state(root.path(), &partition, "calendar").expect("re-run");
    assert_eq!(
        std::fs::read_to_string(partition.join("calendar/events.db")).unwrap(),
        "rows"
    );
}

#[cfg(unix)]
#[test]
fn a_missing_source_is_simply_nothing_to_do() {
    let root = fixtures::Root::new("missing");
    let partition = root.partition("kv");
    migrate_legacy_state(root.path(), &partition, "kv").expect("migrate");
    assert_eq!(marker_version(&partition), CURRENT_VERSION);
    assert!(!partition.join("kv.json").exists());
}

#[cfg(unix)]
#[test]
fn an_app_without_legacy_state_is_left_alone() {
    let root = fixtures::Root::new("unlisted");
    let partition = root.partition("search");
    migrate_legacy_state(root.path(), &partition, "search").expect("migrate");
    // No marker either: there was never anything to bring forward.
    assert_eq!(marker_version(&partition), 0);
}

#[cfg(unix)]
#[test]
fn only_the_apps_own_files_leave_a_shared_directory() {
    let root = fixtures::Root::new("shared");
    root.write("proc/stdout.1234", "captured");
    root.write("proc/stderr.1234", "diagnostics");
    root.write("proc/sess-abcdef.json", "kernel session");
    let partition = root.partition("exec");

    migrate_legacy_state(root.path(), &partition, "exec").expect("migrate");

    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stdout.1234")).unwrap(),
        "captured"
    );
    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stderr.1234")).unwrap(),
        "diagnostics"
    );
    // The session registry's neighbours never moved.
    assert_eq!(
        std::fs::read_to_string(root.path().join("proc/sess-abcdef.json")).unwrap(),
        "kernel session"
    );
    assert!(!root.path().join("proc/stdout.1234").exists());
}

#[cfg(unix)]
#[test]
fn the_kernel_session_registry_stays_exactly_where_it_is() {
    // `<owner-root>/proc/registry.json` is `crate::proc::registry_path`
    // in an ordinary non-routed context: the session and capability
    // registry, with its lock sentinel and rename staging file beside
    // it. The `exec` App wrote captures into the same directory, and
    // only those may move.
    let root = fixtures::Root::new("registry");
    let registry = root.write(
        "proc/registry.json",
        r#"{"sessions":{"app-7":{"caps":["fs.read"]}}}"#,
    );
    root.write("proc/registry.json.lock", "");
    root.write("proc/registry.tmp", "half-written");
    root.write("proc/stdout.99", "exec output");
    root.write("proc/stderr.99", "exec errors");
    let before = std::fs::read(&registry).unwrap();
    let partition = root.partition("exec");

    migrate_legacy_state(root.path(), &partition, "exec").expect("migrate");

    // Only the App's captures moved.
    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stdout.99")).unwrap(),
        "exec output"
    );
    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stderr.99")).unwrap(),
        "exec errors"
    );
    // The registry is byte-identical at its own path, and neither it
    // nor its sentinels are anywhere inside the partition — which is
    // the whole sandbox view, so a worker cannot reach them either.
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    assert!(root.path().join("proc/registry.json.lock").is_file());
    assert!(root.path().join("proc/registry.tmp").is_file());
    for leaked in ["registry.json", "registry.json.lock", "registry.tmp"] {
        assert!(
            !partition.join("proc").join(leaked).exists(),
            "{leaked} entered the App partition"
        );
    }

    // A non-App reader still finds the registry through the kernel's
    // own path resolution.
    std::env::set_var("COS_PROC_DATA_DIR", root.path());
    let resolved = crate::proc::registry_path_for_caps();
    std::env::remove_var("COS_PROC_DATA_DIR");
    assert_eq!(resolved, registry);
    assert!(std::fs::read_to_string(&resolved)
        .unwrap()
        .contains("app-7"));

    // And the second launch, marker in place, does not revisit it.
    assert_eq!(marker_version(&partition), CURRENT_VERSION);
    migrate_legacy_state(root.path(), &partition, "exec").expect("re-run");
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    assert!(!partition.join("proc/registry.json").exists());

    // Even after a marker reset — the retry path — it is still refused.
    std::fs::remove_file(partition.join(super::MARKER)).unwrap();
    migrate_legacy_state(root.path(), &partition, "exec").expect("retry");
    assert_eq!(std::fs::read(&registry).unwrap(), before);
    assert!(!partition.join("proc/registry.json").exists());
}

#[cfg(unix)]
#[test]
fn a_gateway_collects_its_state_from_the_partition_root() {
    let root = fixtures::Root::new("gateway");
    // The pre-isolation path *is* the partition, so the files sit at
    // its root and have to move down one level.
    root.write("apps/gateway-discord/state.json", "cursor");
    root.write("apps/gateway-discord/config.json", "settings");
    let partition = root.partition("gateway-discord");

    migrate_legacy_state(root.path(), &partition, "gateway-discord").expect("migrate");

    assert_eq!(
        std::fs::read_to_string(partition.join("apps/gateway-discord/state.json")).unwrap(),
        "cursor"
    );
    assert_eq!(
        std::fs::read_to_string(partition.join("apps/gateway-discord/config.json")).unwrap(),
        "settings"
    );
    assert!(!partition.join("state.json").exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_source_is_refused_and_left_in_place() {
    let root = fixtures::Root::new("symlink");
    let elsewhere = root.write("elsewhere/events.db", "not ours");
    std::os::unix::fs::symlink(elsewhere.parent().unwrap(), root.path().join("calendar"))
        .expect("symlink");
    let partition = root.partition("calendar");

    let error = migrate_legacy_state(root.path(), &partition, "calendar").unwrap_err();
    assert!(error.contains("symlink"), "{error}");
    assert!(root.path().join("calendar").exists());
    assert!(!partition.join("calendar").exists());
    // A refused migration leaves no marker, so it is retried rather
    // than silently forgotten.
    assert_eq!(marker_version(&partition), 0);
}

#[cfg(unix)]
#[test]
fn a_hardlinked_file_is_refused() {
    let root = fixtures::Root::new("hardlink");
    let original = root.write("original.json", "shared");
    std::fs::hard_link(&original, root.path().join("kv.json")).expect("hard link");
    let partition = root.partition("kv");

    let error = migrate_legacy_state(root.path(), &partition, "kv").unwrap_err();
    assert!(error.contains("hard link"), "{error}");
    assert!(root.path().join("kv.json").exists());
}

#[cfg(unix)]
#[test]
fn a_special_file_is_refused() {
    let root = fixtures::Root::new("fifo");
    let path = root.path().join("kv.json");
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes().to_vec()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let partition = root.partition("kv");

    let error = migrate_legacy_state(root.path(), &partition, "kv").unwrap_err();
    assert!(error.contains("regular file"), "{error}");
    assert!(path.exists());
}

#[cfg(unix)]
#[test]
fn a_populated_destination_collision_fails_without_merging() {
    let root = fixtures::Root::new("collision");
    root.write("calendar/events.db", "legacy rows");
    let partition = root.partition("calendar");
    std::fs::create_dir_all(partition.join("calendar")).unwrap();
    std::fs::write(partition.join("calendar/events.db"), "new rows").unwrap();

    let error = migrate_legacy_state(root.path(), &partition, "calendar").unwrap_err();
    assert!(error.contains("keep one and remove the other"), "{error}");
    // Neither side was touched.
    assert_eq!(
        std::fs::read_to_string(root.path().join("calendar/events.db")).unwrap(),
        "legacy rows"
    );
    assert_eq!(
        std::fs::read_to_string(partition.join("calendar/events.db")).unwrap(),
        "new rows"
    );
    assert_eq!(marker_version(&partition), 0);
}

#[cfg(unix)]
#[test]
fn an_empty_destination_accepts_the_migration() {
    let root = fixtures::Root::new("empty-dest");
    root.write("db/store.sqlite", "rows");
    let partition = root.partition("db");
    std::fs::create_dir_all(partition.join("db")).unwrap();

    migrate_legacy_state(root.path(), &partition, "db").expect("migrate");
    assert_eq!(
        std::fs::read_to_string(partition.join("db/store.sqlite")).unwrap(),
        "rows"
    );
}

#[cfg(unix)]
#[test]
fn an_interrupted_migration_is_finished_by_the_next_launch() {
    let root = fixtures::Root::new("interrupted");
    root.write("proc/stdout.7", "captured");
    root.write("proc/stderr.7", "diagnostics");
    let partition = root.partition("exec");

    // Stand in for a crash after the first rename: one file has
    // already moved, the marker was never written.
    std::fs::create_dir_all(partition.join("proc")).unwrap();
    std::fs::rename(
        root.path().join("proc/stdout.7"),
        partition.join("proc/stdout.7"),
    )
    .unwrap();
    assert_eq!(marker_version(&partition), 0);

    migrate_legacy_state(root.path(), &partition, "exec").expect("retry");
    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stdout.7")).unwrap(),
        "captured"
    );
    assert_eq!(
        std::fs::read_to_string(partition.join("proc/stderr.7")).unwrap(),
        "diagnostics"
    );
    assert_eq!(marker_version(&partition), CURRENT_VERSION);
}

#[cfg(unix)]
#[test]
fn permissions_survive_the_move() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixtures::Root::new("modes");
    let source = root.write("kv.json", "{}");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    let partition = root.partition("kv");

    migrate_legacy_state(root.path(), &partition, "kv").expect("migrate");
    let moved = std::fs::metadata(partition.join("kv.json")).unwrap();
    assert_eq!(moved.permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn a_traversal_outside_the_owner_root_is_rejected() {
    // The table is fixed, so this guards the splitter that consumes
    // it rather than any caller-reachable input.
    assert!(unix::test_split("../escape").is_err());
    assert!(unix::test_split("/absolute").is_err());
    assert!(unix::test_split("a//b").is_err());
    assert_eq!(unix::test_split("calendar").unwrap(), ("", "calendar"));
    assert_eq!(
        unix::test_split("apps/gateway-discord/state.json").unwrap(),
        ("apps/gateway-discord", "state.json")
    );
}
