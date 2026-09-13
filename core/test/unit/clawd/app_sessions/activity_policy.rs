use super::*;
use crate::activities::ActivityService;
use crate::caps::activity_boundary::ActivityBoundary;
use crate::test_env::TestEnvVarGuard;
use serde_json::json;

fn boundary_context(mode: &str) -> (String, Arc<ActivityBoundary>) {
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            4242,
            serde_json::from_value(json!({
                "title":"Read draft","goal":"Prepare a result"
            }))
            .unwrap(),
        )
        .unwrap();
    service.set_capability_policy(4242, &activity.id, None, serde_json::from_value(json!({
        "rules":[{"verb":"fs.read","mode":mode,"scopes":
            if mode == "deny" { json!([]) } else { json!([{"kind":"path","value":"/home/u/**"}]) }
        }]
    })).unwrap()).unwrap();
    let job: crate::agent::service::Job = serde_json::from_value(json!({
        "id":"app-plan-task","prompt":"test","status":"running","created_at":"2026-01-01T00:00:00Z",
        "owner_uid":4242,"session_id":"cli-test","activity_id":activity.id,
    }))
    .unwrap();
    (
        activity.id,
        ActivityBoundary::for_job(&job).unwrap().unwrap(),
    )
}

fn approve(cap: &Cap, duration: crate::approvals::GrantDuration) {
    let id = crate::approvals::submit_owned(
        cap.verb,
        cap.scope.clone(),
        "cli-test",
        "test",
        Some("test".into()),
        Some(4242),
    )
    .unwrap();
    crate::approvals::approve_for_owner(&id, duration, Some("test".into()), None, Some(4242))
        .unwrap();
}

#[test]
fn an_activity_asks_for_held_caps_once_and_cannot_borrow_a_broader_approval() {
    let _consent = approval_sandbox();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let (_, boundary) = boundary_context("require_approval");
    let cap = Cap::new(Verb::FS_READ, Scope::path("/home/u/draft"));
    let mut delegation = delegation(CapSet::from_caps([cap.clone()]));
    delegation.activity = Some(boundary.clone());
    let plan = || {
        let mut plan = LaunchPlan::default();
        plan.require(cap.clone(), &delegation);
        plan.require(cap.clone(), &delegation);
        plan
    };
    let broad = Cap::new(Verb::FS_READ, Scope::path("/home/u/**"));
    approve(&broad, crate::approvals::GrantDuration::Session);
    let error = authorize_plan(&delegation, plan(), &publisher_ceiling(), "fs").unwrap_err();
    assert_eq!(approval_requests(&error).len(), 1);
    assert_eq!(
        crate::approvals::list_pending_for_owner(Some(4242))[0].scope,
        cap.scope
    );
    approve_only_pending(4242, crate::approvals::GrantDuration::Session);
    let granted = authorize_plan(&delegation, plan(), &publisher_ceiling(), "fs").unwrap();
    assert_eq!(granted.len(), 1);
    let binding = boundary.binding_for_authorized(&granted).unwrap();
    binding.check_delegated(std::slice::from_ref(&cap)).unwrap();
    binding.check_delegated(std::slice::from_ref(&cap)).unwrap();
    assert!(!crate::approvals::has_approved_grant_for_owner("cli-test", &cap, Some(4242)).unwrap());
    assert!(
        crate::approvals::has_approved_grant_for_owner("cli-test", &broad, Some(4242)).unwrap()
    );
    assert!(authorize_plan(&delegation, plan(), &publisher_ceiling(), "fs").is_err());
}

#[test]
fn forced_and_missing_app_capabilities_are_settled_all_or_none() {
    let _consent = approval_sandbox();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let (_, boundary) = boundary_context("require_approval");
    let held = Cap::new(Verb::FS_READ, Scope::path("/home/u/draft"));
    let missing = Cap::new(Verb::SYS_PACKAGE, Scope::name("test-package"));
    let mut delegation = delegation(CapSet::from_caps([held.clone()]));
    delegation.activity = Some(boundary);
    let plan = || {
        let mut plan = LaunchPlan::default();
        plan.require(held.clone(), &delegation);
        plan.require(missing.clone(), &delegation);
        plan
    };
    approve(&held, crate::approvals::GrantDuration::Once);
    let error = authorize_plan(&delegation, plan(), &publisher_ceiling(), "fs").unwrap_err();
    assert_eq!(approval_requests(&error).len(), 1);
    assert!(crate::approvals::has_approved_grant_for_owner("cli-test", &held, Some(4242)).unwrap());
    approve_only_pending(4242, crate::approvals::GrantDuration::Once);
    let caps = authorize_plan(&delegation, plan(), &publisher_ceiling(), "fs").unwrap();
    assert!(caps.covers(&held) && caps.covers(&missing));
    assert!(
        !crate::approvals::has_approved_grant_for_owner("cli-test", &held, Some(4242)).unwrap()
    );
    assert!(
        !crate::approvals::has_approved_grant_for_owner("cli-test", &missing, Some(4242)).unwrap()
    );
}

#[test]
fn a_denied_or_stale_plan_neither_creates_nor_consumes_approvals() {
    let _consent = approval_sandbox();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let (id, boundary) = boundary_context("normal");
    let cap = Cap::new(Verb::FS_READ, Scope::path("/home/u/draft"));
    approve(&cap, crate::approvals::GrantDuration::Once);
    let mut delegation = delegation(CapSet::from_caps([cap.clone()]));
    delegation.activity = Some(boundary);
    crate::activities::open_default()
        .unwrap()
        .set_capability_policy(
            4242,
            &id,
            Some(1),
            serde_json::from_value(json!({
                "rules":[{"verb":"fs.read","mode":"deny","scopes":[]}]
            }))
            .unwrap(),
        )
        .unwrap();
    let mut plan = LaunchPlan::default();
    plan.require(cap.clone(), &delegation);
    let error = authorize_plan(&delegation, plan, &publisher_ceiling(), "fs").unwrap_err();
    assert!(error.message.contains("changed"));
    assert!(crate::approvals::list_pending_for_owner(Some(4242)).is_empty());
    assert!(crate::approvals::has_approved_grant_for_owner("cli-test", &cap, Some(4242)).unwrap());
    let (_, fresh) = boundary_context("deny");
    delegation.activity = Some(fresh);
    let mut plan = LaunchPlan::default();
    plan.require(cap.clone(), &delegation);
    let error = authorize_plan(&delegation, plan, &publisher_ceiling(), "fs").unwrap_err();
    assert!(error.message.contains("cannot override"));
    assert!(crate::approvals::list_pending_for_owner(Some(4242)).is_empty());
    assert!(crate::approvals::has_approved_grant_for_owner("cli-test", &cap, Some(4242)).unwrap());
}
