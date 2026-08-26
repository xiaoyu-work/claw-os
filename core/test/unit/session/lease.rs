use super::*;

#[test]
fn acquire_error_held_reports_pid_in_display() {
    let err = AcquireError::Held {
        held_by: Lease {
            pid: 4242,
            started_at: "2024-01-01T00:00:00Z".into(),
            heartbeat_at: "2024-01-01T00:00:30Z".into(),
            runtime: None,
        },
    };
    let s = format!("{err}");
    assert!(s.contains("4242"), "display: {s}");
}

#[test]
fn acquire_error_not_found_includes_sid() {
    let err = AcquireError::NotFound("ses_x_y".into());
    assert!(format!("{err}").contains("ses_x_y"));
}
