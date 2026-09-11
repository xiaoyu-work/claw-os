use super::*;
use serde_json::json;

#[test]
fn validates_calendar_dates_without_touching_a_provider() {
    for date in [
        CalendarDate {
            year: 2024,
            month: 2,
            day: 29,
        },
        CalendarDate {
            year: 2000,
            month: 2,
            day: 29,
        },
        CalendarDate {
            year: -9999,
            month: 1,
            day: 1,
        },
        CalendarDate {
            year: 9999,
            month: 12,
            day: 31,
        },
    ] {
        date.validate().unwrap();
    }
    for date in [
        CalendarDate {
            year: 2025,
            month: 2,
            day: 29,
        },
        CalendarDate {
            year: 1900,
            month: 2,
            day: 29,
        },
        CalendarDate {
            year: 2026,
            month: 4,
            day: 31,
        },
        CalendarDate {
            year: 2026,
            month: 0,
            day: 1,
        },
        CalendarDate {
            year: 2026,
            month: 13,
            day: 1,
        },
        CalendarDate {
            year: 2026,
            month: 1,
            day: 0,
        },
        CalendarDate {
            year: 10000,
            month: 1,
            day: 1,
        },
    ] {
        assert_eq!(date.validate().unwrap_err().code, ErrorCode::InvalidRequest);
    }
}

#[test]
fn request_schema_is_closed_and_never_accepts_authority() {
    for value in [
        json!({"version":1,"id":1,"operation":{"kind":"tasks","owner":10}}),
        json!({"version":1,"id":1,"operation":{"kind":"tasks"},"session":"other"}),
        json!({"version":1,"id":1,"operation":{"kind":"tasks"},"app_id":"panel-calendar"}),
        json!({"version":1,"id":1,"operation":{"kind":"tasks"},"native":true}),
        json!({"version":1,"id":1,"operation":{"kind":"tasks"},"trusted":true}),
        json!({"version":1,"id":1,"operation":{"kind":"history-check","permission":"wild"}}),
        json!({"version":1,"id":1,"operation":{"kind":"calendar-day","date":{"year":2026,"month":9,"day":9,"path":"/data"}}}),
        json!({"version":1,"id":1,"operation":{"kind":"run","program":"cos"}}),
        json!({"version":1,"id":-1,"operation":{"kind":"system"}}),
        json!({"version":1,"id":1,"operation":{"kind":"calendar-today","scope":"*"}}),
    ] {
        assert!(serde_json::from_value::<Request>(value).is_err());
    }
    assert!(serde_json::from_str::<Request>(
        r#"{"version":1,"id":1,"id":2,"operation":{"kind":"tasks"}}"#,
    )
    .is_err());
    let mut request = Request {
        version: 2,
        id: 1,
        operation: Operation::Tasks {},
    };
    assert_eq!(
        request.validate().unwrap_err().code,
        ErrorCode::UnsupportedVersion
    );
    request.version = VERSION;
    request.id = 0;
    assert_eq!(
        request.validate().unwrap_err().code,
        ErrorCode::InvalidRequest
    );
}

#[tokio::test]
async fn framing_is_bounded_even_across_small_reads() {
    let mut input = tokio::io::BufReader::with_capacity(2, &b"abc\nx\n"[..]);
    assert_eq!(
        read_frame(&mut input, 4).await.unwrap(),
        Some(b"abc".to_vec())
    );
    assert_eq!(
        read_frame(&mut input, 4).await.unwrap(),
        Some(b"x".to_vec())
    );
    assert_eq!(read_frame(&mut input, 4).await.unwrap(), None);
    let mut input = tokio::io::BufReader::with_capacity(2, &b"abcd\n"[..]);
    assert_eq!(
        read_frame(&mut input, 4).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut input = tokio::io::BufReader::new(&b"abc"[..]);
    assert_eq!(
        read_frame(&mut input, 4).await.unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(encode_frame(&"x", 4).unwrap(), b"\"x\"\n");
    assert!(encode_frame(&"x", 3).is_err());
}

#[test]
fn response_type_and_identity_are_explicit() {
    let response = Response {
        version: VERSION,
        id: 3,
        outcome: Outcome::Error {
            error: Failure::new(ErrorCode::PermissionDenied, "history denied"),
        },
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"version":1,"id":3,"outcome":{"status":"error","error":{
            "code":"permission-denied","message":"history denied"
        }}}),
    );
    let tasks = Operation::Tasks {};
    assert!(!tasks.accepts(&Data::HistoryAllowed {}));
    assert!(tasks.accepts(&Data::Tasks { tasks: Vec::new() }));
}

#[test]
fn fractional_telemetry_roundtrips_through_tagged_wire_envelopes() {
    for cpu_percent in [None, Some(0.0), Some(42.5), Some(100.0)] {
        let response = Response {
            version: VERSION,
            id: 1,
            outcome: Outcome::Ok {
                data: Data::System {
                    summary: SystemSummary {
                        cpu_percent,
                        ..Default::default()
                    },
                },
            },
        };
        let bytes = encode_frame(&response, MAX_RESPONSE_BYTES).unwrap();
        assert_eq!(
            serde_json::from_slice::<Response>(&bytes).unwrap(),
            response
        );
    }

    for percentage in ["-1", "101", "1e999"] {
        let raw = format!(
            r#"{{"version":1,"id":1,"outcome":{{"status":"ok","data":{{"kind":"system","summary":{{"cpu_percent":{percentage},"fallback":false}}}}}}}}"#,
        );
        assert!(serde_json::from_str::<Response>(&raw).is_err());
    }
}

#[test]
fn telemetry_rejects_numeric_objects_instead_of_confusing_them_with_json_numbers() {
    let raw = r#"{"version":1,"id":1,"outcome":{"status":"ok","data":{"kind":"system","summary":{"cpu_percent":{"$serde_json::private::Number":"42.5"},"fallback":false}}}}"#;
    assert!(serde_json::from_str::<Response>(raw).is_err());
}

#[test]
fn invalid_telemetry_cannot_be_serialized_as_a_successful_null_measurement() {
    for cpu_percent in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 100.1] {
        let response = Response {
            version: VERSION,
            id: 1,
            outcome: Outcome::Ok {
                data: Data::System {
                    summary: SystemSummary {
                        cpu_percent: Some(cpu_percent),
                        ..Default::default()
                    },
                },
            },
        };
        assert!(encode_frame(&response, MAX_RESPONSE_BYTES).is_err());
    }
}

#[test]
fn versioned_conformance_cases_match_the_public_codec() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wire/v1/applet-services.cases.json"
    )))
    .unwrap();
    for case in cases["requests"].as_array().unwrap() {
        let result = serde_json::from_str::<Request>(case["wire"].as_str().unwrap())
            .map_err(|error| Failure::new(ErrorCode::InvalidRequest, error.to_string()))
            .and_then(|request| request.validate());
        let error = result
            .err()
            .map(|error| serde_json::to_value(error.code).unwrap());
        assert_eq!(
            error.unwrap_or(serde_json::Value::Null),
            case["error"],
            "{}",
            case["name"]
        );
    }
    for case in cases["responses"].as_array().unwrap() {
        assert_eq!(
            serde_json::from_str::<Response>(case["wire"].as_str().unwrap()).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["name"],
        );
    }
}
