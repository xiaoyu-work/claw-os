use super::*;

#[test]
fn append_and_query_filters_by_source_time_and_order() {
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", tmp.path());

    let client = ClientIdentity::unknown();
    append(
        json!({
            "source": "app.terminal.events",
            "app_id": "terminal",
            "event_type": "command.finished",
            "entity_id": "pty-1",
            "ts": "2026-05-17T20:00:00Z",
            "payload": {"exit_code": 0}
        }),
        &client,
    )
    .unwrap();
    append(
        json!({
            "source": "app.browser.events",
            "app_id": "browser",
            "event_type": "download.finished",
            "entity_id": "download-1",
            "ts": "2026-05-17T20:05:00Z",
            "payload": {"path": "/home/user/Downloads/logo.png"}
        }),
        &client,
    )
    .unwrap();
    append(
        json!({
            "source": "app.terminal.events",
            "app_id": "terminal",
            "event_type": "command.failed",
            "entity_id": "pty-1",
            "ts": "2026-05-17T20:10:00Z",
            "payload": {"exit_code": 1}
        }),
        &client,
    )
    .unwrap();

    let result = query(json!({
        "source": "app.terminal.events",
        "since": "2026-05-17T20:01:00Z",
        "until": "2026-05-17T20:15:00Z",
        "order": "asc"
    }))
    .unwrap();
    let events = result["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "command.failed");
    assert_eq!(events[0]["payload"]["exit_code"], 1);

    match prev {
        Some(value) => std::env::set_var("COS_DATA_DIR", value),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}

#[test]
fn query_rejects_backwards_time_range() {
    let err = query(json!({
        "since": "2026-05-17T20:10:00Z",
        "until": "2026-05-17T20:00:00Z"
    }))
    .unwrap_err();
    assert!(err.contains("since must be <= until"));
}
