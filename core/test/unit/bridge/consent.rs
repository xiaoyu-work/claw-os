use super::*;
use clawd_client::system_review::{ReviewAction, ReviewSubject};
use std::cell::RefCell;
use std::collections::VecDeque;

const APP: &str = "independent";
const REVIEW: &str = "rv-0123456789abcdef0123456789abcdef";
const CAPABILITY: &str = "ap-0123456789ab";

fn review_denial() -> ClawdCallError {
    ClawdCallError {
        code: Some("not_authorized".into()),
        message: "App activation is waiting for the owner's OS permission review".into(),
        data: Some(json!({
            "status": "system_review_required",
            "system_review_id": REVIEW,
            "app_id": APP,
        })),
    }
}

fn capability_denial() -> ClawdCallError {
    ClawdCallError {
        code: Some("not_authorized".into()),
        message: "capability approval is required".into(),
        data: Some(json!({
            "status": "approval_required",
            "approval_requests": [CAPABILITY],
        })),
    }
}

fn review(status: ReviewStatus, revision: u64) -> SystemReview {
    SystemReview {
        schema_version: clawd_client::system_review::SCHEMA_VERSION,
        id: REVIEW.into(),
        revision,
        kind: ReviewKind::Activation,
        status,
        status_message: None,
        subject: ReviewSubject {
            app_id: Some(APP.into()),
            name: "Independent App".into(),
            version: Some("1.0.0".into()),
            publisher: None,
        },
        initiator: "uid:1000 pid:1234".into(),
        context: Vec::new(),
        permissions: Vec::new(),
        disclosures: Vec::new(),
        changes: None,
        contract_digest: Some("comparison-only-not-authority".into()),
        actions: if status == ReviewStatus::Pending {
            vec![ReviewAction::Cancel, ReviewAction::ConfirmActivation]
        } else {
            Vec::new()
        },
    }
}

#[test]
fn system_review_is_not_a_legacy_capability_approval() {
    let denial = review_denial();
    assert_eq!(
        pending_consent(&denial, APP).unwrap(),
        Some(PendingConsent::SystemReview(REVIEW.into()))
    );
    assert!(crate::bridge::approval_requests(&denial).is_empty());
    assert_eq!(
        pending_consent(&capability_denial(), APP).unwrap(),
        Some(PendingConsent::Capabilities(vec![CAPABILITY.into()]))
    );
}

#[test]
fn malformed_or_mismatched_review_requests_never_enter_the_wait() {
    for id in [
        "",
        "rv-x",
        CAPABILITY,
        "../review",
        "rv-0123456789ABCDEF0123456789ABCDEF",
    ] {
        let mut denial = review_denial();
        denial.data.as_mut().unwrap()["system_review_id"] = json!(id);
        assert!(pending_consent(&denial, APP).is_err(), "{id}");
    }
    let mut denial = review_denial();
    denial
        .data
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("system_review_id");
    assert!(pending_consent(&denial, APP).is_err());
    assert!(pending_consent(&review_denial(), "another-app").is_err());
    let mut denial = review_denial();
    denial.code = Some("execution_failed".into());
    assert_eq!(pending_consent(&denial, APP).unwrap(), None);
    for ids in [
        json!([REVIEW]),
        json!([CAPABILITY, CAPABILITY]),
        json!([CAPABILITY, 1]),
        json!([]),
    ] {
        let mut denial = capability_denial();
        denial.data.as_mut().unwrap()["approval_requests"] = ids;
        assert!(pending_consent(&denial, APP).is_err());
    }
}

#[test]
fn refusal_code_and_review_data_survive_the_launch_error_boundary() {
    let denial = review_denial();
    let expected = denial.data.clone().unwrap();
    let text = String::from(denial.with_context("the review controller is unavailable"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["code"], "not_authorized");
    assert_eq!(value["details"], expected);
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("controller is unavailable"));
}

#[test]
fn review_then_capability_decisions_only_retry_the_original_registration() {
    let _lock = crate::test_env::lock_env();
    let params = json!({
        "app_id": APP,
        "kind": "operation",
        "operation": "read",
        "args": ["selected"],
        "package": {"content_digest": "original-signed-snapshot"},
    });
    let responses = RefCell::new(VecDeque::from([
        Err(review_denial()),
        Err(capability_denial()),
        Ok(json!({"session_id": "broker-issued"})),
    ]));
    let sent = RefCell::new(Vec::new());
    let waits = RefCell::new(Vec::new());
    let result = register_with(
        APP,
        params.clone(),
        true,
        Instant::now() + Duration::from_secs(1),
        |command, body| {
            sent.borrow_mut().push((command, body));
            responses.borrow_mut().pop_front().unwrap()
        },
        |pending, _| {
            waits.borrow_mut().push(pending.clone());
            Ok(())
        },
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(result, json!({"session_id": "broker-issued"}));
    assert_eq!(sent.borrow().len(), 3);
    for (command, body) in sent.borrow().iter() {
        assert_eq!(*command, ClawdCommand::AppSessionRegister);
        assert_eq!(
            *body, params,
            "a review id or claimed confirmation entered registration"
        );
    }
    assert_eq!(
        *waits.borrow(),
        [
            PendingConsent::SystemReview(REVIEW.into()),
            PendingConsent::Capabilities(vec![CAPABILITY.into()]),
        ]
    );
}

#[test]
fn a_confirmed_status_cannot_override_an_authoritative_registration_refusal() {
    let _lock = crate::test_env::lock_env();
    let mut sent = 0;
    let mut waited = 0;
    let error = register_with(
        APP,
        json!({}),
        true,
        Instant::now() + Duration::from_secs(1),
        |_, _| {
            sent += 1;
            Err(review_denial())
        },
        |_, _| {
            waited += 1;
            assert!(review_complete(
                &review(ReviewStatus::Confirmed, 2),
                APP,
                REVIEW
            )?);
            Ok(())
        },
    )
    .expect_err("a status response cannot mint an App session");
    assert_eq!((sent, waited), (2, 1));
    assert!(error.message.contains("broker still refuses"));
    assert_eq!(error.data, review_denial().data);
}

#[test]
fn private_hosts_preserve_review_refusals_without_bypassing_their_proxy() {
    let error = register_with(
        APP,
        json!({}),
        false,
        Instant::now() + Duration::from_secs(1),
        |command, _| {
            assert_eq!(command, ClawdCommand::AppSessionRegister);
            Err(review_denial())
        },
        |_, _| panic!("a private Host reached around its route-filtered broker"),
    )
    .expect_err("the controller must receive the review refusal");
    assert_eq!(error.code, review_denial().code);
    assert_eq!(error.message, review_denial().message);
    assert_eq!(error.data, review_denial().data);
}

#[test]
fn failed_waits_preserve_the_refusal_and_do_not_retry_or_register_locally() {
    let _lock = crate::test_env::lock_env();
    let mut sent = 0;
    let error = register_with(
        APP,
        json!({}),
        true,
        Instant::now() + Duration::from_secs(1),
        |_, _| {
            sent += 1;
            Err(review_denial())
        },
        |_, _| Err("root broker disconnected".into()),
    )
    .expect_err("missing review status must refuse the launch");
    assert_eq!(sent, 1);
    assert!(error.message.contains("root broker disconnected"));
    assert_eq!(error.data, review_denial().data);
}

#[test]
fn only_matching_activation_statuses_allow_a_retry() {
    for status in [ReviewStatus::Confirmed, ReviewStatus::Consumed] {
        assert!(review_complete(&review(status, 1), APP, REVIEW).unwrap());
    }
    assert!(!review_complete(&review(ReviewStatus::Pending, 1), APP, REVIEW).unwrap());
    for status in [
        ReviewStatus::Completed,
        ReviewStatus::Cancelled,
        ReviewStatus::Stale,
        ReviewStatus::Expired,
        ReviewStatus::Failed,
    ] {
        assert!(review_complete(&review(status, 1), APP, REVIEW).is_err());
    }
    assert!(review_complete(&review(ReviewStatus::Confirmed, 0), APP, REVIEW).is_err());
    assert!(review_complete(&review(ReviewStatus::Confirmed, 1), "another", REVIEW).is_err());
    assert!(review_complete(&review(ReviewStatus::Confirmed, 1), APP, CAPABILITY).is_err());
    let mut wrong_kind = review(ReviewStatus::Confirmed, 1);
    wrong_kind.kind = ReviewKind::Capability;
    assert!(review_complete(&wrong_kind, APP, REVIEW).is_err());
    let mut wrong_schema = review(ReviewStatus::Confirmed, 1);
    wrong_schema.schema_version += 1;
    assert!(review_complete(&wrong_schema, APP, REVIEW).is_err());
}

#[test]
fn polling_uses_current_review_status_and_presents_each_revision_once() {
    let _lock = crate::test_env::lock_env();
    let mut responses = VecDeque::from([
        review(ReviewStatus::Pending, 1),
        review(ReviewStatus::Pending, 1),
        review(ReviewStatus::Confirmed, 2),
    ]);
    let mut shown = Vec::new();
    poll_review(
        APP,
        REVIEW,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        || Ok(responses.pop_front().unwrap()),
        |review| {
            shown.push(review.revision);
            Ok(())
        },
    )
    .unwrap();
    assert!(responses.is_empty());
    assert_eq!(shown, [1, 2]);
}

#[test]
fn stale_or_inconsistent_revisions_do_not_end_a_review_wait_successfully() {
    let _lock = crate::test_env::lock_env();
    for revision in [1, 2] {
        let mut responses = VecDeque::from([
            review(ReviewStatus::Pending, 2),
            review(ReviewStatus::Confirmed, revision),
        ]);
        let error = poll_review(
            APP,
            REVIEW,
            Instant::now() + Duration::from_secs(1),
            Duration::ZERO,
            || Ok(responses.pop_front().unwrap()),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("newer presentation revision"), "{error}");
    }
}

#[test]
fn review_waits_honor_cancellation_deadlines_and_controller_failure() {
    let _lock = crate::test_env::lock_env();
    crate::bridge::cancel_pending_approval_wait();
    let error = poll_review(
        APP,
        REVIEW,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        || panic!("cancelled wait contacted the controller"),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("cancelled"));
    let error = poll_review(
        APP,
        REVIEW,
        Instant::now(),
        Duration::ZERO,
        || panic!("expired wait contacted the controller"),
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("timed out"));
    let error = poll_review(
        APP,
        REVIEW,
        Instant::now() + Duration::from_secs(1),
        Duration::ZERO,
        || Err("review no longer exists".into()),
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error, "review no longer exists");
}

#[test]
fn capability_statuses_must_cover_the_exact_requested_set() {
    let ids = vec![CAPABILITY.to_string(), "ap-second".to_string()];
    let complete = |statuses| crate::bridge::approval_statuses_complete(&ids, &statuses);
    assert!(complete(json!({"statuses": [
        {"id": "ap-second", "status": "approved"},
        {"id": CAPABILITY, "status": "approved"},
    ]}))
    .unwrap());
    for status in ["pending", "resolving"] {
        assert!(!complete(json!({"statuses": [
            {"id": CAPABILITY, "status": status},
            {"id": "ap-second", "status": "approved"},
        ]}))
        .unwrap());
    }
    for response in [
        json!({}),
        json!({"statuses": []}),
        json!({"statuses": [{"id": CAPABILITY, "status": "approved"}]}),
        json!({"statuses": [
            {"id": CAPABILITY, "status": "approved"},
            {"id": CAPABILITY, "status": "approved"},
        ]}),
        json!({"statuses": [
            {"id": CAPABILITY, "status": "approved"},
            {"id": "ap-unrequested", "status": "approved"},
        ]}),
        json!({"statuses": [
            {"id": CAPABILITY, "status": "approved"},
            {"status": "approved"},
        ]}),
    ] {
        assert!(complete(response).is_err());
    }
    for status in ["denied", "unknown", "expired", ""] {
        assert!(complete(json!({"statuses": [
            {"id": CAPABILITY, "status": status},
            {"id": "ap-second", "status": "approved"},
        ]}))
        .is_err());
    }
    assert!(crate::bridge::approval_statuses_complete(&[], &json!({"statuses": []})).is_err());
    assert!(crate::bridge::approval_statuses_complete(
        &[CAPABILITY.to_string(), CAPABILITY.to_string()],
        &json!({"statuses": [
            {"id": CAPABILITY, "status": "approved"},
            {"id": CAPABILITY, "status": "approved"},
        ]})
    )
    .is_err());
}
