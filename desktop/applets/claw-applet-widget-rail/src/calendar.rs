// SPDX-License-Identifier: GPL-3.0-only

use crate::policy::{self, Scope};
use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::task::spawn_blocking;

const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CalendarEvent {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub location: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum StartSort {
    AllDay(Date),
    Timed(Timestamp),
}

pub async fn load_today() -> Result<Vec<CalendarEvent>, String> {
    policy::require("data.db.read", Scope::Name("calendar")).await?;

    let data_dir = std::env::var_os("COS_DATA_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Calendar data directory is unavailable.".to_string())?;
    let path = PathBuf::from(data_dir).join("calendar/events.db");
    let time_zone = TimeZone::system();
    let today = Timestamp::now().to_zoned(time_zone.clone()).date();

    spawn_blocking(move || load_today_from_db(&path, today, time_zone))
        .await
        .map_err(|error| format!("Calendar database task failed: {error}"))?
}

fn load_today_from_db(
    path: &Path,
    today: Date,
    time_zone: TimeZone,
) -> Result<Vec<CalendarEvent>, String> {
    match path.try_exists() {
        Ok(false) => return Ok(Vec::new()),
        Ok(true) => {}
        Err(error) => {
            return Err(format!(
                "Could not access calendar database {}: {error}",
                path.display()
            ));
        }
    }

    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            format!(
                "Could not open calendar database {}: {error}",
                path.display()
            )
        })?;
    connection
        .busy_timeout(DB_BUSY_TIMEOUT)
        .map_err(|error| format!("Could not configure calendar database: {error}"))?;

    let has_events_table = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("Could not inspect calendar database: {error}"))?
        .is_some();
    if !has_events_table {
        return Ok(Vec::new());
    }

    let mut statement = connection
        .prepare(
            "SELECT id, title, start_time, end_time, location
             FROM events",
        )
        .map_err(|error| format!("Could not query calendar database: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(CalendarEvent {
                id: row.get(0)?,
                title: row.get(1)?,
                start: row.get(2)?,
                end: row.get(3)?,
                location: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        })
        .map_err(|error| format!("Could not read calendar events: {error}"))?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row.map_err(|error| format!("Could not read a calendar event: {error}"))?);
    }
    Ok(filter_events_for_day(events, today, time_zone))
}

fn filter_events_for_day(
    events: Vec<CalendarEvent>,
    day: Date,
    time_zone: TimeZone,
) -> Vec<CalendarEvent> {
    let day_bounds = day_bounds(day, &time_zone);
    let mut matched = events
        .into_iter()
        .filter_map(|event| {
            let sort = parse_time(&event.start, &time_zone)?;
            event_overlaps_day(&event, &sort, day, day_bounds, &time_zone).then_some((sort, event))
        })
        .collect::<Vec<_>>();

    matched.sort_by(|(left_sort, left), (right_sort, right)| {
        left_sort
            .cmp(right_sort)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.location.cmp(&right.location))
    });
    matched.into_iter().map(|(_, event)| event).collect()
}

fn event_overlaps_day(
    event: &CalendarEvent,
    start: &StartSort,
    day: Date,
    day_bounds: Option<(Timestamp, Timestamp)>,
    time_zone: &TimeZone,
) -> bool {
    match start {
        StartSort::AllDay(start_date) => {
            let end_date = event
                .end
                .as_deref()
                .and_then(|end| parse_time(end, time_zone))
                .map(|end| match end {
                    StartSort::AllDay(date) => date,
                    StartSort::Timed(timestamp) => timestamp.to_zoned(time_zone.clone()).date(),
                })
                .or_else(|| start_date.tomorrow().ok())
                .unwrap_or(*start_date);
            *start_date <= day && end_date > day
        }
        StartSort::Timed(start_timestamp) => {
            let Some((day_start, day_end)) = day_bounds else {
                return false;
            };
            let end_timestamp = event
                .end
                .as_deref()
                .and_then(|end| parse_time(end, time_zone))
                .and_then(|end| match end {
                    StartSort::AllDay(date) => date
                        .to_zoned(time_zone.clone())
                        .ok()
                        .map(|zoned| zoned.timestamp()),
                    StartSort::Timed(timestamp) => Some(timestamp),
                });

            match end_timestamp.filter(|end| end > start_timestamp) {
                Some(end) => *start_timestamp < day_end && end > day_start,
                None => *start_timestamp >= day_start && *start_timestamp < day_end,
            }
        }
    }
}

fn day_bounds(day: Date, time_zone: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    let start = day.to_zoned(time_zone.clone()).ok()?.timestamp();
    let end = day
        .tomorrow()
        .ok()?
        .to_zoned(time_zone.clone())
        .ok()?
        .timestamp();
    Some((start, end))
}

fn parse_time(value: &str, time_zone: &TimeZone) -> Option<StartSort> {
    if value.len() == 10 {
        let date = value.parse::<Date>().ok()?;
        return Some(StartSort::AllDay(date));
    }

    if let Ok(timestamp) = value.parse::<Timestamp>() {
        return Some(StartSort::Timed(timestamp));
    }

    let datetime = value.parse::<DateTime>().ok()?;
    let zoned = datetime.to_zoned(time_zone.clone()).ok()?;
    Some(StartSort::Timed(zoned.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let matched = filter_events_for_day(events, day, time_zone);
        assert_eq!(
            matched
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["all-day", "offset-match", "utc-match", "naive-match"]
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

        let sorted = filter_events_for_day(events, day, time_zone);
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

        let matched = filter_events_for_day(vec![overnight, multi_day, ended], day, time_zone);
        assert_eq!(
            matched
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            ["multi-day", "overnight"]
        );
    }
}
