use super::*;

#[test]
fn internal_reader_requires_its_root_broker_and_a_valid_date() {
    use std::os::fd::AsRawFd;
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(reader::run(&[]).is_err());
    for (year, month, day) in [("2026", "9", "9"), ("2026", "2", "30"), ("10000", "1", "1")] {
        let args = [
            "--query-v1",
            year,
            month,
            day,
            &socket.as_raw_fd().to_string(),
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert!(reader::run(&args).is_err());
    }
}

#[cfg(feature = "provider")]
#[test]
fn calendar_wire_errors_and_shapes_remain_explicit() {
    use std::os::unix::process::ExitStatusExt;
    let output = |body: &str| std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: body.as_bytes().to_vec(),
        stderr: Vec::new(),
    };
    let denied = decode_query(output(
        r#"{"wire_version":1,"ok":false,"code":"PERMISSION_DENIED","error":"denied"}"#,
    ))
    .unwrap_err();
    assert_eq!(denied.kind, FailureKind::Denied);
    for invalid in [
        r#"{"wire_version":1,"ok":true,"data":{"wrong":[]}}"#,
        r#"{"wire_version":2,"ok":true,"data":{"events":[]}}"#,
        r#"{"wire_version":1,"ok":true,"data":{"events":null}}"#,
    ] {
        assert!(decode_query(output(invalid)).is_err());
    }
    assert!(
        decode_query(output(
            r#"{"wire_version":1,"ok":true,"data":{"events":[]}}"#
        ))
        .unwrap()
        .is_empty()
    );
}

#[cfg(not(feature = "provider"))]
#[test]
fn default_library_keeps_plain_kernel_output_without_changing_json_features() {
    use std::os::unix::process::ExitStatusExt;
    let result = decode_query(std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: br#"{"events":[]}"#.to_vec(),
        stderr: Vec::new(),
    })
    .unwrap();
    assert!(result.is_empty());
}

fn filtered(events: Vec<CalendarEvent>, day: Date, time_zone: TimeZone) -> Vec<CalendarEvent> {
    filter_events_for_day(
        events.into_iter().map(Ok),
        day,
        time_zone,
        Instant::now() + QUERY_TIMEOUT,
    )
    .unwrap()
}

fn event(id: &str, title: &str, start: &str) -> CalendarEvent {
    CalendarEvent {
        id: id.to_string(),
        title: title.to_string(),
        start: start.to_string(),
        end: None,
        location: String::new(),
    }
}

#[test]
fn filters_local_day_across_offsets_date_only_and_naive_values() {
    let time_zone = TimeZone::fixed(jiff::tz::offset(5));
    let day: Date = "2026-08-05".parse().unwrap();
    let events = vec![
        event("all-day", "All day", "2026-08-05"),
        event("utc-match", "UTC match", "2026-08-04T20:00:00Z"),
        event("offset-match", "Offset match", "2026-08-05T00:15:00+05:00"),
        event("naive-match", "Naive match", "2026-08-05T09:30:00"),
        event("utc-previous", "Previous", "2026-08-04T18:59:59Z"),
        event("offset-next", "Next", "2026-08-05T23:30:00-04:00"),
        event("other-day", "Other all day", "2026-08-06"),
        event("malformed", "Malformed", "not-a-date"),
    ];

    let matched = filtered(events, day, time_zone);
    assert_eq!(
        matched
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["all-day", "offset-match", "utc-match", "naive-match"]
    );
}

#[test]
fn filters_events_for_the_selected_day_instead_of_today() {
    let time_zone = TimeZone::fixed(jiff::tz::offset(0));
    let selected: Date = "2031-11-18".parse().unwrap();
    let events = vec![
        event("selected", "Selected", "2031-11-18T09:00:00Z"),
        event("previous", "Previous", "2031-11-17T23:59:59Z"),
        event("next", "Next", "2031-11-19"),
    ];

    let matched = filtered(events, selected, time_zone);
    assert_eq!(
        matched
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["selected"]
    );
}

#[test]
fn sorts_all_day_first_then_by_instant_with_deterministic_ties() {
    let time_zone = TimeZone::fixed(jiff::tz::offset(2));
    let day: Date = "2026-08-05".parse().unwrap();
    let events = vec![
        event("late", "Late", "2026-08-05T12:00:00+02:00"),
        event("tie-b", "Bravo", "2026-08-05T09:00:00Z"),
        event("all-z", "Zulu", "2026-08-05"),
        event("tie-a", "Alpha", "2026-08-05T11:00:00+02:00"),
        event("early", "Early", "2026-08-05T08:30:00Z"),
        event("all-a", "Alpha", "2026-08-05"),
    ];

    let sorted = filtered(events, day, time_zone);
    assert_eq!(
        sorted
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["all-a", "all-z", "early", "tie-a", "tie-b", "late"]
    );
}

#[test]
fn includes_events_that_overlap_the_local_day() {
    let time_zone = TimeZone::fixed(jiff::tz::offset(0));
    let day: Date = "2026-08-05".parse().unwrap();
    let mut overnight = event("overnight", "Overnight", "2026-08-04T23:00:00Z");
    overnight.end = Some("2026-08-05T01:00:00Z".to_string());
    let mut multi_day = event("multi-day", "Conference", "2026-08-04");
    multi_day.end = Some("2026-08-06".to_string());
    let mut ended = event("ended", "Ended", "2026-08-04T20:00:00Z");
    ended.end = Some("2026-08-04T22:00:00Z".to_string());

    let matched = filtered(vec![overnight, multi_day, ended], day, time_zone);
    assert_eq!(
        matched
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["multi-day", "overnight"]
    );
}

#[test]
fn result_bounds_and_deadlines_fail_without_returning_partial_events() {
    let day = "2026-08-05".parse().unwrap();
    let zone = TimeZone::UTC;
    let ordinary = event("first", "First", "2026-08-05");
    let oversized = event(
        "large",
        &"\0".repeat(MAX_CALENDAR_BYTES / 6 + 1),
        "2026-08-05",
    );
    let error = filter_events_for_day(
        [Ok(ordinary.clone()), Ok(oversized)],
        day,
        zone.clone(),
        Instant::now() + QUERY_TIMEOUT,
    )
    .unwrap_err();
    assert!(error.contains("byte limit"));
    let error = filter_events_for_day(
        [Ok(ordinary)],
        day,
        zone,
        Instant::now() - Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error.contains("deadline"));
}
