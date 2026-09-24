use super::*;

#[test]
fn built_in_hook_settings_are_closed_and_apply_to_future_tasks() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hooks.json");

    let initial = get_at(&path).unwrap();
    assert_eq!(initial["applies_to"], "future_tasks");
    assert_eq!(
        initial["hooks"],
        json!([
            {"kind": "logging", "enabled": false},
            {"kind": "audit", "enabled": false},
            {"kind": "checkpoint", "enabled": false},
        ])
    );

    let enabled = set_at(&path, HookKind::Checkpoint, true).unwrap();
    assert_eq!(enabled["updated_kind"], "checkpoint");
    assert_eq!(enabled["changed"], true);
    assert_eq!(enabled["hooks"][2]["enabled"], true);
    let unchanged = set_at(&path, HookKind::Checkpoint, true).unwrap();
    assert_eq!(unchanged["changed"], false);
    let disabled = set_at(&path, HookKind::Checkpoint, false).unwrap();
    assert_eq!(disabled["hooks"][2]["enabled"], false);
}

#[test]
fn broker_hook_settings_refuse_root_owner() {
    let client = ClientIdentity {
        uid: Some(0),
        ..ClientIdentity::unknown()
    };
    assert_eq!(get(json!({}), &client).unwrap_err(), ROOT_OWNER_REFUSAL);
}

#[cfg(target_os = "linux")]
#[test]
fn broker_hook_settings_round_trip_in_the_authenticated_owner_partition() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = crate::test_env::lock_env();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", root.path());
    let client = ClientIdentity {
        uid: Some(owner_uid),
        gid: Some(unsafe { libc::getegid() } as u32),
        ..ClientIdentity::unknown()
    };

    let updated = set(json!({ "kind": "audit", "enabled": true }), &client).unwrap();
    assert_eq!(updated["updated_kind"], "audit");
    assert_eq!(updated["changed"], true);
    let current = get(json!({}), &client).unwrap();
    assert_eq!(current["hooks"][1]["enabled"], true);

    let path = crate::paths::clawd_user_agent_state_dir(owner_uid).join("hooks.json");
    assert!(path.is_file());
}
