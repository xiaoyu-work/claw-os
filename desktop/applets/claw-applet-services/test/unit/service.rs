use super::*;
use claw_os_sdk::applet::CalendarDate;
use std::fs;

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/support/fixture.rs"
    ));
}
use fixture::Fixture;

fn request(operation: Operation) -> Request {
    Request {
        version: protocol::VERSION,
        id: 1,
        operation,
    }
}

fn day() -> Operation {
    Operation::CalendarDay {
        date: CalendarDate {
            year: 2026,
            month: 9,
            day: 9,
        },
    }
}

#[tokio::test]
async fn every_operation_checks_only_its_original_grant_before_access() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("data/calendar/events.db"),
        "must not be opened",
    )
    .unwrap();
    let mut service = Service::default();
    for operation in [
        day(),
        Operation::CalendarToday {},
        Operation::Tasks {},
        Operation::System {},
        Operation::HistoryCheck {
            permission: HistoryPermission::Read,
        },
        Operation::HistoryCheck {
            permission: HistoryPermission::Write,
        },
    ] {
        let response = service.handle(request(operation)).await;
        assert_eq!(
            response.outcome,
            Outcome::Error {
                error: Failure::new(ErrorCode::PermissionDenied, "fixture denied"),
            }
        );
    }
    assert_eq!(
        fixture.calls(),
        "__policy check data.db.read --name calendar\n\
         __policy check data.db.read --name calendar\n\
         __policy check agent.observe --name tasks\n\
         __policy check sys.observe --wild\n\
         __policy check clipboard.read --name history\n\
         __policy check clipboard.write --name history\n"
    );
}

#[tokio::test]
async fn malformed_and_unsupported_requests_never_reach_policy() {
    let fixture = Fixture::new();
    let mut service = Service::default();
    let invalid = Operation::CalendarDay {
        date: CalendarDate {
            year: 2026,
            month: 2,
            day: 30,
        },
    };
    for request in [
        request(invalid),
        Request {
            version: 99,
            ..request(day())
        },
        Request {
            id: 0,
            ..request(day())
        },
    ] {
        assert!(matches!(
            service.handle(request).await.outcome,
            Outcome::Error { .. }
        ));
    }
    let input = br#"{"version":1,"id":1,"operation":{"kind":"history-check","permission":"read","scope":"selection"}}
{"version":1,"id":2,"operation":{"kind":"tasks"},"owner":10}
{"version":1,"id":3,"operation":{"kind":"system","program":"other"}}
{"version":1,"id":4,"operation":{"kind":"tasks"},"app_id":"panel-calendar"}
{"version":1,"id":5,"operation":{"kind":"tasks"},"native":true,"trusted":true}
"#;
    let mut output = Vec::new();
    serve(tokio::io::BufReader::new(&input[..]), &mut output)
        .await
        .unwrap();
    let replies: Vec<_> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(replies.len(), 5);
    for line in replies {
        let response: Response = serde_json::from_slice(line).unwrap();
        assert!(matches!(
            response.outcome,
            Outcome::Error {
                error: Failure {
                    code: ErrorCode::InvalidRequest,
                    ..
                },
            }
        ));
    }
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn stdio_rejects_oversized_and_incomplete_frames() {
    let fixture = Fixture::new();
    for input in [vec![b'x'; protocol::MAX_REQUEST_BYTES + 1], b"{".to_vec()] {
        let mut output = Vec::new();
        assert!(
            serve(tokio::io::BufReader::new(input.as_slice()), &mut output)
                .await
                .is_err()
        );
        let response: Response = serde_json::from_slice(&output).unwrap();
        assert!(matches!(
            response.outcome,
            Outcome::Error {
                error: Failure {
                    code: ErrorCode::InvalidRequest,
                    ..
                },
            }
        ));
        assert!(output.len() < protocol::MAX_RESPONSE_BYTES);
    }
    assert!(fixture.calls().is_empty());
}

#[tokio::test]
async fn presents_the_original_calendar_task_and_system_records() {
    let fixture = Fixture::new();
    fixture.calendar();
    fixture.allow();
    let mut service = Service::default();
    let response = service.handle(request(day())).await;
    assert_eq!(
        response.outcome,
        Outcome::Ok {
            data: Data::Calendar {
                events: vec![CalendarEvent {
                    id: "event-1".into(),
                    title: "Review".into(),
                    start: "2026-09-09".into(),
                    end: Some("2026-09-10".into()),
                    location: "Office".into(),
                }]
            }
        }
    );
    let response = service.handle(request(Operation::Tasks {})).await;
    assert_eq!(
        response.outcome,
        Outcome::Ok {
            data: Data::Tasks {
                tasks: vec![Task {
                    id: "task-1".into(),
                    purpose: "Review".into(),
                    status: "running".into(),
                    created_at: "2026-09-09".into(),
                }]
            }
        }
    );
    for _ in 0..2 {
        let response = service.handle(request(Operation::System {})).await;
        let Outcome::Ok {
            data: Data::System { summary },
        } = response.outcome
        else {
            panic!("system data expected")
        };
        assert_eq!(
            summary.memory,
            Some(Usage {
                used_mb: 200,
                total_mb: 1000
            })
        );
        assert_eq!(
            summary.storage,
            Some(Usage {
                used_mb: 300,
                total_mb: 2000
            })
        );
        assert!(!summary.fallback);
    }
    assert_eq!(
        fixture.calls(),
        "__policy check data.db.read --name calendar\n\
         __policy check agent.observe --name tasks\nagent ls\n\
         __policy check sys.observe --wild\nsys resources\n\
         __policy check sys.observe --wild\nsys resources\n"
    );
}

#[tokio::test]
async fn missing_and_broken_providers_are_visible_errors() {
    let fixture = Fixture::new();
    let mut service = Service::default();
    unsafe {
        std::env::set_var("COS_BIN", fixture.root.join("missing"));
    }
    let response = service.handle(request(day())).await;
    assert!(matches!(
        response.outcome,
        Outcome::Error {
            error: Failure {
                code: ErrorCode::ProviderUnavailable,
                ..
            },
        }
    ));
    unsafe {
        std::env::set_var("COS_BIN", fixture.root.join("cos"));
    }
    fs::write(fixture.root.join("policy-reply"), "not json").unwrap();
    let response = service.handle(request(day())).await;
    assert!(matches!(
        response.outcome,
        Outcome::Error {
            error: Failure {
                code: ErrorCode::ProviderFailure,
                ..
            },
        }
    ));
    fs::remove_file(fixture.root.join("policy-reply")).unwrap();
    fixture.allow();
    fs::write(fixture.root.join("provider-failed"), "").unwrap();
    let response = service.handle(request(Operation::Tasks {})).await;
    assert_eq!(
        response.outcome,
        Outcome::Error {
            error: Failure::new(ErrorCode::ProviderFailure, "fixture provider failed"),
        }
    );
}

#[tokio::test]
async fn oversized_result_is_an_error_not_an_empty_success() {
    let response = Response {
        version: protocol::VERSION,
        id: 7,
        outcome: Outcome::Ok {
            data: Data::Calendar {
                events: vec![CalendarEvent {
                    id: "large".into(),
                    title: "x".repeat(protocol::MAX_RESPONSE_BYTES),
                    start: "2026-09-09".into(),
                    end: None,
                    location: String::new(),
                }],
            },
        },
    };
    let mut output = Vec::new();
    send(&mut output, response).await.unwrap();
    assert!(output.len() < protocol::MAX_RESPONSE_BYTES);
    let response: Response = serde_json::from_slice(&output).unwrap();
    assert_eq!(response.id, 7);
    assert!(matches!(
        response.outcome,
        Outcome::Error {
            error: Failure {
                code: ErrorCode::ProviderFailure,
                ..
            },
        }
    ));
}
