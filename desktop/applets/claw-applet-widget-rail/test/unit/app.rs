use super::*;

fn task(status: &str, created_at: &str) -> Task {
    Task {
        id: format!("ses_{status}_{created_at}"),
        purpose: status.to_string(),
        status: status.to_string(),
        creator_runtime: None,
        created_at: created_at.to_string(),
        ended_at: None,
        lease: None,
    }
}

#[test]
fn active_tasks_sort_before_recent_tasks() {
    let selected = select_tasks(vec![
        task("done", "2026-08-05T10:00:00Z"),
        task("running", "2026-08-05T08:00:00Z"),
        task("failed", "2026-08-05T11:00:00Z"),
        task("pending", "2026-08-05T09:00:00Z"),
    ]);
    assert_eq!(selected.len(), 4);
    assert_eq!(selected[0].status, "pending");
    assert_eq!(selected[1].status, "running");
    assert_eq!(selected[2].status, "failed");
    assert_eq!(selected[3].status, "done");
}

#[test]
fn suggestions_are_deterministic_from_state() {
    let calendar = SourceState::Empty;
    let tasks = SourceState::Ready(vec![task("running", "2026-08-05T08:00:00Z")]);
    let system = SourceState::Ready(SystemSummary {
        memory: Some(Usage {
            used_mb: 900,
            total_mb: 1000,
        }),
        ..Default::default()
    });
    let result = suggestions(&calendar, &tasks, &system);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0], fl!("suggestion-clear"));
    assert!(result[1].contains('1'));
    assert_eq!(result[2], fl!("suggestion-memory"));
}

#[test]
fn source_completion_only_clears_its_own_in_flight_guard() {
    let mut rail = WidgetRail {
        calendar_in_flight: true,
        tasks_in_flight: true,
        system_in_flight: true,
        ..Default::default()
    };
    let _ = cosmic::Application::update(
        &mut rail,
        Message::CalendarLoaded(Err("timed out".to_string())),
    );
    assert!(!rail.calendar_in_flight);
    assert!(rail.tasks_in_flight);
    assert!(rail.system_in_flight);
}

#[test]
fn formats_event_times_and_rates() {
    assert_eq!(event_time("2026-08-05"), fl!("all-day"));
    let eastern = TimeZone::fixed(jiff::tz::offset(-4));
    assert_eq!(
        event_time_in_zone("2026-08-05T09:30:00Z", eastern.clone()),
        "05:30"
    );
    assert_eq!(
        event_time_in_zone("2026-08-05T09:30:00+02:00", eastern),
        "03:30"
    );
    assert_eq!(format_rate(1536), "2 KB/s");
}
