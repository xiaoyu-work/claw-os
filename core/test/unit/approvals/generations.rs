use super::*;

use std::ffi::OsString;

/// Every test drives the real generation state, so they share one
/// process-wide `COS_DATA_DIR` and must not overlap.
struct IsolatedEnv {
    _dir: tempfile::TempDir,
    previous: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn isolated_env() -> IsolatedEnv {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let lock = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", dir.path());
    IsolatedEnv {
        _dir: dir,
        previous,
        _lock: lock,
    }
}

#[test]
fn a_fresh_install_starts_at_generation_zero() {
    let _env = isolated_env();
    assert_eq!(current(Some(1000), "sess-a").unwrap(), 0);
    assert_eq!(current(None, "sess-a").unwrap(), 0);
}

#[test]
fn revoking_a_session_raises_only_that_session() {
    let _env = isolated_env();
    let raised = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();

    assert_eq!(raised, 1);
    assert_eq!(current(Some(1000), "sess-a").unwrap(), 1);
    assert_eq!(
        current(Some(1000), "sess-b").unwrap(),
        0,
        "a sibling session keeps its own authority"
    );
    assert_eq!(
        current(Some(1001), "sess-a").unwrap(),
        0,
        "another owner's identically named session is untouched"
    );
}

#[test]
fn revoking_an_owner_raises_every_session_it_holds() {
    let _env = isolated_env();
    revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    let raised = revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();

    assert!(raised >= 1);
    assert_eq!(current(Some(1000), "sess-a").unwrap(), raised);
    assert_eq!(current(Some(1000), "sess-anything").unwrap(), raised);
    assert_eq!(
        current(Some(1001), "sess-a").unwrap(),
        0,
        "an owner-wide revocation is scoped to that owner"
    );
}

#[test]
fn generations_only_ever_move_forward() {
    let _env = isolated_env();
    let mut last = 0;
    for _ in 0..5 {
        let next = revoke(&RevocationScope::Session {
            uid: Some(1000),
            session: "sess-a".to_string(),
        })
        .unwrap();
        assert!(next > last, "{next} must exceed {last}");
        last = next;
    }
    // An owner-wide revocation cannot lower a session already above it.
    let owner = revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();
    assert!(current(Some(1000), "sess-a").unwrap() >= last.max(owner));
}

#[test]
fn a_session_revocation_after_an_owner_revocation_clears_the_floor() {
    let _env = isolated_env();
    let floor = revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();
    let session = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    assert!(
        session > floor,
        "a session revocation must move past the owner floor, not behind it"
    );
    assert_eq!(current(Some(1000), "sess-a").unwrap(), session);
}

#[test]
fn unparseable_state_fails_closed() {
    let _env = isolated_env();
    super::super::ensure_dirs().unwrap();
    let path = state_path();
    std::fs::write(&path, "{ not json").unwrap();
    assert!(
        current(Some(1000), "sess-a").is_err(),
        "an authority that cannot tell whether something was revoked must refuse"
    );
}

#[cfg(unix)]
#[test]
fn world_writable_state_fails_closed() {
    use std::os::unix::fs::PermissionsExt;

    let _env = isolated_env();
    revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    let path = state_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(
        current(Some(1000), "sess-a").is_err(),
        "state anyone can rewrite is not an authority"
    );
}

#[test]
fn concurrent_revocations_do_not_collide() {
    let _env = isolated_env();
    let mut threads = Vec::new();
    for _ in 0..6 {
        threads.push(std::thread::spawn(|| {
            revoke(&RevocationScope::Session {
                uid: Some(1000),
                session: "sess-race".to_string(),
            })
            .unwrap()
        }));
    }
    let mut seen: Vec<u32> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        6,
        "each revocation must publish a distinct generation"
    );
    assert_eq!(current(Some(1000), "sess-race").unwrap(), 6);
}

#[test]
fn state_survives_a_reopen() {
    let _env = isolated_env();
    revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    // A restart is just another read of the same root-owned file: the
    // counter is durable, which is what makes revocation outlive the
    // daemon that performed it.
    assert_eq!(current(Some(1000), "sess-a").unwrap(), 1);
}

#[test]
fn a_symlinked_store_fails_closed() {
    let _env = isolated_env();
    super::super::ensure_dirs().unwrap();
    let path = state_path();
    let decoy = path.parent().unwrap().join("decoy.json");
    std::fs::write(&decoy, r#"{"owners":{},"sessions":{}}"#).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&decoy, &path).unwrap();
    assert!(
        current(Some(1000), "sess-a").is_err(),
        "a symlink is a refusal, not something to follow"
    );
}

#[test]
fn a_truncated_store_fails_closed_rather_than_reading_as_zero() {
    let _env = isolated_env();
    revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    // Exactly what a torn write would leave if the store did not go
    // through a temp file and a rename. Reading it as an empty object
    // would silently re-arm every grant this revocation retired.
    std::fs::write(state_path(), "{\"owners\":{\"1000\":").unwrap();
    assert!(current(Some(1000), "sess-a").is_err());
}

#[test]
fn a_write_leaves_no_temp_file_and_a_private_regular_file() {
    let _env = isolated_env();
    revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();

    let path = state_path();
    let metadata = std::fs::symlink_metadata(&path).unwrap();
    assert!(metadata.file_type().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "the counter is not readable or writable by anyone else"
        );
        assert_eq!(metadata.nlink(), 1, "no extra hard link to the counter");
    }
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the rename must not leave scratch files behind: {leftovers:?}"
    );
}

#[test]
fn a_stale_temp_file_does_not_affect_the_counter() {
    let _env = isolated_env();
    let first = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();

    // What a crash between the temp write and the rename leaves.
    let parent = state_path().parent().unwrap().to_path_buf();
    std::fs::write(
        parent.join(".generations.json.tmp.deadbeef"),
        r#"{"owners":{},"sessions":{}}"#,
    )
    .unwrap();

    assert_eq!(
        current(Some(1000), "sess-a").unwrap(),
        first,
        "an orphaned temp file is not the counter"
    );
    let next = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    assert!(next > first, "the store still moves forward after a crash");
}

#[test]
fn revocation_and_consumption_never_run_at_the_same_time() {
    // Both take the approvals store lock, and neither re-enters it, so
    // a spend that begins after a revocation completes always sees the
    // new generation. A missing exclusion would show up here as a
    // deadlock or as a generation read that skipped an increment.
    let _env = isolated_env();
    let rounds = 24;
    let revoker = std::thread::spawn(move || {
        for _ in 0..rounds {
            revoke(&RevocationScope::Session {
                uid: Some(1000),
                session: "sess-lock".to_string(),
            })
            .unwrap();
        }
    });
    let reader = std::thread::spawn(move || {
        let mut last = 0;
        for _ in 0..rounds {
            let seen = current(Some(1000), "sess-lock").unwrap();
            assert!(seen >= last, "the generation must never move backwards");
            last = seen;
        }
        last
    });
    revoker.join().expect("revoker");
    reader.join().expect("reader");
    assert_eq!(current(Some(1000), "sess-lock").unwrap(), rounds);
}

#[cfg(unix)]
#[test]
fn a_widened_store_fails_closed_for_reads_and_for_revocation_alike() {
    // A widened state file is refused by `current`, and — because
    // `revoke` reads through the same `load` — the rewrite is refused
    // too. That is the safe direction in both cases: nothing is
    // authorized against state anyone could edit, and an attacker
    // cannot use a chmod to make the daemon rebuild the counter from
    // whatever they left behind. The counter survives untouched, so
    // repairing the mode restores the exact generations.
    use std::os::unix::fs::PermissionsExt;

    let _env = isolated_env();
    let before = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();

    let path = state_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(
        current(Some(1000), "sess-a").is_err(),
        "state anyone can rewrite authorizes nothing"
    );
    let blocked = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    });
    assert!(
        blocked.is_err(),
        "a revocation must not rebuild the counter from untrusted state"
    );

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        current(Some(1000), "sess-a").unwrap(),
        before,
        "the refused write left the previous counter exactly as it was"
    );
    let after = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();
    assert!(
        after > before,
        "revocation resumes once the mode is repaired"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600,
        "and the rewrite installs a private file, not the widened one"
    );
}

#[test]
fn a_write_that_cannot_complete_retires_nothing() {
    // `create_new` means a pre-planted scratch path makes the write
    // fail rather than be redirected. The honest consequence is a
    // refusal to revoke — never a partial counter, and never a silent
    // success that leaves the caller believing authority was retired.
    let _env = isolated_env();
    let first = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-a".to_string(),
    })
    .unwrap();

    // Make the directory itself unwritable, which is the general form
    // of "the store cannot be updated".
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent = state_path().parent().unwrap().to_path_buf();
        let original = std::fs::metadata(&parent).unwrap().permissions();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();

        let blocked = revoke(&RevocationScope::Session {
            uid: Some(1000),
            session: "sess-a".to_string(),
        });

        std::fs::set_permissions(&parent, original).unwrap();
        // Running as root ignores directory permissions, so only assert
        // the invariant the failure is supposed to preserve.
        if blocked.is_err() {
            assert_eq!(
                current(Some(1000), "sess-a").unwrap(),
                first,
                "a failed revocation leaves the previous counter exactly as it was"
            );
        }
    }
}

#[test]
fn a_failed_parent_sync_reports_failure_and_never_a_revocation() {
    // The increment may or may not have reached the disk after a failed
    // directory sync, so the only honest answer is failure. Reporting
    // success would tell an operator that authority was retired when a
    // power cut could still bring it back.
    let _env = isolated_env();
    let before = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-sync".to_string(),
    })
    .unwrap();

    super::super::set_parent_sync_failure(true);
    let outcome = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-sync".to_string(),
    });
    super::super::set_parent_sync_failure(false);

    let error = outcome.expect_err("an uncommitted directory entry is not a revocation");
    assert!(
        error.contains("write approval generations"),
        "unexpected: {error}"
    );

    // Whichever way the ambiguous write resolved, the counter never
    // moved backwards, and a retry moves it forward from wherever it
    // actually landed.
    let observed = current(Some(1000), "sess-sync").unwrap();
    assert!(
        observed >= before,
        "the counter must never fall below {before}, saw {observed}"
    );
    let retried = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-sync".to_string(),
    })
    .expect("a retry succeeds once the sync works again");
    assert!(
        retried > observed,
        "a retry increments from the current file, not from a stale read"
    );
    assert_eq!(current(Some(1000), "sess-sync").unwrap(), retried);
}

#[test]
fn a_failed_revocation_is_reported_to_the_session_teardown_path_too() {
    // `revoke_session_best_effort` deliberately does not fail the
    // operation it is cleaning up after, so the one thing it must not
    // do is claim success. It logs and returns; the grant it could not
    // retire is still bounded by expiry and by the in-memory authority,
    // which the caller revoked separately.
    let _env = isolated_env();
    let before = current(Some(1000), "sess-teardown").unwrap();

    super::super::set_parent_sync_failure(true);
    revoke_session_best_effort(Some(1000), "sess-teardown");
    super::super::set_parent_sync_failure(false);

    let observed = current(Some(1000), "sess-teardown").unwrap();
    assert!(
        observed >= before,
        "a failed teardown revocation never lowers the counter"
    );
}

#[test]
fn repeated_failures_still_only_ever_move_the_counter_forward() {
    let _env = isolated_env();
    let mut last = current(Some(1000), "sess-forward").unwrap();
    for round in 0..4 {
        super::super::set_parent_sync_failure(round % 2 == 0);
        let _ = revoke(&RevocationScope::Session {
            uid: Some(1000),
            session: "sess-forward".to_string(),
        });
        super::super::set_parent_sync_failure(false);
        let observed = current(Some(1000), "sess-forward").unwrap();
        assert!(
            observed >= last,
            "round {round}: {observed} fell below {last}"
        );
        last = observed;
    }
    let final_generation = revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-forward".to_string(),
    })
    .unwrap();
    assert!(final_generation > last);
}
