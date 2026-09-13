use super::*;
use crate::activities::CapabilityPolicyDraft;
use crate::caps::{Scope, Verb};
use crate::test_env::TestEnvVarGuard;
use serde_json::json;

fn create(owner: u32) -> String {
    crate::clawd::activities::create(
        json!({"title":"Scoped work","goal":"Prepare a document"}),
        &crate::clawd::client_identity::ClientIdentity {
            uid: Some(owner),
            pid: Some(std::process::id()),
            gid: Some(owner),
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            ..crate::clawd::client_identity::ClientIdentity::unknown()
        },
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn ask_policy() -> CapabilityPolicyDraft {
    serde_json::from_value(json!({"rules":[{
        "verb":"fs.write","mode":"require_approval",
        "scopes":[{"kind":"path","value":"/workspace/**"}]
    }]}))
    .unwrap()
}

#[cfg(unix)]
#[test]
fn runtime_scope_denies_symlink_escapes_and_new_leaves() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let allowed = data.path().join("drafts");
    let outside = data.path().join("originals");
    std::fs::create_dir(&allowed).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("document"), "original").unwrap();
    std::os::unix::fs::symlink(&outside, allowed.join("escape")).unwrap();
    let policy_scope = Scope::path(format!("{}/**", allowed.display()));
    let id = create(1000);
    let service = crate::activities::open_default().unwrap();
    service
        .set_capability_policy(
            1000,
            &id,
            None,
            serde_json::from_value(json!({
                "rules":[
                    {"verb":"fs.read","mode":"normal","scopes":[policy_scope.clone()]},
                    {"verb":"fs.write","mode":"require_approval","scopes":[policy_scope.clone()]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
    let boundary = ActivityBoundary::open(1000, &id, Some("task".into())).unwrap();
    let read = Cap::new(
        Verb::FS_READ,
        Scope::path(allowed.join("escape/document").to_string_lossy()),
    );
    assert!(!policy_scope.covers(&read.scope));
    assert!(Scope::path(format!("{}/**", data.path().display())).covers(&read.scope));
    assert_eq!(
        boundary.decision(&read).unwrap(),
        CapabilityBoundaryDecision::Deny
    );
    let write = Cap::new(
        Verb::FS_WRITE,
        Scope::path(allowed.join("escape/new-document").to_string_lossy()),
    );
    assert_eq!(
        boundary.decision(&write).unwrap(),
        CapabilityBoundaryDecision::Deny
    );
    assert!(boundary
        .approval_needs(&CapSet::from_caps([write]))
        .is_err());
    let within = Cap::new(
        Verb::FS_READ,
        Scope::path(allowed.join("new-draft").to_string_lossy()),
    );
    assert_eq!(
        boundary.decision(&within).unwrap(),
        CapabilityBoundaryDecision::Normal
    );
}

#[test]
fn runtime_scope_confirmation_cannot_widen_symbolic_request() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let id = create(1000);
    crate::activities::open_default()
        .unwrap()
        .set_capability_policy(1000, &id, None, ask_policy())
        .unwrap();
    let boundary = ActivityBoundary::open(1000, &id, Some("task".into())).unwrap();
    let approved = Cap::new(Verb::FS_WRITE, Scope::path("/workspace/*"));
    let binding = boundary
        .binding_for_authorized(&CapSet::from_caps([approved]))
        .unwrap();
    let broader = Cap::new(Verb::FS_WRITE, Scope::path("/workspace/**"));
    assert!(binding.check_delegated(&[broader]).is_err());
}

#[test]
fn a_new_policy_invalidates_an_already_running_unrestricted_attempt() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let id = create(1000);
    let boundary = ActivityBoundary::open(1000, &id, Some("task".into())).unwrap();
    let cap = Cap::new(
        super::super::Verb::FS_WRITE,
        super::super::Scope::path("/workspace/draft"),
    );
    assert_eq!(
        boundary.decision(&cap).unwrap(),
        CapabilityBoundaryDecision::Normal
    );
    crate::activities::open_default()
        .unwrap()
        .set_capability_policy(1000, &id, None, ask_policy())
        .unwrap();
    assert!(boundary
        .check()
        .unwrap_err()
        .contains("fresh execution attempt"));
    assert!(boundary.decision(&cap).is_err());
    assert!(ActivityBoundary::open(0, &id, None).is_err());
    assert!(ActivityBoundary::open(2000, &id, None).is_err());
}

#[test]
fn a_policy_binding_is_not_confirmation_and_does_not_survive_revision_changes() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let id = create(1000);
    let service = crate::activities::open_default().unwrap();
    service
        .set_capability_policy(1000, &id, None, ask_policy())
        .unwrap();
    let boundary = ActivityBoundary::open(1000, &id, Some("task".into())).unwrap();
    let cap = Cap::new(
        super::super::Verb::FS_WRITE,
        super::super::Scope::path("/workspace/draft"),
    );
    let outside = Cap::new(
        super::super::Verb::FS_WRITE,
        super::super::Scope::path("/outside"),
    );
    assert_eq!(
        boundary.decision(&cap).unwrap(),
        CapabilityBoundaryDecision::RequireApproval
    );
    assert_eq!(
        boundary.decision(&outside).unwrap(),
        CapabilityBoundaryDecision::Deny
    );
    assert!(boundary
        .binding()
        .check_delegated(std::slice::from_ref(&cap))
        .is_err());
    let confirmed = boundary
        .binding_for_authorized(&CapSet::from_caps([cap.clone()]))
        .unwrap();
    confirmed
        .check_delegated(std::slice::from_ref(&cap))
        .unwrap();
    assert!(confirmed.check_delegated(&[outside]).is_err());
    assert!(confirmed
        .check_delegated(&[Cap::new(
            super::super::Verb::FS_WRITE,
            super::super::Scope::path("/workspace/other"),
        )])
        .is_err());
    service
        .set_capability_policy_enabled(1000, &id, 1, false)
        .unwrap();
    assert!(boundary.check().is_err());
    assert!(confirmed.check_delegated(&[cap]).is_err());
    assert!(ActivityBoundary::open(1000, &id, Some("task".into())).is_err());
    service
        .set_capability_policy_enabled(1000, &id, 2, true)
        .unwrap();
    assert!(
        confirmed.check().is_err(),
        "re-enabling cannot revive an old grant"
    );
}

#[tokio::test]
async fn task_local_boundaries_check_owner_session_and_do_not_escape_the_scope() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let id = create(1000);
    let boundary = ActivityBoundary::open(1000, &id, Some("task".into())).unwrap();
    scope(Some(boundary), async {
        assert!(ActivityBoundary::for_session(1000, "task")
            .unwrap()
            .is_some());
        assert!(ActivityBoundary::for_session(0, "task").is_err());
        assert!(ActivityBoundary::for_session(1000, "other").is_err());
        scope(None, async {
            assert!(current().is_none());
        })
        .await;
        assert!(current().is_some());
    })
    .await;
    assert!(current().is_none());
}
