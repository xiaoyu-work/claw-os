use super::*;
use crate::activities::ActivityService;
use crate::caps::activity_boundary::ActivityBoundary;
use crate::test_env::TestEnvVarGuard;
use serde_json::json;

fn activity_boundary() -> (String, std::sync::Arc<ActivityBoundary>) {
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            current_uid(),
            serde_json::from_value(json!({
                "title":"Observe status","goal":"Inspect one resource"
            }))
            .unwrap(),
        )
        .unwrap();
    service.set_capability_policy(current_uid(), &activity.id, None, serde_json::from_value(json!({
        "rules":[{"verb":"sys.observe","mode":"require_approval","scopes":[{"kind":"name","value":"power"}]}]
    })).unwrap()).unwrap();
    let job: crate::agent::service::Job = serde_json::from_value(json!({
        "id":"policy-grant-task","prompt":"test","status":"running","created_at":"2026-01-01T00:00:00Z",
        "owner_uid":current_uid(),"session_id":"policy-grant-session","activity_id":activity.id,
    })).unwrap();
    (
        activity.id,
        ActivityBoundary::for_job(&job).unwrap().unwrap(),
    )
}

#[test]
fn child_grants_inherit_the_policy_binding_and_cannot_replace_it() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let (_, boundary) = activity_boundary();
    let binding = boundary
        .binding_for_authorized(&caps(&[read_cap()]))
        .unwrap();
    let store = store();
    let mut parent = issuance("activity-parent", &[Audience::SystemService]);
    parent.subject = parent.subject.with_activity(Some(binding.clone()));
    parent.index_session = false;
    let (handle, _) = store.issue(parent).unwrap();
    let handle = handle.into_wire();
    let child = || Attenuation {
        issuer: Issuer::AppSessionAuthority,
        principal: self_principal(),
        binding: Binding::ProcessTree,
        subject: Subject::session("activity-child"),
        audience: AudienceSet::one(Audience::SystemService),
        caps: caps(&[read_cap()]),
        lifetime: Duration::from_secs(20),
        uses: Uses::Budget(1),
        index_session: false,
    };
    let (_, inherited) = store.attenuate(&handle, child()).unwrap();
    assert_eq!(inherited.subject.activity.as_ref(), Some(&binding));
    let (_, different) = activity_boundary();
    let mut replacement = child();
    replacement.subject.activity = Some(different.binding());
    assert!(matches!(
        store.attenuate(&handle, replacement),
        Err(AuthorityError::Attenuation(
            AttenuationError::ActivityChanged
        ))
    ));
    let mut foreign = issuance("foreign-binding", &[Audience::SystemService]);
    let mut wrong_owner = binding;
    wrong_owner.owner_uid = current_uid().saturating_add(1);
    foreign.subject.activity = Some(wrong_owner);
    assert_eq!(store.issue(foreign).unwrap_err(), AuthorityError::Subject);
}

#[test]
fn broker_effects_need_the_confirmed_invocation_and_recheck_its_live_revision() {
    let _lock = crate::test_env::lock_env();
    let data = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", data.path());
    let (activity, boundary) = activity_boundary();
    let grant_caps = caps(&[read_cap()]);
    let mut request = issuance("unconfirmed-policy", &[Audience::SystemService]);
    request.caps = grant_caps.clone();
    request.subject.activity = Some(boundary.binding());
    request.uses = Uses::Budget(2);
    let (handle, view) = authority().issue(request).unwrap();
    let handle = handle.into_wire();
    let decision = crate::clawd::authority::Decision::for_test(
        view.clone(),
        "test.activity",
        Audience::SystemService,
        presentation(Audience::SystemService),
        None,
        &crate::clawd::authority::Requirement::RouteDerived,
    );
    assert!(decision
        .require(read_cap())
        .unwrap_err()
        .contains("confirmation"));
    assert_eq!(
        authority()
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap()
            .uses_remaining,
        Some(2)
    );
    authority().revoke(view.id);

    let mut request = issuance("confirmed-policy", &[Audience::SystemService]);
    request.caps = grant_caps.clone();
    request.subject.activity = Some(boundary.binding_for_authorized(&grant_caps).unwrap());
    request.uses = Uses::Budget(2);
    let (handle, view) = authority().issue(request).unwrap();
    let handle = handle.into_wire();
    let decision = crate::clawd::authority::Decision::for_test(
        view.clone(),
        "test.activity",
        Audience::SystemService,
        presentation(Audience::SystemService),
        None,
        &crate::clawd::authority::Requirement::RouteDerived,
    );
    let _authorized = decision.require(read_cap()).unwrap();
    crate::activities::open_default()
        .unwrap()
        .set_capability_policy(
            current_uid(),
            &activity,
            Some(1),
            serde_json::from_value(json!({"rules":[
                {"verb":"sys.observe","mode":"deny","scopes":[]}
            ]}))
            .unwrap(),
        )
        .unwrap();
    assert!(decision
        .require(read_cap())
        .unwrap_err()
        .contains("changed"));
    assert_eq!(
        authority()
            .resolve(&handle, &presentation(Audience::SystemService))
            .unwrap()
            .uses_remaining,
        Some(1)
    );
    authority().revoke(view.id);
}
