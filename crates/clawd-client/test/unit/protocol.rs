use super::*;
use crate::{ErrorCode, RemoteError};
use serde_json::json;

#[test]
fn activity_commands_have_stable_names_and_round_trip() {
    for (command, name) in [
        (Command::ActivityCreate, "activity.create"),
        (Command::ActivityList, "activity.list"),
        (Command::ActivityGet, "activity.get"),
        (Command::ActivityUpdate, "activity.update"),
        (Command::ActivityTransition, "activity.transition"),
        (Command::ActivityRun, "activity.run"),
        (Command::ActivityObjects, "activity.objects"),
        (Command::ActivityObjectAttach, "activity.object.attach"),
        (Command::ActivityOperationPreview, "activity.operation.preview"),
        (Command::ActivityReceipts, "activity.receipts"),
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
