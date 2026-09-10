use super::*;

fn isolated(test: impl FnOnce(&std::path::Path)) {
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps =
        crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    test(root.path());
}

#[test]
fn system_review_revisions_are_stable_monotonic_and_owner_scoped() {
    isolated(|_| {
        let id = "rv-0123456789abcdef0123456789abcdef";
        let first = "a".repeat(64);
        let changed = "b".repeat(64);
        assert_eq!(revision(1000, id, &first).unwrap(), 1);
        assert_eq!(revision(1000, id, &first).unwrap(), 1);
        assert_eq!(revision(1001, id, &changed).unwrap(), 1);
        assert_eq!(revision(1000, id, &changed).unwrap(), 2);
        assert_eq!(revision(1000, id, &first).unwrap(), 3);
        assert!(super::super::list_recent(100).is_empty());
    });
}

#[test]
fn system_review_revision_rejects_invalid_keys_and_counter_overflow() {
    isolated(|_| {
        let fingerprint = "a".repeat(64);
        for id in ["", "../elsewhere", "rv-/path", "rv-\n"] {
            assert!(revision(1000, id, &fingerprint).is_err());
        }
        assert!(revision(1000, "rv-fixture", "not-a-digest").is_err());
        let id = "rv-fixture";
        revision(1000, id, &fingerprint).unwrap();
        let path = super::super::root()
            .join("presentation")
            .join(format!("1000-{id}.json"));
        let stamp = Stamp {
            owner_uid: 1000,
            request_id: id.into(),
            fingerprint,
            revision: u64::MAX,
        };
        super::super::write_atomic(&path, &serde_json::to_vec(&stamp).unwrap()).unwrap();
        assert!(revision(1000, id, &"b".repeat(64))
            .unwrap_err()
            .contains("exhausted"));
        assert_eq!(
            read_owned_json::<Stamp>(&path, MAX_STAMP_BYTES)
                .unwrap()
                .unwrap()
                .revision,
            u64::MAX
        );
    });
}

#[test]
fn system_review_revision_refuses_corruption_and_foreign_stamp_ownership() {
    isolated(|_| {
        let id = "rv-fixture";
        let fingerprint = "a".repeat(64);
        revision(1000, id, &fingerprint).unwrap();
        let path = super::super::root()
            .join("presentation")
            .join(format!("1000-{id}.json"));
        let stamp = Stamp {
            owner_uid: 1001,
            request_id: id.into(),
            fingerprint: fingerprint.clone(),
            revision: 1,
        };
        super::super::write_atomic(&path, &serde_json::to_vec(&stamp).unwrap()).unwrap();
        assert!(revision(1000, id, &fingerprint)
            .unwrap_err()
            .contains("ownership"));
        std::fs::write(&path, b"not-json").unwrap();
        assert!(revision(1000, id, &fingerprint).is_err());
    });
}

#[cfg(unix)]
#[test]
fn system_review_protected_reads_reject_public_linked_and_oversized_files() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    isolated(|root| {
        let file = root.join("record.json");
        super::super::write_atomic(&file, b"{}").unwrap();
        assert!(read_owned_json::<serde_json::Value>(&file, 2)
            .unwrap()
            .is_some());
        assert!(read_owned_json::<serde_json::Value>(&file, 1).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_owned_json::<serde_json::Value>(&file, 2).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = root.join("alias.json");
        symlink(&file, &alias).unwrap();
        assert!(read_owned_json::<serde_json::Value>(&alias, 2).is_err());
        let hardlink = root.join("hardlink.json");
        std::fs::hard_link(&file, &hardlink).unwrap();
        assert!(read_owned_json::<serde_json::Value>(&file, 2).is_err());
        assert!(
            read_owned_json::<serde_json::Value>(&root.join("missing"), 2)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn system_review_legacy_queue_and_decisions_do_not_cross_owners() {
    isolated(|_| {
        let submit = |owner| {
            super::super::submit_owned(
                crate::caps::Verb::SYS_OBSERVE,
                crate::caps::Scope::name("hardware"),
                "review-test",
                "Inspect hardware",
                Some("terminal".into()),
                Some(owner),
            )
            .unwrap()
        };
        let first = submit(1000);
        let second = submit(1001);
        assert_eq!(
            legacy_pending(1000)
                .unwrap()
                .iter()
                .map(|request| &request.id)
                .collect::<Vec<_>>(),
            vec![&first]
        );
        assert!(legacy_get(1000, &second).is_err());
        assert!(matches!(
            legacy_get(1000, &first).unwrap(),
            LegacyReview::Pending(_)
        ));
        super::super::deny_for_owner(&first, Some("uid:1000".into()), None, Some(1000)).unwrap();
        assert!(legacy_pending(1000).unwrap().is_empty());
        assert!(matches!(
            legacy_get(1000, &first).unwrap(),
            LegacyReview::Resolved(_)
        ));
        assert!(legacy_get(1001, &first).is_err());
    });
}

#[test]
fn system_review_legacy_queue_reports_bad_records_instead_of_hiding_them() {
    isolated(|_| {
        super::super::ensure_dirs().unwrap();
        let path = super::super::pending_dir().join("ap-0123456789ab.json");
        super::super::write_atomic(&path, b"invalid").unwrap();
        assert!(legacy_pending(1000).is_err());
    });
}
