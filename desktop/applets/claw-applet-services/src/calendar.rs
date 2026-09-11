// SPDX-License-Identifier: GPL-3.0-only

use crate::policy::{self, Scope};
use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, limits::Limit};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::task::spawn_blocking;

const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CALENDAR_BYTES: usize = crate::command::OUTPUT_BYTES;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    let time_zone = TimeZone::system();
    let today = Timestamp::now().to_zoned(time_zone).date();
    load_day(today).await
}

pub async fn load_day(day: Date) -> Result<Vec<CalendarEvent>, String> {
    policy::require("data.db.read", Scope::Name("calendar")).await?;
    load_day_authorized(day).await
}

pub(crate) async fn load_day_authorized(day: Date) -> Result<Vec<CalendarEvent>, String> {
    let data_dir = std::env::var_os("COS_DATA_DIR")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Calendar data directory is unavailable.".to_string())?;
    let path = PathBuf::from(data_dir).join("calendar/events.db");
    let time_zone = TimeZone::system();

    spawn_blocking(move || load_day_from_db(&path, day, time_zone))
        .await
        .map_err(|error| format!("Calendar database task failed: {error}"))?
}

fn load_day_from_db(
    path: &Path,
    day: Date,
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
    connection
        .set_limit(
            Limit::SQLITE_LIMIT_LENGTH,
            i32::try_from(MAX_CALENDAR_BYTES).expect("calendar byte limit fits SQLite"),
        )
        .map_err(|error| format!("Could not bound calendar database records: {error}"))?;
    let deadline = Instant::now() + QUERY_TIMEOUT;
    connection
        .progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|error| format!("Could not bound calendar query time: {error}"))?;

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

    filter_events_for_day(
        rows.map(|row| row.map_err(|error| format!("Could not read a calendar event: {error}"))),
        day,
        time_zone,
        deadline,
    )
}

fn filter_events_for_day(
    events: impl IntoIterator<Item = Result<CalendarEvent, String>>,
    day: Date,
    time_zone: TimeZone,
    deadline: Instant,
) -> Result<Vec<CalendarEvent>, String> {
    let day_bounds = day_bounds(day, &time_zone);
    let mut matched = Vec::new();
    let mut bytes = 2usize;
    for event in events {
        if Instant::now() >= deadline {
            return Err("Calendar query exceeded its three-second deadline.".into());
        }
        let event = event?;
        let Some(sort) = parse_time(&event.start, &time_zone) else {
            continue;
        };
        if !event_overlaps_day(&event, &sort, day, day_bounds, &time_zone) {
            continue;
        }
        let encoded = serde_json::to_vec(&event)
            .map_err(|error| format!("Could not measure calendar event: {error}"))?;
        bytes = bytes.saturating_add(encoded.len()).saturating_add(1);
        if bytes > MAX_CALENDAR_BYTES {
            return Err("Calendar result exceeds its one-MiB byte limit.".into());
        }
        matched.push((sort, event));
    }

    matched.sort_by(|(left_sort, left), (right_sort, right)| {
        left_sort
            .cmp(right_sort)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.location.cmp(&right.location))
    });
    Ok(matched.into_iter().map(|(_, event)| event).collect())
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/calendar.rs"
    ));
}
