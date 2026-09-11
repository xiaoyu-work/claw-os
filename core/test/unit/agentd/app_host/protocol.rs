use super::*;
use serde_json::json;

fn registration() -> Request {
    Request::build(
        Command::AppSessionRegister,
        json!({
            "app_id":"demo","kind":"operation","operation":"read",
            "args":["/home/user/document"],"parent_caps":[]
        }),
    )
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn invocation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn hosted_registration_requires_a_retained_one_shot_invocation() {
    let request = registration();
    assert!(AppHostCall::from_request(&request, None).is_err());
    assert!(AppHostCall::from_request(&request, Some("invalid context".into())).is_err());
    let call = AppHostCall::from_request(&request, Some(invocation_id())).unwrap();
    assert_eq!(call.command(), Some(Command::AppSessionRegister));
    assert_eq!(
        call.params().unwrap()["args"],
        json!(["/home/user/document"])
    );
    for kind in ["gui", "mcp", "native"] {
        let mut request = request.clone();
        request.params["kind"] = json!(kind);
        assert!(AppHostCall::from_request(&request, Some(invocation_id())).is_err());
    }
}

#[test]
fn the_host_control_surface_is_closed_to_general_broker_commands() {
    for command in Command::ALL {
        if matches!(
            command,
            Command::AppSessionRegister
                | Command::AppSessionBind
                | Command::AppSessionRelay
                | Command::AppSessionDeregister
                | Command::PermissionStatus
        ) {
            continue;
        }
        let request = Request::build(*command, json!({}));
        assert!(
            AppHostCall::from_request(&request, None).is_err(),
            "{command:?}"
        );
    }
    for forbidden in [
        "app_session_set_transient",
        "register_native",
        "permission_decide",
        "system",
    ] {
        assert!(serde_json::from_value::<AppHostCall>(json!({
            "method":forbidden,"params":{}
        }))
        .is_err());
    }
}

#[test]
fn host_control_cannot_carry_owner_authority_or_unknown_fields() {
    let call = AppHostCall::from_request(&registration(), Some(invocation_id())).unwrap();
    let value = serde_json::to_value(&call).unwrap();
    for field in ["owner_uid", "task_id", "session_id", "role", "caps"] {
        let mut forged = value.clone();
        forged["params"][field] = json!("forged");
        assert!(
            serde_json::from_value::<AppHostCall>(forged).is_err(),
            "{field}"
        );
    }
    let mut forged = value;
    forged["params"]["request"]["owner_uid"] = json!(0);
    assert!(serde_json::from_value::<AppHostCall>(forged).is_err());
    let frame = AppHostRequest {
        task_id: "task-a".into(),
        correlation_id: 1,
        request_id: RequestId::generate(),
        call,
    };
    let mut value = serde_json::to_value(frame).unwrap();
    value["owner_uid"] = json!(0);
    assert!(serde_json::from_value::<AppHostRequest>(value).is_err());
}

#[test]
fn liveness_checks_name_only_the_hosted_session_and_package() {
    let check: AppHostCall = serde_json::from_value(json!({
        "method":"check","params":{"session_id":"app-example","package_digest":digest()}
    }))
    .unwrap();
    assert_eq!(check.command(), None);
    assert_eq!(check.session_id(), Some("app-example"));
    check.validate().unwrap();
    assert!(serde_json::from_value::<AppHostCall>(json!({
        "method":"check","params":{"session_id":"app-example","package_digest":"not-a-digest"}
    }))
    .is_err());
}

#[test]
fn original_invocations_are_bounded_data_without_authority_selectors() {
    let original = crate::operations::invocation::AppInvocation {
        app_id: "demo".into(),
        operation: "read".into(),
        args: vec!["relative.txt".into()],
        package_digest: digest(),
    };
    let call = AppHostCall::Begin(original);
    call.validate().unwrap();
    let value = serde_json::to_value(call).unwrap();
    for field in ["owner_uid", "session_id", "caps", "invocation_id"] {
        let mut forged = value.clone();
        forged["params"][field] = json!(0);
        assert!(serde_json::from_value::<AppHostCall>(forged).is_err());
    }
    let mut oversized = value;
    oversized["params"]["args"] = json!(["x".repeat(4097)]);
    let call: AppHostCall = serde_json::from_value(oversized).unwrap();
    assert!(call.validate().is_err());
}
