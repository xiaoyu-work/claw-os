use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize)]
struct TestContext<'a> {
    mode: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
}

impl Context for TestContext<'_> {
    const APP_ID: &'static str = "test-app";
}

#[test]
fn context_round_trips_and_matches_wire_shape() {
    let serialized = serialize_context(&TestContext {
        mode: "inspect",
        path: Some("C:\\quoted \"name\"\nfile"),
    })
    .unwrap();
    assert_eq!(
        serialized,
        r#"{"app":"test-app","mode":"inspect","path":"C:\\quoted \"name\"\nfile"}"#
    );

    let decoded: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert_eq!(decoded["app"], "test-app");
    assert_eq!(decoded["mode"], "inspect");
    assert_eq!(decoded["path"], "C:\\quoted \"name\"\nfile");
}

#[test]
fn context_at_limit_is_accepted_and_larger_context_is_rejected() {
    let baseline = serialize_context(&TestContext {
        mode: "",
        path: None,
    })
    .unwrap()
    .len();
    let at_limit = "x".repeat(MAX_CONTEXT_BYTES - baseline);
    assert_eq!(
        serialize_context(&TestContext {
            mode: &at_limit,
            path: None,
        })
        .unwrap()
        .len(),
        MAX_CONTEXT_BYTES
    );

    let over_limit = format!("{at_limit}x");
    assert!(matches!(
        serialize_context(&TestContext {
            mode: &over_limit,
            path: None,
        }),
        Err(ContextError::TooLarge {
            actual,
            limit: MAX_CONTEXT_BYTES
        }) if actual == MAX_CONTEXT_BYTES + 1
    ));
}

#[test]
fn launcher_selects_executable_and_builds_activation_arguments() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var(AGENT_UI_ENV, "/opt/claw/custom-agent-ui");
    let activation = Activation::overlay_with_context(
        serialize_context(&TestContext {
            mode: "inspect",
            path: None,
        })
        .unwrap(),
    );
    let argv = launch_argv(&activation);
    std::env::remove_var(AGENT_UI_ENV);

    assert_eq!(argv[0], "/opt/claw/custom-agent-ui");
    assert_eq!(argv[1], "--overlay");
    assert_eq!(argv[2], "--context");
    assert_eq!(argv[3], r#"{"app":"test-app","mode":"inspect"}"#);
}

#[test]
fn launcher_uses_path_lookup_by_default() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var(AGENT_UI_ENV);
    assert_eq!(agent_ui_executable(), DEFAULT_AGENT_UI);
}

#[test]
fn ui_argument_parser_and_activation_json_round_trip() {
    let parsed = parse_ui_arguments([
        "--overlay",
        "--voice",
        "--query",
        "explain this",
        "--context",
        r#"{"app":"test-app"}"#,
        "--future",
    ]);
    assert_eq!(parsed.unknown, ["--future"]);

    let activation = parsed.activation().unwrap();
    let encoded = activation.to_string();
    assert_eq!(Activation::from_str(&encoded).unwrap(), activation);
}

#[test]
fn reserved_app_field_is_rejected() {
    #[derive(Serialize)]
    struct InvalidContext {
        app: &'static str,
    }

    impl Context for InvalidContext {
        const APP_ID: &'static str = "test-app";
    }

    assert!(matches!(
        serialize_context(&InvalidContext { app: "spoofed" }),
        Err(ContextError::ReservedAppField { app: "test-app" })
    ));
}

#[test]
fn process_spawn_failures_are_preserved() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var(
        "CLAW_COS_BIN",
        "/nonexistent/definitely-not-a-real-cos-binary-issue-47",
    );
    let error = launch(&TestContext {
        mode: "inspect",
        path: None,
    })
    .unwrap_err();
    std::env::remove_var("CLAW_COS_BIN");

    match error {
        LaunchError::Process(BridgeError::BinaryNotFound(_)) => {}
        other => panic!("expected binary-not-found launch error, got {other:?}"),
    }
}
