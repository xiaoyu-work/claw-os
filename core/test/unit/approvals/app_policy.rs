use super::*;
use crate::approvals as approvals_store;
use crate::test_env::TestEnvVarGuard;

struct Fixture {
    _data: TestEnvVarGuard,
    _caps: TestEnvVarGuard,
    _root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build");
    std::fs::create_dir_all(&root).unwrap();
    let dir = tempfile::tempdir_in(root).unwrap();
    Fixture {
        _data: TestEnvVarGuard::set("COS_DATA_DIR", dir.path()),
        _caps: TestEnvVarGuard::set("COS_CAPS_DATA_DIR", dir.path()),
        _root: dir,
    }
}

fn restore(block: &Block, uid: u32, app: &str) -> approvals_store::Resolved {
    let id = approvals_store::submit_owned(
        block.cap.verb,
        block.cap.scope.clone(),
        block.session(uid, app),
        "restore fixture permission",
        None,
        Some(uid),
    )
    .unwrap();
    approvals_store::approve_for_owner(
        &id,
        approvals_store::GrantDuration::Forever,
        Some(format!("uid:{uid}")),
        None,
        Some(uid),
    )
    .unwrap()
}

fn write_receipt(resolved: &approvals_store::Resolved) -> std::path::PathBuf {
    let path = approvals_store::approved_dir().join(format!("{}.json", resolved.request.id));
    approvals_store::write_atomic_with(
        &path,
        &serde_json::to_vec(resolved).unwrap(),
        approvals_store::Durability::Committed,
    )
    .unwrap();
    path
}

fn age(resolved: &mut approvals_store::Resolved, legacy: bool) {
    resolved.decision.decided_at =
        approvals_store::now_secs() - 2 * approvals_store::FOREVER_GRANT_SECS;
    resolved.request.requested_at = resolved.decision.decided_at - 1;
    if legacy {
        let restoration = resolved.decision.restoration.take().unwrap();
        resolved.decision.grant = Some(approvals_store::GrantBinding::mint(
            approvals_store::GrantDuration::Forever,
            &resolved.request.id,
            resolved.decision.decided_at,
            restoration.generation,
            restoration.authorization,
        ));
        assert!(resolved.decision.grant.as_ref().unwrap().expires_at < approvals_store::now_secs());
    }
    write_receipt(resolved);
}

#[test]
fn restoration_survives_process_restart_beyond_execution_ttl_including_existing_receipts() {
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    let caps = CapSet::from_caps([cap.clone()]);
    if std::env::var_os("CLAW_PERMISSION_RESTORED_FIXTURE").is_some() {
        assert!(require(1000, "audio-manager", &caps).is_ok());
        let block = blocks(1000, "audio-manager").unwrap().remove(0);
        assert!(block.enabled(1000, "audio-manager").unwrap());
        assert!(!approvals_store::has_approved_grant_for_owner(
            &block.session(1000, "audio-manager"),
            &cap,
            Some(1000),
        )
        .unwrap());
        assert!(
            !approvals_store::has_approved_grant_for_owner("ordinary-execution", &cap, Some(1000),)
                .unwrap(),
            "ordinary forever execution approvals still expire after 30 days"
        );
        return;
    }
    let _lock = crate::test_env::lock_env();
    for legacy in [false, true] {
        let _fixture = fixture();
        let block = revoke(1000, "audio-manager", cap.clone()).unwrap();
        let mut receipt = restore(&block, 1000, "audio-manager");
        age(&mut receipt, legacy);
        let path = write_receipt(&receipt);
        let before = std::fs::read(&path).unwrap();
        assert!(require(1000, "audio-manager", &caps).is_ok());
        assert!(!approvals_store::has_approved_grant_for_owner(
            &block.session(1000, "audio-manager"),
            &cap,
            Some(1000),
        )
        .unwrap());
        assert!(approvals_store::spend_grant(&path, receipt)
            .unwrap()
            .is_none());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "policy receipts are never consumed"
        );
        let id = approvals_store::submit_owned(
            cap.verb,
            cap.scope.clone(),
            "ordinary-execution",
            "fixture",
            None,
            Some(1000),
        )
        .unwrap();
        let mut ordinary = approvals_store::approve_for_owner(
            &id,
            approvals_store::GrantDuration::Forever,
            None,
            None,
            Some(1000),
        )
        .unwrap();
        let grant = ordinary.decision.grant.as_mut().unwrap();
        assert_eq!(
            grant.expires_at,
            ordinary.decision.decided_at + approvals_store::FOREVER_GRANT_SECS
        );
        grant.expires_at -= 2 * approvals_store::FOREVER_GRANT_SECS;
        ordinary.decision.decided_at -= 2 * approvals_store::FOREVER_GRANT_SECS;
        write_receipt(&ordinary);
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact",
                "approvals::app_policy::tests::restoration_survives_process_restart_beyond_execution_ttl_including_existing_receipts",
                "--test-threads=1"])
            .env("CLAW_PERMISSION_RESTORED_FIXTURE", "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn old_restoration_cannot_undo_app_session_or_owner_revocation() {
    let _lock = crate::test_env::lock_env();
    for legacy in [false, true] {
        let _fixture = fixture();
        let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
        let block = revoke(1000, "audio-manager", cap.clone()).unwrap();
        let other_owner = revoke(1001, "audio-manager", cap.clone()).unwrap();
        let other_app = revoke(1000, "another-app", cap.clone()).unwrap();
        let mut receipt = restore(&block, 1000, "audio-manager");
        age(&mut receipt, legacy);
        assert!(block.enabled(1000, "audio-manager").unwrap());
        assert!(!other_owner.enabled(1001, "audio-manager").unwrap());
        assert!(!other_app.enabled(1000, "another-app").unwrap());
        approvals_store::generations::revoke(&approvals_store::RevocationScope::Session {
            uid: Some(1000),
            session: block.session(1000, "audio-manager"),
        })
        .unwrap();
        write_receipt(&receipt);
        assert!(!block.enabled(1000, "audio-manager").unwrap());
        let renewed = restore(&block, 1000, "audio-manager");
        assert!(block.enabled(1000, "audio-manager").unwrap());
        approvals_store::generations::revoke(&approvals_store::RevocationScope::Owner {
            uid: Some(1000),
        })
        .unwrap();
        write_receipt(&receipt);
        write_receipt(&renewed);
        assert!(!block.enabled(1000, "audio-manager").unwrap());
        restore(&block, 1000, "audio-manager");
        assert!(block.enabled(1000, "audio-manager").unwrap());
        let next = revoke(1000, "audio-manager", cap).unwrap();
        write_receipt(&receipt);
        write_receipt(&renewed);
        assert!(!next.enabled(1000, "audio-manager").unwrap());
        assert!(
            !block.enabled(1000, "audio-manager").unwrap(),
            "stale policy snapshots stay revoked"
        );
        assert!(!other_owner.enabled(1001, "audio-manager").unwrap());
        assert!(!other_app.enabled(1000, "another-app").unwrap());
    }
}

#[test]
fn restoration_requires_exact_request_and_authorization_not_just_a_session_prefix() {
    let _lock = crate::test_env::lock_env();
    let corruptions: &[fn(&mut serde_json::Value, &str)] = &[
        |value, _| {
            value["owner_uid"] = serde_json::json!(1001);
        },
        |value, _| {
            value["session"] = serde_json::json!("app-permission:1000:another-app:1:forged");
        },
        |value, _| {
            value["scope"] = serde_json::json!({"kind":"name","value":"camera"});
        },
        |value, _| {
            value["risk"] = serde_json::json!("critical");
        },
        |value, _| {
            value["operation_digest"] = serde_json::json!("a".repeat(64));
        },
        |value, field| {
            value["decision"][field]["authorization"]["owner_uid"] = serde_json::json!(1001);
        },
        |value, field| {
            value["decision"][field]["authorization"]["session"] = serde_json::json!("forged");
        },
        |value, field| {
            value["decision"][field]["authorization"]["capability"]["scope"] =
                serde_json::json!({"kind":"name","value":"camera"});
        },
        |value, field| {
            value["decision"][field]["authorization"]["context"] = serde_json::json!("attended");
        },
        |value, field| {
            value["decision"][field]["generation"] = serde_json::json!(100);
        },
        |value, field| {
            value["decision"][field]
                .as_object_mut()
                .unwrap()
                .remove("authorization");
        },
        |value, _| {
            value["decision"]["outcome"] = serde_json::json!("denied");
        },
        |value, _| {
            value["decision"]["duration"] = serde_json::json!("once");
        },
    ];
    for legacy in [false, true] {
        let _fixture = fixture();
        let block = revoke(
            1000,
            "audio-manager",
            Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio")),
        )
        .unwrap();
        let mut receipt = restore(&block, 1000, "audio-manager");
        age(&mut receipt, legacy);
        let path = write_receipt(&receipt);
        let value = serde_json::to_value(&receipt).unwrap();
        for corrupt in corruptions {
            let mut changed = value.clone();
            corrupt(&mut changed, if legacy { "grant" } else { "restoration" });
            approvals_store::write_atomic(&path, &serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                !block
                    .enabled(1000, "audio-manager")
                    .is_ok_and(|enabled| enabled),
                "{changed}"
            );
        }
        write_receipt(&receipt);
        assert!(block.enabled(1000, "audio-manager").unwrap());
    }
}

#[test]
fn unreadable_policy_or_corrupt_receipts_fail_explicitly_closed() {
    let _lock = crate::test_env::lock_env();
    let _fixture = fixture();
    let block = revoke(
        1000,
        "audio-manager",
        Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio")),
    )
    .unwrap();
    let receipt = restore(&block, 1000, "audio-manager");
    let path = write_receipt(&receipt);
    std::fs::write(&path, b"{incomplete").unwrap();
    assert!(block
        .enabled(1000, "audio-manager")
        .unwrap_err()
        .contains("parse approval"));
    write_receipt(&receipt);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(block.enabled(1000, "audio-manager").is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let linked = path.with_extension("saved");
    std::fs::rename(&path, &linked).unwrap();
    std::os::unix::fs::symlink(&linked, &path).unwrap();
    assert!(block.enabled(1000, "audio-manager").is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(linked, path).unwrap();
    let state = approvals_store::generations::state_path_for_test();
    std::fs::write(&state, "{incomplete").unwrap();
    assert!(require(1000, "audio-manager", &CapSet::from_caps([block.cap])).is_err());
}

#[test]
fn stale_pending_request_is_rechecked_inside_the_decision_lock() {
    let _lock = crate::test_env::lock_env();
    let _fixture = fixture();
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    let block = revoke(1000, "audio-manager", cap.clone()).unwrap();
    let id = approvals_store::submit_owned(
        cap.verb,
        cap.scope.clone(),
        block.session(1000, "audio-manager"),
        "restore",
        None,
        Some(1000),
    )
    .unwrap();
    revoke(1000, "audio-manager", cap).unwrap();
    let error = approvals_store::approve_for_owner(
        &id,
        approvals_store::GrantDuration::Forever,
        None,
        None,
        Some(1000),
    )
    .unwrap_err();
    assert!(error.contains("stale"), "{error}");
    assert_eq!(
        approvals_store::status_for_owner(&id, Some(1000)),
        approvals_store::RequestStatus::Denied
    );
}

#[test]
fn restoration_requires_committed_policy_receipt_and_reports_projection_failure() {
    let _lock = crate::test_env::lock_env();
    let _fixture = fixture();
    let block = revoke(
        1000,
        "audio-manager",
        Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio")),
    )
    .unwrap();
    let id = approvals_store::submit_owned(
        block.cap.verb,
        block.cap.scope.clone(),
        block.session(1000, "audio-manager"),
        "restore",
        None,
        Some(1000),
    )
    .unwrap();
    approvals_store::set_parent_sync_failure(true);
    let result = approvals_store::approve_for_owner(
        &id,
        approvals_store::GrantDuration::Forever,
        None,
        None,
        Some(1000),
    );
    approvals_store::set_parent_sync_failure(false);
    assert!(result.unwrap_err().contains("sync failure"));
    let log = crate::paths::system_operations_log_path();
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir(&log).unwrap();
    let id = approvals_store::submit_owned(
        block.cap.verb,
        block.cap.scope.clone(),
        block.session(1000, "audio-manager"),
        "restore",
        None,
        Some(1000),
    )
    .unwrap();
    let error = approvals_store::approve_for_owner(
        &id,
        approvals_store::GrantDuration::Forever,
        None,
        None,
        Some(1000),
    )
    .unwrap_err();
    assert!(
        error.contains("decision is committed") && error.contains("journal projection failed"),
        "{error}"
    );
}

#[test]
fn revocation_survives_process_restart() {
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    if std::env::var_os("CLAW_PERMISSION_RESTART_FIXTURE").is_some() {
        assert_eq!(blocks(1000, "audio-manager").unwrap()[0].generation, 1);
        assert!(require(1000, "audio-manager", &CapSet::from_caps([cap])).is_err());
        return;
    }
    let _lock = crate::test_env::lock_env();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build");
    let dir = tempfile::tempdir_in(root).unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let _caps = crate::test_env::TestEnvVarGuard::set("COS_CAPS_DATA_DIR", dir.path());
    revoke(1000, "audio-manager", cap).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "approvals::app_policy::tests::revocation_survives_process_restart",
            "--test-threads=1",
        ])
        .env("CLAW_PERMISSION_RESTART_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn owner_app_revocation_persists_and_restoration_needs_trusted_decision() {
    let _lock = crate::test_env::lock_env();
    let _fixture = fixture();
    let cap = Cap::new(Verb::SYS_OBSERVE, crate::caps::Scope::name("audio"));
    let caps = CapSet::from_caps([cap.clone()]);
    assert!(require(1000, "audio-manager", &caps).is_ok());
    let block = revoke(1000, "audio-manager", cap.clone()).unwrap();
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(require(1001, "audio-manager", &caps).is_ok());
    assert!(require(1000, "camera-manager", &caps).is_ok());
    assert_eq!(blocks(1000, "audio-manager").unwrap()[0].generation, 1);
    let id = super::super::submit_owned(
        cap.verb,
        cap.scope.clone(),
        block.session(1000, "audio-manager"),
        "restore",
        None,
        Some(1000),
    )
    .unwrap();
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Forever,
        None,
        None,
        Some(1001)
    )
    .is_err());
    assert!(super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Once,
        None,
        None,
        Some(1000)
    )
    .is_err());
    super::super::approve_for_owner(
        &id,
        super::super::GrantDuration::Forever,
        None,
        None,
        Some(1000),
    )
    .unwrap();
    assert!(require(1000, "audio-manager", &caps).is_ok());
    let next = revoke(1000, "audio-manager", cap).unwrap();
    assert_eq!(next.generation, 2);
    assert!(require(1000, "audio-manager", &caps).is_err());
    assert!(revoke(1000, "audio-manager", Cap::unscoped(Verb::FS_WRITE)).is_err());
}
