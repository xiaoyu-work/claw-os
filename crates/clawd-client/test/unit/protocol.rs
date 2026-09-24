use super::*;
use crate::{ErrorCode, RemoteError};
use serde_json::json;

#[test]
fn activity_commands_have_stable_names_and_round_trip() {
    for (command, name) in [
        (Command::ActivityCreate, "activity.create"),
        (Command::ActivityList, "activity.list"),
        (Command::ActivityGet, "activity.get"),
        (Command::ActivityAttention, "activity.attention"),
        (Command::ActivityUpdate, "activity.update"),
        (Command::ActivityTransition, "activity.transition"),
        (Command::ActivityRun, "activity.run"),
        (
            Command::ActivityContinuityExport,
            "activity.continuity.export",
        ),
        (
            Command::ActivityContinuityImport,
            "activity.continuity.import",
        ),
        (
            Command::ActivityExecutionLimitsGet,
            "activity.execution_limits.get",
        ),
        (
            Command::ActivityExecutionLimitsSet,
            "activity.execution_limits.set",
        ),
        (
            Command::ActivityExecutionLimitsEnabled,
            "activity.execution_limits.enabled",
        ),
        (
            Command::ActivityMonetaryBudgetGet,
            "activity.monetary_budget.get",
        ),
        (
            Command::ActivityMonetaryBudgetSet,
            "activity.monetary_budget.set",
        ),
        (
            Command::ActivityMonetaryBudgetEnabled,
            "activity.monetary_budget.enabled",
        ),
        (
            Command::ActivitySchedulingPolicyGet,
            "activity.scheduling_policy.get",
        ),
        (
            Command::ActivitySchedulingPolicySet,
            "activity.scheduling_policy.set",
        ),
        (
            Command::ActivityCapabilityPolicyGet,
            "activity.capability_policy.get",
        ),
        (
            Command::ActivityCapabilityPolicySet,
            "activity.capability_policy.set",
        ),
        (
            Command::ActivityCapabilityPolicyEnabled,
            "activity.capability_policy.enabled",
        ),
        (Command::ActivityObjects, "activity.objects"),
        (Command::ActivityObjectAttach, "activity.object.attach"),
        (
            Command::ActivityObjectStateList,
            "activity.object_state.list",
        ),
        (
            Command::ActivityObjectStateRecord,
            "activity.object_state.record",
        ),
        (
            Command::ActivityOperationPreview,
            "activity.operation.preview",
        ),
        (Command::ActivityReceipts, "activity.receipts"),
        (
            Command::AgentConversationCreate,
            "agent.conversation.create",
        ),
        (Command::AgentConversationGet, "agent.conversation.get"),
        (Command::AgentConversationList, "agent.conversation.list"),
        (
            Command::AgentConversationUpdate,
            "agent.conversation.update",
        ),
        (Command::AgentConversationFork, "agent.conversation.fork"),
        (
            Command::AgentConversationRevert,
            "agent.conversation.revert",
        ),
        (Command::TaskGet, "task.get"),
        (Command::TaskRetry, "task.retry"),
    ] {
        assert_eq!(command.as_str(), name);
        assert!(Command::ALL.contains(&command));
        assert_eq!(serde_json::to_value(command).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Command>(json!(name)).unwrap(),
            command
        );
    }
}

#[test]
fn capability_policy_commands_have_stable_names_and_unique_inventory_entries() {
    for (command, name) in [
        (
            Command::ActivityCapabilityPolicyGet,
            "activity.capability_policy.get",
        ),
        (
            Command::ActivityCapabilityPolicySet,
            "activity.capability_policy.set",
        ),
        (
            Command::ActivityCapabilityPolicyEnabled,
            "activity.capability_policy.enabled",
        ),
    ] {
        assert_eq!(command.as_str(), name);
        assert_eq!(command.to_string(), name);
        assert_eq!(
            Command::ALL
                .iter()
                .filter(|entry| **entry == command)
                .count(),
            1
        );
        assert_eq!(serde_json::to_value(command).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Command>(json!(name)).unwrap(),
            command
        );
    }

    assert!(serde_json::from_value::<Command>(json!("activity.capability_policy.grant")).is_err());
    assert!(
        serde_json::from_value::<Command>(json!("activity.capability_policy.approve")).is_err()
    );
}

#[test]
fn scheduling_policy_commands_are_closed_unique_and_never_root_exempt() {
    for (command, name) in [
        (
            Command::ActivitySchedulingPolicyGet,
            "activity.scheduling_policy.get",
        ),
        (
            Command::ActivitySchedulingPolicySet,
            "activity.scheduling_policy.set",
        ),
    ] {
        assert_eq!(command.as_str(), name);
        assert_eq!(command.to_string(), name);
        assert_eq!(
            Command::ALL
                .iter()
                .filter(|entry| **entry == command)
                .count(),
            1
        );
        assert!(!command.requires_root_peer());
        assert_eq!(serde_json::to_value(command).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Command>(json!(name)).unwrap(),
            command
        );
    }
    for invalid in [
        "activity.scheduling_policy.grant",
        "activity.scheduling_policy.preempt",
        "activity.scheduling_priority.set",
    ] {
        assert!(serde_json::from_value::<Command>(json!(invalid)).is_err());
    }
}

#[test]
fn continuity_commands_are_closed_unique_and_never_root_exempt() {
    for (command, name) in [
        (
            Command::ActivityContinuityExport,
            "activity.continuity.export",
        ),
        (
            Command::ActivityContinuityImport,
            "activity.continuity.import",
        ),
    ] {
        assert_eq!(command.as_str(), name);
        assert_eq!(command.to_string(), name);
        assert_eq!(
            Command::ALL
                .iter()
                .filter(|entry| **entry == command)
                .count(),
            1
        );
        assert!(!command.requires_root_peer());
        assert_eq!(serde_json::to_value(command).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<Command>(json!(name)).unwrap(),
            command
        );
    }
    for invalid in [
        "activity.continuity.restore",
        "activity.continuity.sync",
        "activity.continuity.import_authority",
    ] {
        assert!(serde_json::from_value::<Command>(json!(invalid)).is_err());
    }
}

#[test]
fn main_routes_survive_activity_inventory_merge() {
    let main_routes = [
        (
            Command::AgentConversationCreate,
            "agent.conversation.create",
        ),
        (Command::AgentConversationGet, "agent.conversation.get"),
        (Command::AgentConversationList, "agent.conversation.list"),
        (
            Command::AgentConversationUpdate,
            "agent.conversation.update",
        ),
        (Command::AgentConversationFork, "agent.conversation.fork"),
        (
            Command::AgentConversationRevert,
            "agent.conversation.revert",
        ),
        (Command::TaskSubmit, "task.submit"),
        (Command::TaskStream, "task.stream"),
        (Command::TaskCancel, "task.cancel"),
        (Command::MemorySessions, "memory.sessions"),
        (Command::MemoryHistory, "memory.history"),
        (Command::MemoryReset, "memory.reset"),
        (Command::PermissionPending, "permission.pending"),
        (Command::SystemReviewPrepare, "system.review.prepare"),
        (Command::SystemReviewPending, "system.review.pending"),
        (Command::SystemReviewShow, "system.review.show"),
        (Command::SystemReviewConsume, "system.review.consume"),
        (Command::SystemReviewCancel, "system.review.cancel"),
        (Command::NotificationSubscribe, "notification.subscribe"),
        (
            Command::NotificationDeliveryClaim,
            "notification.delivery.claim",
        ),
        (
            Command::NotificationDeliveryComplete,
            "notification.delivery.complete",
        ),
        (Command::NotificationAcknowledge, "notification.acknowledge"),
        (Command::NotificationDismiss, "notification.dismiss"),
    ];
    assert_eq!(Command::ALL.len(), 52);
    let mut names = std::collections::HashSet::new();
    for command in Command::ALL {
        assert!(names.insert(command.as_str()), "duplicate {command}");
        assert_eq!(
            serde_json::to_value(command).unwrap(),
            json!(command.as_str())
        );
        assert_eq!(
            serde_json::from_value::<Command>(json!(command.as_str())).unwrap(),
            command,
        );
    }
    for (command, name) in main_routes {
        assert!(Command::ALL.contains(&command), "missing {name}");
        assert_eq!(command.as_str(), name);
    }
}

#[test]
fn system_review_root_peer_requirement_survives_activity_merge() {
    let protected = [
        Command::SystemReviewPrepare,
        Command::SystemReviewPending,
        Command::SystemReviewShow,
        Command::SystemReviewConsume,
        Command::SystemReviewCancel,
    ];
    for command in Command::ALL {
        assert_eq!(
            command.requires_root_peer(),
            protected.contains(&command),
            "{command}",
        );
    }
    assert!(serde_json::from_value::<Command>(json!("system.review.decide")).is_err());
}

#[test]
fn requests_are_closed_typed_envelopes_with_fresh_bounded_ids() {
    let first = Request::new(Command::TaskSubmit, json!({"prompt": "hello"}));
    let second = Request::new(Command::TaskSubmit, json!({"prompt": "hello"}));
    assert_eq!(first.v, PROTOCOL_VERSION);
    assert_ne!(first.id, second.id);
    assert!(first.id.as_str().len() <= MAX_REQUEST_ID_BYTES);
    assert_eq!(
        serde_json::to_value(&first).unwrap()["command"],
        json!("task.submit")
    );
    assert!(serde_json::from_value::<Request>(json!({
        "v": PROTOCOL_VERSION,
        "id": "r1",
        "command": "task.submit",
        "params": {},
        "uid": 0,
    }))
    .is_err());
}

#[test]
fn response_shape_and_request_id_are_enforced() {
    let id = RequestId::parse("desktop-1").unwrap();
    let valid = Response {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        ok: true,
        result: Some(json!({"status": "ok"})),
        error: None,
    };
    assert!(valid.clone().validate(&id).is_ok());

    let inconsistent = Response {
        v: PROTOCOL_VERSION,
        id: id.clone(),
        ok: true,
        result: None,
        error: None,
    };
    assert!(matches!(
        inconsistent.validate(&id),
        Err(ClientError::InvalidResponse(_))
    ));

    let other = RequestId::parse("desktop-2").unwrap();
    assert!(matches!(
        valid.validate(&other),
        Err(ClientError::MismatchedRequestId { .. })
    ));
}

#[test]
fn stable_remote_codes_are_typed_and_unknown_codes_are_preserved() {
    let known: RemoteError = serde_json::from_value(json!({
        "code": "not_authorized",
        "message": "approval required",
        "data": {"approval_requests": ["request-1"]},
    }))
    .unwrap();
    assert_eq!(known.code, ErrorCode::NotAuthorized);

    let future: RemoteError = serde_json::from_value(json!({
        "code": "future_code",
        "message": "new failure",
    }))
    .unwrap();
    assert_eq!(future.code, ErrorCode::Other("future_code".to_string()));
}
