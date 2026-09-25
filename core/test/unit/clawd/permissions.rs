use super::*;
use crate::test_env::TestEnvVarGuard;

fn client(uid: u32, attended_local: bool) -> ClientIdentity {
    ClientIdentity {
        uid: Some(uid),
        gid: Some(1000),
        pid: Some(std::process::id()),
        execution_uid: None,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        attended_local,
        extension_host: None,
    }
}

fn pending_for(owner_uid: u32) -> String {
    approvals::submit_owned(
        Verb::FS_WRITE,
        Scope::path("/home/owner/report.txt"),
        "session-a",
        "Write the requested report",
        Some("Claw Agent".into()),
        Some(owner_uid),
    )
    .unwrap()
}

#[test]
fn attended_owner_can_decide_only_its_exact_one_shot_request() {
    let root = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().join("data"));
    let _caps = TestEnvVarGuard::set("COS_CAPS_DATA_DIR", root.path().join("caps"));
    let owner_uid = 1000;

    let denied_id = pending_for(owner_uid);
    let denied = decide(
        json!({"id": denied_id, "decision": "deny"}),
        &client(owner_uid, true),
    )
    .unwrap();
    assert_eq!(denied["id"], denied_id);
    assert_eq!(denied["decision"], "denied");
    assert_eq!(
        approvals::status_for_owner(&denied_id, Some(owner_uid)).as_str(),
        "denied"
    );

    let approval_id = pending_for(owner_uid);
    let approved = decide(
        json!({"id": approval_id, "decision": "approve", "duration": "once"}),
        &client(owner_uid, true),
    )
    .unwrap();
    assert_eq!(approved["id"], approval_id);
    assert_eq!(approved["decision"], "approved");
    assert_eq!(
        approvals::status_for_owner(&approval_id, Some(owner_uid)).as_str(),
        "approved"
    );

    let unattended_id = pending_for(owner_uid);
    let error = decide(
        json!({"id": unattended_id, "decision": "approve", "duration": "once"}),
        &client(owner_uid, false),
    )
    .unwrap_err();
    assert!(error.contains("attended local terminal"));
    assert_eq!(
        approvals::status_for_owner(&unattended_id, Some(owner_uid)).as_str(),
        "pending"
    );

    let reusable_id = pending_for(owner_uid);
    let error = decide(
        json!({"id": reusable_id, "decision": "approve", "duration": "session"}),
        &client(owner_uid, true),
    )
    .unwrap_err();
    assert!(error.contains("one exact request"));
    assert_eq!(
        approvals::status_for_owner(&reusable_id, Some(owner_uid)).as_str(),
        "pending"
    );

    let foreign_id = pending_for(owner_uid);
    assert!(decide(
        json!({"id": foreign_id, "decision": "deny"}),
        &client(owner_uid + 1, true),
    )
    .is_err());
    assert!(decide(
        json!({"id": foreign_id, "decision": "deny", "owner_uid": owner_uid}),
        &client(owner_uid, true),
    )
    .is_err());
    assert_eq!(
        approvals::status_for_owner(&foreign_id, Some(owner_uid)).as_str(),
        "pending"
    );
}
