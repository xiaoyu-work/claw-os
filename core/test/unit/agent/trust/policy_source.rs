// The operator-policy source gate.
//
// A configured prompt file is the only non-compiled thing that may
// enter the policy channel, and only when an administrator — not the
// session owner — could have authored it. Everything a normal owner can
// reach must stay `UserControlledContext`.

use super::*;
use crate::agent::prompt::{operator_prompt_kind_for_test, root_authored_for_test};

#[test]
fn a_normal_owner_config_file_is_never_policy() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("preface.md");
    std::fs::write(&path, "owner preface").expect("write");

    assert!(!root_authored_for_test(&path));
    assert_eq!(
        operator_prompt_kind_for_test(&path),
        Some(SourceKind::OperatorPromptFile)
    );
    assert_eq!(
        SourceKind::OperatorPromptFile.class(),
        TrustClass::UserControlledContext
    );
    assert!(!SourceKind::OperatorPromptFile.class().is_policy());
}

#[test]
fn a_missing_or_empty_file_yields_no_segment() {
    let dir = tempfile::tempdir().expect("tmp");
    assert_eq!(operator_prompt_kind_for_test(&dir.path().join("nope.md")), None);

    let empty = dir.path().join("empty.md");
    std::fs::write(&empty, "   \n\n").expect("write");
    assert_eq!(operator_prompt_kind_for_test(&empty), None);
}

#[test]
fn an_oversized_file_is_refused_entirely() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("huge.md");
    std::fs::write(&path, "x".repeat(256 * 1024 + 1)).expect("write");
    assert_eq!(operator_prompt_kind_for_test(&path), None);
}

#[cfg(unix)]
mod unix_ownership {
    use super::*;

    /// Running as root would make the owner-writable cases
    /// indistinguishable from administrator ones, so those assertions
    /// are skipped rather than silently inverted.
    fn running_as_root() -> bool {
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn a_file_under_an_owner_writable_directory_is_not_policy() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("preface.md");
        std::fs::write(&path, "rules").expect("write");
        // The temp directory is owned by the test user, so even a
        // read-only file inside it is not administrator-authored.
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o444);
        std::fs::set_permissions(&path, perms).expect("chmod");
        assert!(!root_authored_for_test(&path));
    }

    #[test]
    fn a_symlink_into_owner_space_is_not_policy() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let target = dir.path().join("owned.md");
        std::fs::write(&target, "owner rules").expect("write");
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        // Canonicalisation follows the link, so the *target's*
        // ancestors decide — and they are owner-writable.
        assert!(!root_authored_for_test(&link));
        assert_eq!(
            operator_prompt_kind_for_test(&link),
            Some(SourceKind::OperatorPromptFile)
        );
    }

    #[test]
    fn a_symlink_to_a_root_owned_file_through_owner_space_is_not_policy() {
        if running_as_root() {
            return;
        }
        // /etc/hostname is root-owned on a normal system, but reaching
        // it through an owner-controlled link must not confer policy on
        // the *link*; the check must resolve and re-verify.
        let root_owned = std::path::Path::new("/etc/hostname");
        if !root_owned.exists() {
            return;
        }
        let dir = tempfile::tempdir().expect("tmp");
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(root_owned, &link).expect("symlink");
        // Resolution lands on a genuinely root-owned path, so this is
        // allowed to be policy — the point is that the decision is made
        // about the resolved target, never about the link.
        let resolved = std::fs::canonicalize(&link).expect("canonicalize");
        assert_eq!(root_authored_for_test(&link), root_authored_for_test(&resolved));
    }

    #[test]
    fn a_root_owned_system_file_is_recognised_as_policy() {
        // A file that really is root-owned with no group/other write
        // bit anywhere on its path is the one promotion case.
        for candidate in ["/etc/hostname", "/etc/os-release", "/etc/hosts"] {
            let path = std::path::Path::new(candidate);
            if !path.is_file() {
                continue;
            }
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let uid = std::os::unix::fs::MetadataExt::uid(&meta);
            let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
            if uid != 0 || mode & 0o022 != 0 {
                continue;
            }
            assert!(
                root_authored_for_test(path),
                "{candidate} is root-owned and locked down but was refused"
            );
            assert_eq!(
                operator_prompt_kind_for_test(path),
                Some(SourceKind::RootOperatorPolicyFile)
            );
            assert_eq!(
                SourceKind::RootOperatorPolicyFile.class(),
                TrustClass::SystemPolicy
            );
            return;
        }
    }

    #[test]
    fn a_world_writable_root_owned_file_is_refused() {
        if running_as_root() {
            return;
        }
        // /tmp is root-owned but world-writable; nothing under it can
        // be administrator policy.
        let path = std::path::Path::new("/tmp");
        if path.exists() {
            assert!(!root_authored_for_test(path));
        }
    }
}

/// Whatever the ownership outcome, only these two kinds may ever be
/// policy, and only they may take the policy projection.
#[test]
fn the_policy_channel_admits_exactly_two_sources() {
    let policy: Vec<_> = SourceKind::ALL
        .iter()
        .filter(|kind| kind.class().is_policy())
        .copied()
        .collect();
    assert_eq!(
        policy,
        vec![
            SourceKind::SystemScaffold,
            SourceKind::RootOperatorPolicyFile
        ]
    );
    for kind in policy {
        assert_eq!(kind.projection(), Projection::PolicyChannel);
    }
}
