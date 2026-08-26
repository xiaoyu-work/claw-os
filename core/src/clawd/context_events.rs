use std::fs;
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::client_identity::ClientIdentity;

const MAX_QUERY_LIMIT: usize = 5_000;

pub fn append(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let source = required_string(&params, "source")?;
    if let Some(uid) = client.uid {
        if uid != 0 && (source == super::heartbeat::SOURCE || source.starts_with("clawd.")) {
            return Err(format!("reserved context event source: {source}"));
        }
    }
    let event_type = required_string(&params, "event_type")?;
    let ts = optional_timestamp(&params, "ts")?.unwrap_or_else(Utc::now);
    let received_at = Utc::now();
    let payload = params.get("payload").cloned().unwrap_or_else(|| json!({}));
    let metadata = params.get("metadata").cloned().unwrap_or_else(|| json!({}));

    let record = json!({
        "schema": 1,
        "event": "context.event",
        "ts": ts,
        "received_at": received_at,
        "source": source,
        "app_id": optional_string(&params, "app_id"),
        "event_type": event_type,
        "entity_id": optional_string(&params, "entity_id"),
        "session_id": optional_string(&params, "session_id"),
        "task_id": optional_string(&params, "task_id"),
        "payload": payload,
        "metadata": metadata,
        "client": client,
    });
    append_record(&record)?;

    Ok(json!({
        "accepted": true,
        "path": crate::paths::context_events_log_path(),
        "event": record,
    }))
}

pub fn query(params: Value) -> Result<Value, String> {
    query_with_owner(params, None)
}

pub fn query_for_client(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
    let uid = client.require_uid()?;
    query_with_owner(params, (uid != 0).then_some(uid))
}

fn query_with_owner(params: Value, owner_uid: Option<u32>) -> Result<Value, String> {
    let filters = QueryFilters::from_params(&params)?;
    let events = query_events(&filters, owner_uid)?;

    Ok(json!({
        "schema": 1,
        "path": crate::paths::context_events_log_path(),
        "limit": filters.limit,
        "order": filters.order.as_str(),
        "filters": {
            "source": filters.source,
            "app_id": filters.app_id,
            "event_type": filters.event_type,
            "entity_id": filters.entity_id,
            "since": filters.since,
            "until": filters.until,
        },
        "events": events,
    }))
}

pub(crate) fn event_visible_to(event: &Value, owner_uid: Option<u32>) -> bool {
    let Some(uid) = owner_uid else {
        return true;
    };
    event
        .pointer("/client/uid")
        .and_then(Value::as_u64)
        .is_some_and(|value| value == uid as u64)
        || (event.get("source").and_then(Value::as_str) == Some(super::heartbeat::SOURCE)
            && event.pointer("/client/uid").is_none())
}

pub fn context_payload(limit: usize) -> Value {
    let params = json!({
        "limit": limit,
        "order": "desc",
    });
    let events = query(params)
        .ok()
        .and_then(|value| value.get("events").cloned())
        .unwrap_or_else(|| json!([]));
    json!({
        "path": crate::paths::context_events_log_path(),
        "recent": events,
        "recent_limit": limit,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryOrder {
    Asc,
    Desc,
}

impl QueryOrder {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("desc").trim().to_ascii_lowercase().as_str() {
            "" | "desc" | "newest" | "newest_first" => Ok(Self::Desc),
            "asc" | "oldest" | "oldest_first" => Ok(Self::Asc),
            other => Err(format!("invalid order `{other}`; expected asc or desc")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone)]
struct QueryFilters {
    limit: usize,
    order: QueryOrder,
    source: Option<String>,
    app_id: Option<String>,
    event_type: Option<String>,
    entity_id: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl QueryFilters {
    fn from_params(params: &Value) -> Result<Self, String> {
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(100)
            .clamp(1, MAX_QUERY_LIMIT);
        let order = QueryOrder::parse(params.get("order").and_then(Value::as_str))?;
        let since = optional_timestamp(params, "since")?;
        let until = optional_timestamp(params, "until")?;
        if let (Some(since), Some(until)) = (since, until) {
            if since > until {
                return Err("since must be <= until".to_string());
            }
        }

        Ok(Self {
            limit,
            order,
            source: optional_string(params, "source"),
            app_id: optional_string(params, "app_id"),
            event_type: optional_string(params, "event_type"),
            entity_id: optional_string(params, "entity_id"),
            since,
            until,
        })
    }
}

fn query_events(
    filters: &QueryFilters,
    owner_uid: Option<u32>,
) -> Result<Vec<Value>, String> {
    let path = crate::paths::context_events_log_path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(format!(
                "failed to read context event journal {}: {err}",
                path.display()
            ));
        }
    };

    let mut matched = Vec::<(DateTime<Utc>, Value)>::new();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(ts) = value_timestamp(&value, "ts") else {
            continue;
        };
        if !matches_filters(&value, ts, filters) {
            continue;
        }
        if !event_visible_to(&value, owner_uid) {
            continue;
        }
        matched.push((ts, value));
    }

    matched.sort_by(|left, right| match filters.order {
        QueryOrder::Asc => left.0.cmp(&right.0),
        QueryOrder::Desc => right.0.cmp(&left.0),
    });

    Ok(matched
        .into_iter()
        .take(filters.limit)
        .map(|(_, value)| value)
        .collect())
}

fn matches_filters(value: &Value, ts: DateTime<Utc>, filters: &QueryFilters) -> bool {
    if let Some(since) = filters.since {
        if ts < since {
            return false;
        }
    }
    if let Some(until) = filters.until {
        if ts > until {
            return false;
        }
    }
    string_field_matches(value, "source", filters.source.as_deref())
        && string_field_matches(value, "app_id", filters.app_id.as_deref())
        && string_field_matches(value, "event_type", filters.event_type.as_deref())
        && string_field_matches(value, "entity_id", filters.entity_id.as_deref())
}

fn string_field_matches(value: &Value, key: &str, expected: Option<&str>) -> bool {
    match expected {
        Some(expected) => value.get(key).and_then(Value::as_str) == Some(expected),
        None => true,
    }
}

fn append_record(record: &Value) -> Result<(), String> {
    let line = serde_json::to_string(record).map_err(|err| err.to_string())?;
    let path = crate::paths::context_events_log_path();
    append_durable(&path, &line).map_err(|err| {
        format!(
            "failed to write context event journal {}: {err}",
            path.display()
        )
    })
}

fn append_durable(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::storage::ensure_private_dir(parent)?;
    }

    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    crate::storage::set_private_file(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }

    let write_result = writeln!(file, "{line}").and_then(|_| file.sync_all());

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }

    write_result
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key).ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_timestamp(params: &Value, key: &str) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = optional_string(params, key) else {
        return Ok(None);
    };
    parse_timestamp(&raw)
        .map(Some)
        .map_err(|err| format!("{key}: {err}"))
}

fn value_timestamp(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    value
        .get(key)?
        .as_str()
        .and_then(|raw| parse_timestamp(raw).ok())
}

fn parse_timestamp(raw: &str) -> Result<DateTime<Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| format!("invalid RFC3339 timestamp `{raw}`: {err}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/context_events.rs"
    ));
}
