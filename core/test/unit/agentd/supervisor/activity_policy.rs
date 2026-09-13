use super::*;
use crate::activities::{ActivityService, CapabilityBoundaryDecision as Boundary};
use crate::caps::activity_boundary::{scope as policy_scope, ActivityBoundary};
use serde_json::json;

fn boundary(lease: &Lease, mode: &str) -> (String, Arc<ActivityBoundary>) {
    let service = crate::activities::open_default().unwrap();
    let activity = service
        .create(
            lease.owner_uid,
            serde_json::from_value(json!({
                "title":"Bounded task","goal":"Prepare, do not send"
            }))
            .unwrap(),
        )
        .unwrap();
    let scopes = if mode == "deny" {
        json!([])
    } else {
        json!([{"kind":"path","value":"/home/user/**"}])
    };
    service
        .set_capability_policy(
            lease.owner_uid,
            &activity.id,
            None,
            serde_json::from_value(
                json!({"rules":[{"verb":"fs.read","mode":mode,"scopes":scopes}]}),
            )
            .unwrap(),
        )
        .unwrap();
    let job: Job = serde_json::from_value(json!({
        "id":lease.task_id,"prompt":"test","status":"running","created_at":"2026-01-01T00:00:00Z",
        "owner_uid":lease.owner_uid,"session_id":lease.session_id,"activity_id":activity.id,
    }))
    .unwrap();
    (
        activity.id,
        ActivityBoundary::for_job(&job).unwrap().unwrap(),
    )
}

#[tokio::test]
async fn denied_boundaries_refuse_direct_consent_requests_without_spending_existing_approvals() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    approve_once(
        lease.session_id.as_deref().unwrap(),
        lease.owner_uid,
        crate::caps::Verb::FS_READ,
        &resource,
    );
    let (_, boundary) = boundary(&lease, "deny");
    policy_scope(Some(boundary), async {
        let mut used = 0;
        let ask = ApprovalAsk::Boundary {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        assert_eq!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Boundary {
                decision: Boundary::Deny
            }
        );
        assert!(matches!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        let ask = ApprovalAsk::Request {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        assert!(matches!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Refused { .. }
        ));
        assert!(crate::approvals::list_pending_for_owner(Some(lease.owner_uid)).is_empty());
        assert!(crate::approvals::has_approved_grant_for_owner(
            lease.session_id.as_deref().unwrap(),
            &crate::caps::Cap::new(crate::caps::Verb::FS_READ, resource.clone()),
            Some(lease.owner_uid),
        )
        .unwrap());
    })
    .await;
}

#[tokio::test]
async fn activity_consent_is_exact_single_use_and_cannot_select_another_lease() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    approve_once(
        lease.session_id.as_deref().unwrap(),
        lease.owner_uid,
        crate::caps::Verb::FS_READ,
        &crate::caps::Scope::path("/home/user/**"),
    );
    let (_, boundary) = boundary(&lease, "require_approval");
    policy_scope(Some(boundary), async {
        let mut used = 0;
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Pending { request_id: None }
        );
        let ask = ApprovalAsk::Request {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        let ApprovalReply::Pending {
            request_id: Some(id),
        } = mediate_approval(&mut used, &lease, &ask)
        else {
            panic!("an exact confirmation must be requested");
        };
        let pending = crate::approvals::lookup_pending(&id).unwrap();
        assert_eq!(pending.scope, resource);
        crate::approvals::approve_for_owner(
            &id,
            crate::approvals::GrantDuration::Session,
            Some("test".into()),
            None,
            Some(lease.owner_uid),
        )
        .unwrap();
        for owner in [0, lease.owner_uid + 1] {
            let mut foreign = new_lease();
            foreign.owner_uid = owner;
            assert!(matches!(
                mediate_approval(&mut used, &foreign, &consume_ask(&resource)),
                ApprovalReply::Refused { .. }
            ));
        }
        let mut foreign = new_lease();
        foreign.session_id = Some("another-session".into());
        assert!(matches!(
            mediate_approval(&mut used, &foreign, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Granted
        );
        assert_eq!(
            mediate_approval(&mut used, &lease, &consume_ask(&resource)),
            ApprovalReply::Pending { request_id: None }
        );
    })
    .await;
}

#[tokio::test]
async fn changed_policy_refuses_even_a_direct_consumption_message_and_has_a_separate_bound() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let resource = crate::caps::Scope::path("/home/user/notes.txt");
    let (id, boundary) = boundary(&lease, "normal");
    policy_scope(Some(boundary), async {
        let ask = ApprovalAsk::Boundary {
            verb: "fs.read".into(),
            scope: resource.clone(),
        };
        let mut used = protocol::MAX_BOUNDARY_CHECKS - 1;
        assert_eq!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Boundary {
                decision: Boundary::Normal
            }
        );
        assert!(matches!(
            mediate_approval(&mut used, &lease, &ask),
            ApprovalReply::Refused { .. }
        ));
        crate::activities::open_default()
            .unwrap()
            .set_capability_policy_enabled(lease.owner_uid, &id, 1, false)
            .unwrap();
        assert!(matches!(
            mediate_approval(&mut 0, &lease, &consume_ask(&resource)),
            ApprovalReply::Refused { .. }
        ));
        assert!(crate::approvals::list_pending_for_owner(Some(lease.owner_uid)).is_empty());
    })
    .await;
}
