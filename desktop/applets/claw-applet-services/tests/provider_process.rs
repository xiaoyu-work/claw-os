#![cfg(target_os = "linux")]

use claw_os_sdk::applet::{
    CalendarDate, Client, ClientError, HistoryPermission,
    protocol::{ErrorCode, Failure},
};
use std::fs;

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/fixture.rs"
    ));
}
use fixture::Fixture;

#[tokio::test]
async fn real_provider_and_public_sdk_preserve_data_denials_and_process_lifetime() {
    let fixture = Fixture::new();
    fixture.calendar();
    let binary = env!("CARGO_BIN_EXE_claw-os-applet-provider");
    let client = Client::with_binary(fixture.provider(binary)).unwrap();
    let date = CalendarDate {
        year: 2026,
        month: 9,
        day: 9,
    };
    assert!(matches!(
        client.calendar_day(date).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::PermissionDenied,
            ..
        }))
    ));
    assert!(matches!(
        client.require_history(HistoryPermission::Read).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::PermissionDenied,
            ..
        }))
    ));
    assert_eq!(
        fixture.calls(),
        "__policy check data.db.read --name calendar\n__policy check clipboard.read --name history\n"
    );
    fixture.allow();
    let before = fs::read(fixture.root.join("data/calendar/events.db")).unwrap();
    let events = client.calendar_day(date).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, "event-1");
    assert_eq!(events[0].title, "Review");
    assert_eq!(events[0].location, "Office");
    let tasks = client.tasks().await.unwrap();
    assert_eq!(tasks[0].purpose, "Review");
    for _ in 0..2 {
        let summary = client.system().await.unwrap();
        assert_eq!(summary.memory.unwrap().used_mb, 200);
        assert!(!summary.fallback);
    }
    client
        .require_history(HistoryPermission::Write)
        .await
        .unwrap();
    assert_eq!(
        before,
        fs::read(fixture.root.join("data/calendar/events.db")).unwrap()
    );
    fs::remove_file(fixture.root.join("allow")).unwrap();
    assert!(matches!(
        client.system().await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::PermissionDenied,
            ..
        }))
    ));
    drop(client);
}

#[test]
fn real_helper_rejects_other_modes_before_any_policy_call() {
    let fixture = Fixture::new();
    for arguments in [
        vec![],
        vec!["--stdio-v2"],
        vec!["--stdio-v1", "--owner", "10"],
    ] {
        let result = std::process::Command::new(Fixture::binary(env!(
            "CARGO_BIN_EXE_claw-os-applet-provider"
        )))
        .args(arguments)
        .output()
        .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
    }
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn interface_preserves_context_without_identity_or_gui_privileges() {
    let fixture = Fixture::new();
    fixture.calendar();
    for (app_id, gui) in [
        ("example-python-client", None),
        ("example-rust-client", Some("1")),
        ("panel-calendar", None),
    ] {
        unsafe {
            std::env::set_var("COS_APP_ID", app_id);
            match gui {
                Some(value) => std::env::set_var("COS_APP_GUI", value),
                None => std::env::remove_var("COS_APP_GUI"),
            }
        }
        let client =
            Client::with_binary(fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")))
                .unwrap();
        let date = CalendarDate {
            year: 2026,
            month: 9,
            day: 9,
        };
        assert!(matches!(
            client.calendar_day(date).await,
            Err(ClientError::Provider(Failure {
                code: ErrorCode::PermissionDenied,
                ..
            }))
        ));
        fixture.allow();
        assert_eq!(client.calendar_day(date).await.unwrap()[0].id, "event-1");
        fs::remove_file(fixture.root.join("allow")).unwrap();
        assert!(matches!(
            client.calendar_day(date).await,
            Err(ClientError::Provider(Failure {
                code: ErrorCode::PermissionDenied,
                ..
            }))
        ));
        drop(client);
    }
    assert_eq!(
        fixture.calls(),
        "__policy check data.db.read --name calendar\n".repeat(9),
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("contexts")).unwrap(),
        format!(
            "{}{}{}",
            "example-python-client:\n".repeat(3),
            "example-rust-client:1\n".repeat(3),
            "panel-calendar:\n".repeat(3)
        ),
    );
}

#[tokio::test]
async fn installed_provider_ignores_kernel_and_path_overrides() {
    let fixture = Fixture::new();
    fixture.allow();
    let launcher = fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider"));
    unsafe {
        std::env::set_var("COS_BIN", fixture.root.join("not-a-kernel"));
        std::env::set_var("PATH", fixture.root.join("not-a-path"));
    }
    let client = Client::with_binary(launcher).unwrap();
    client
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
    assert_eq!(
        fixture.calls(),
        "__policy check clipboard.read --name history\n"
    );
}

#[tokio::test]
async fn versioned_cases_execute_on_the_actual_provider_without_invalid_request_authority() {
    use claw_os_sdk::applet::protocol::{self, Outcome, Request, Response};
    use std::process::Stdio;
    use tokio::io::{AsyncWriteExt, BufReader};

    let fixture = Fixture::new();
    fixture.allow();
    let mut child = tokio::process::Command::new(
        fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")),
    )
    .arg(protocol::PROVIDER_ARGUMENT)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let cases: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../claw-os-sdk/wire/v1/applet-services.cases.json"
    )))
    .unwrap();
    for case in cases["requests"].as_array().unwrap() {
        let calls = fixture.calls();
        let wire = case["wire"].as_str().unwrap();
        input
            .write_all(format!("{wire}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            protocol::read_frame(&mut output, protocol::MAX_RESPONSE_BYTES),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let response: Response = serde_json::from_slice(&frame).unwrap();
        let decoded = serde_json::from_str::<Request>(wire);
        assert_eq!(
            response.id,
            decoded.as_ref().map_or(0, |request| request.id)
        );
        assert_eq!(response.version, protocol::VERSION);
        match response.outcome {
            Outcome::Ok { data } => {
                assert!(case["error"].is_null(), "{}", case["name"]);
                assert!(decoded.unwrap().operation.accepts(&data));
            }
            Outcome::Error { error } => {
                assert_eq!(
                    serde_json::to_value(error.code).unwrap(),
                    case["error"],
                    "{}",
                    case["name"]
                );
                assert_eq!(
                    fixture.calls(),
                    calls,
                    "invalid requests must not consult policy"
                );
            }
        }
    }
    drop(input);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), child.wait(),)
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn policy_replies_are_exact_bounded_and_rechecked_after_failure() {
    let fixture = Fixture::new();
    fixture.allow();
    let client =
        Client::with_binary(fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")))
            .unwrap();
    for reply in [
        r#"{"decision":"allow"}"#,
        r#"{"decision":"allow","verb":"clipboard.read","scope":{"kind":"name","value":"selection"}}"#,
        r#"{"decision":"allow","verb":"clipboard.write","scope":{"kind":"name","value":"history"}}"#,
        r#"{"decision":"other","verb":"clipboard.read","scope":{"kind":"name","value":"history"}}"#,
    ] {
        fs::write(fixture.root.join("policy-reply"), reply).unwrap();
        assert!(matches!(
            client.require_history(HistoryPermission::Read).await,
            Err(ClientError::Provider(Failure {
                code: ErrorCode::ProviderFailure,
                ..
            })),
        ));
    }
    let mut exact = br#"{"decision":"allow","verb":"clipboard.read","scope":{"kind":"name","value":"history"}}"#.to_vec();
    exact.resize(16 * 1024, b' ');
    fs::write(fixture.root.join("policy-reply"), &exact).unwrap();
    client
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
    exact.push(b' ');
    fs::write(fixture.root.join("policy-reply"), exact).unwrap();
    assert!(matches!(
        client.require_history(HistoryPermission::Read).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::ProviderFailure,
            ..
        })),
    ));
    fs::remove_file(fixture.root.join("policy-reply")).unwrap();
    fs::write(fixture.root.join("stderr-reply"), vec![b'x'; 16 * 1024 + 1]).unwrap();
    assert!(matches!(
        client.require_history(HistoryPermission::Read).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::ProviderFailure,
            ..
        })),
    ));
    fs::remove_file(fixture.root.join("stderr-reply")).unwrap();
    client
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
}

#[tokio::test]
async fn policy_timeout_is_bounded_reaped_and_does_not_poison_the_provider() {
    use std::time::{Duration, Instant};

    let fixture = Fixture::new();
    fixture.allow();
    fs::write(fixture.root.join("hang-policy"), "").unwrap();
    let client =
        Client::with_binary(fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")))
            .unwrap();
    let started = Instant::now();
    assert!(matches!(
        client.require_history(HistoryPermission::Read).await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::ProviderUnavailable,
            ..
        })),
    ));
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(6));
    let pid = fs::read_to_string(fixture.root.join("blocked-policy-pid")).unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
    fs::remove_file(fixture.root.join("hang-policy")).unwrap();
    client
        .require_history(HistoryPermission::Read)
        .await
        .unwrap();
}

#[tokio::test]
async fn malformed_task_data_and_oversized_calendar_records_are_not_empty_successes() {
    let fixture = Fixture::new();
    fixture.allow();
    fixture.calendar();
    let client =
        Client::with_binary(fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")))
            .unwrap();
    for reply in [r#"{}"#, r#"{"n":1,"tasks":[]}"#] {
        fs::write(fixture.root.join("task-reply"), reply).unwrap();
        assert!(matches!(
            client.tasks().await,
            Err(ClientError::Provider(Failure {
                code: ErrorCode::ProviderFailure,
                ..
            })),
        ));
    }
    let database = fixture.root.join("data/calendar/events.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "UPDATE events SET title = ?1 WHERE id = 'event-1'",
            ["x".repeat(1024 * 1024 + 1)],
        )
        .unwrap();
    drop(connection);
    let before = fs::read(&database).unwrap();
    assert!(matches!(
        client
            .calendar_day(CalendarDate {
                year: 2026,
                month: 9,
                day: 9
            })
            .await,
        Err(ClientError::Provider(Failure {
            code: ErrorCode::ProviderFailure,
            ..
        })),
    ));
    assert_eq!(fs::read(&database).unwrap(), before);
}

#[tokio::test]
async fn idle_pipes_survive_but_partial_request_frames_have_one_real_deadline() {
    use claw_os_sdk::applet::protocol::{self, Outcome, Response};
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        time::{sleep, timeout},
    };

    let fixture = Fixture::new();
    let mut child = tokio::process::Command::new(
        fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")),
    )
    .arg(protocol::PROVIDER_ARGUMENT)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    sleep(Duration::from_millis(3100)).await;
    assert!(child.try_wait().unwrap().is_none());
    let started = Instant::now();
    input.write_all(b"{").await.unwrap();
    input.flush().await.unwrap();
    let frame = timeout(
        Duration::from_secs(5),
        protocol::read_frame(&mut output, protocol::MAX_RESPONSE_BYTES),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(3));
    let response: Response = serde_json::from_slice(&frame).unwrap();
    assert_eq!(response.id, 0);
    assert!(matches!(
        response.outcome,
        Outcome::Error {
            error: Failure {
                code: ErrorCode::InvalidRequest,
                ..
            },
        }
    ));
    assert!(
        !timeout(Duration::from_secs(2), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn exact_request_limit_accepts_the_newline_and_the_next_byte_closes_without_policy() {
    use claw_os_sdk::applet::protocol::{self, Outcome, Response};
    use std::{process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncWriteExt, BufReader},
        time::timeout,
    };

    let fixture = Fixture::new();
    fixture.allow();
    let mut child = tokio::process::Command::new(
        fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")),
    )
    .arg(protocol::PROVIDER_ARGUMENT)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut exact =
        br#"{"version":1,"id":1,"operation":{"kind":"history-check","permission":"read"}}"#
            .to_vec();
    exact.resize(protocol::MAX_REQUEST_BYTES - 1, b' ');
    exact.push(b'\n');
    assert_eq!(exact.len(), 4096);
    for chunk in exact.chunks(13) {
        input.write_all(chunk).await.unwrap();
    }
    input.flush().await.unwrap();
    let bytes = timeout(
        Duration::from_secs(5),
        protocol::read_frame(&mut output, protocol::MAX_RESPONSE_BYTES),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert!(matches!(
        serde_json::from_slice::<Response>(&bytes).unwrap().outcome,
        Outcome::Ok { .. }
    ));
    let calls = fixture.calls();
    exact.insert(exact.len() - 1, b' ');
    input.write_all(&exact).await.unwrap();
    input.flush().await.unwrap();
    let bytes = timeout(
        Duration::from_secs(5),
        protocol::read_frame(&mut output, protocol::MAX_RESPONSE_BYTES),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let response: Response = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response.id, 0);
    assert!(matches!(
        response.outcome,
        Outcome::Error {
            error: Failure {
                code: ErrorCode::InvalidRequest,
                ..
            },
        }
    ));
    assert_eq!(fixture.calls(), calls);
    assert!(
        !timeout(Duration::from_secs(2), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn blocked_reply_pipe_has_a_real_write_deadline_and_does_not_block_process_exit() {
    use claw_os_sdk::applet::protocol;
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    use tokio::{io::AsyncWriteExt, time::timeout};

    let fixture = Fixture::new();
    fixture.allow();
    fixture.calendar();
    let mut connection =
        rusqlite::Connection::open(fixture.root.join("data/calendar/events.db")).unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..2000 {
        transaction
            .execute(
                "INSERT INTO events VALUES (?1, ?2, '2026-09-09', NULL, '')",
                rusqlite::params![format!("bulk-{index}"), "x".repeat(128)],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let mut child = tokio::process::Command::new(
        fixture.provider(env!("CARGO_BIN_EXE_claw-os-applet-provider")),
    )
    .arg(protocol::PROVIDER_ARGUMENT)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let started = Instant::now();
    input.write_all(b"{\"version\":1,\"id\":1,\"operation\":{\"kind\":\"calendar-day\",\"date\":{\"year\":2026,\"month\":9,\"day\":9}}}\n")
        .await.unwrap();
    input.flush().await.unwrap();
    let status = timeout(Duration::from_secs(6), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert_eq!(
        fixture.calls(),
        "__policy check data.db.read --name calendar\n"
    );
}
