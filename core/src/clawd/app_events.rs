use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::caps::{Cap, Scope, Verb};

use super::authority::Decision;
use super::client_identity::ClientIdentity;
use super::wire::requests::AppEventPublish;

const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;

pub fn publish(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let request: AppEventPublish =
        serde_json::from_value(params).map_err(|error| error.to_string())?;
    let app_id = authority
        .app_id()
        .ok_or("App events require an authenticated App session")?;
    if authority.owner_uid() == 0 {
        return Err("App event publication requires a non-root owner".into());
    }
    let topic = format!("app:{app_id}");
    let payload = request
        .payload
        .map(|value| value.into_value())
        .unwrap_or_else(|| json!({}));
    if serde_json::to_vec(&payload)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_PAYLOAD_BYTES
    {
        return Err("App event payload exceeds 16 KiB".into());
    }
    let _authorized = authority.require(Cap::new(Verb::IPC_PUBLISH, Scope::name(&topic)))?;
    let owner = authority.owner_uid();
    let event_id = format!(
        "app-event-{}",
        hex::encode(Sha256::digest(
            format!("{owner}\0{app_id}\0{}", request.key.as_str()).as_bytes(),
        ))
    );
    let now = Utc::now();
    let record = json!({
        "schema":1, "event":"context.event", "id":event_id,
        "ts":now, "received_at":now, "source":topic, "app_id":app_id,
        "event_type":request.event_type.as_str(),
        "entity_id":request.entity_id.map(|value| value.as_str().to_string()),
        "session_id":authority.session_id(), "task_id":authority.task_id(),
        "payload":payload, "metadata":{"source":"caller_reported"},
        "client":{"uid":owner,"pid":client.pid},
    });
    append_once(&record)
}

fn append_once(record: &Value) -> Result<Value, String> {
    let path = crate::paths::context_events_log_path();
    if let Some(parent) = path.parent() {
        crate::storage::ensure_private_dir(parent).map_err(|error| error.to_string())?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("App event journal must be an owner-owned single-link regular file".into());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("App event journal is busy".into());
    }
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_JOURNAL_BYTES {
        return Err("App event journal exceeds its retained 32 MiB bound".into());
    }
    let mut existing = String::new();
    file.read_to_string(&mut existing)
        .map_err(|error| error.to_string())?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        return Err("App event journal has an unresolved partial append; replay is refused".into());
    }
    for line in existing.lines().filter(|line| !line.is_empty()) {
        let previous: Value = serde_json::from_str(line)
            .map_err(|_| "App event journal is corrupt; replay is refused")?;
        if previous["id"] == record["id"] {
            for field in [
                "source",
                "app_id",
                "event_type",
                "entity_id",
                "payload",
                "client",
            ] {
                if field == "client" {
                    if previous["client"]["uid"] != record["client"]["uid"] {
                        return Err("App event key has a different owner".into());
                    }
                } else if previous[field] != record[field] {
                    return Err("App event key is already bound to different reported data".into());
                }
            }
            return Ok(json!({"schema":1,"accepted":true,"deduplicated":true,"event":previous}));
        }
    }
    let encoded = serde_json::to_string(record).map_err(|error| error.to_string())?;
    if existing.len() + encoded.len() + 1 > MAX_JOURNAL_BYTES as usize {
        return Err("App event journal reached its retained bound".into());
    }
    writeln!(file, "{encoded}")
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            format!(
                "App event append outcome is indeterminate; retry only its same event key: {error}"
            )
        })?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!("App event directory sync outcome is indeterminate: {error}")
            })?;
    }
    Ok(json!({"schema":1,"accepted":true,"deduplicated":false,"event":record}))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_events.rs"
    ));
}
