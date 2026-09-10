use super::*;
use crate::test_support::{capability_review, install_review, pending};
use clawd_client::system_review::{PermissionChoice, ReviewAction, ScopeKind};

#[test]
fn queue_business_types_remain_independent_of_transport() {
    assert_eq!(
        serde_json::to_value(PermissionChoice::AllowSession).unwrap(),
        json!("allow_session")
    );
    assert_eq!(
        serde_json::to_value(ScopeKind::LateBound).unwrap(),
        json!("late_bound")
    );
}

#[test]
fn pending_and_show_use_the_same_closed_shared_dto() {
    let review = capability_review("synthetic");
    assert_eq!(
        parse_review(serde_json::to_value(&review).unwrap()).unwrap(),
        review
    );
    assert_eq!(
        parse_pending(serde_json::to_value(pending(review.clone())).unwrap())
            .unwrap()
            .reviews,
        [review]
    );
    assert!(parse_pending(json!({"requests": []})).is_err());
    assert!(parse_review(json!({"id":"synthetic","approved":true})).is_err());
}

#[test]
fn helper_path_and_protocol_are_fixed_and_never_contain_capability_args() {
    let command = helper_command();
    let command = command.as_std();
    assert_eq!(command.get_program(), "/usr/bin/pkexec");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "/usr/local/bin/claw-approval-helper",
            "--system-review-json",
        ]
    );
}

#[test]
fn only_cancel_uses_the_owner_route_with_the_displayed_decision() {
    let mut decision = ReviewDecision {
        id: "rv-0123456789abcdef0123456789abcdef".into(),
        revision: 42,
        action: ReviewAction::Cancel,
        choices: vec![],
    };
    let (command, params) = owner_cancel_request(&decision).unwrap();
    assert_eq!(command, Command::SystemReviewCancel);
    assert!(command.requires_root_peer());
    assert_eq!(
        params,
        json!({
            "review": {
                "id": decision.id, "revision": 42, "action": "cancel", "choices": [],
            },
        })
    );
    assert_eq!(
        ReviewDecision::decode(&serde_json::to_vec(&params["review"]).unwrap()).unwrap(),
        decision
    );
    for action in [
        ReviewAction::ConfirmInstall,
        ReviewAction::ConfirmActivation,
        ReviewAction::ConfirmUpdate,
        ReviewAction::ApplyChoices,
    ] {
        decision.action = action;
        assert!(owner_cancel_request(&decision).is_none());
    }
}

#[tokio::test]
async fn helper_reads_explicit_stdin_and_returns_only_validated_review_data() {
    let review = install_review("synthetic");
    let decision = ReviewDecision {
        id: review.id.clone(),
        revision: review.revision,
        action: ReviewAction::ConfirmInstall,
        choices: vec![],
    };
    let input = decision.encode_for(&review).unwrap();
    let mut command = tokio::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "test -n \"$(cat)\" || exit 9; printf '%s' \"$1\"",
            "fixture",
        ])
        .arg(serde_json::to_string(&review).unwrap());
    let returned = run_helper(command, &input, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(returned, review);
}

#[tokio::test]
async fn malformed_empty_or_nonzero_helper_replies_never_become_success() {
    for script in [
        "printf '{}'",
        "printf ''",
        "printf '{\"approved\":true}'",
        "printf 'authentication cancelled' >&2; exit 1",
    ] {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", &format!("cat >/dev/null; {script}")]);
        assert!(
            run_helper(command, b"{}", Duration::from_secs(2))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn helper_output_and_duration_are_bounded() {
    assert_eq!(read_bounded(&b"1234"[..], 4).await.unwrap(), b"1234");
    assert!(read_bounded(&b"12345"[..], 4).await.is_err());
    let mut command = tokio::process::Command::new("/bin/sh");
    command.args(["-c", "cat >/dev/null; exec sleep 10"]);
    let result = run_helper(command, b"{}", Duration::from_millis(30))
        .await
        .unwrap_err();
    assert!(result.0.contains("timed out"));
}
