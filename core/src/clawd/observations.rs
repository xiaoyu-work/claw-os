use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::SessionAction;
use crate::agent::service::{Job, JobStatus, Store};
use crate::caps::{Cap, Scope, Verb};

const MAX_TASKS: usize = 100;
const MAX_PURPOSE_CHARS: usize = 256;

fn authorize(authority: &Decision, owner: u32, action: &str) -> Result<Authorized, BrokerError> {
    if authority.owner_uid() != owner {
        return Err(BrokerError::authorization(
            "Observation belongs to another owner",
        ));
    }
    let cap = match action {
        "tasks" => Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks")),
        "resources" => Cap::new(Verb::SYS_OBSERVE, Scope::Wild),
        _ => return Err(BrokerError::execution("Unknown observation action")),
    };
    authority.require(cap).map_err(BrokerError::authorization)
}

pub async fn query(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, BrokerError> {
    let request: SessionAction = serde_json::from_value(params)
        .map_err(|error| BrokerError::execution(format!("Invalid observation request: {error}")))?;
    let owner = client.require_uid().map_err(BrokerError::authorization)?;
    let action = request.action.as_str();
    let _authorized = authorize(authority, owner, action)?;
    match action {
        "tasks" => tokio::task::spawn_blocking(move || tasks(owner))
            .await
            .map_err(|error| BrokerError::unavailable(error.to_string()))?
            .map_err(BrokerError::unavailable),
        "resources" => tokio::task::spawn_blocking(resources)
            .await
            .map_err(|error| BrokerError::unavailable(error.to_string()))?,
        _ => unreachable!("authorized closed observation action"),
    }
}

fn task_value(job: Job) -> Value {
    json!({
        "id": job.id,
        "purpose": job.prompt.lines().next().unwrap_or_default().chars()
            .take(MAX_PURPOSE_CHARS).collect::<String>(),
        "status": job.status,
        "created_at": job.created_at,
    })
}

fn tasks(owner: u32) -> Result<Value, String> {
    let store = Store::open_default().map_err(|error| error.to_string())?;
    tasks_from(&store, owner)
}

fn tasks_from(store: &Store, owner: u32) -> Result<Value, String> {
    let mut jobs = Vec::new();
    for bucket in [
        JobStatus::Pending,
        JobStatus::Running,
        JobStatus::WaitingApproval,
        JobStatus::Ok,
    ] {
        jobs.extend(
            store
                .list_bucket_for_owner(bucket, Some(MAX_TASKS), Some(owner))
                .map_err(|error| error.to_string())?,
        );
    }
    jobs.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    jobs.truncate(MAX_TASKS);
    Ok(json!({"tasks": jobs.into_iter().map(task_value).collect::<Vec<_>>()}))
}

struct Sample {
    time: Instant,
    cpu: Option<(u64, u64)>,
    network: Option<(u64, u64)>,
}

fn cpu_sample(text: &str) -> Option<(u64, u64)> {
    let mut fields = text.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values = fields
        .take(8)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }
    let total = values
        .iter()
        .try_fold(0_u64, |total, value| total.checked_add(*value))?;
    let idle = values[3].checked_add(*values.get(4).unwrap_or(&0))?;
    Some((total, idle))
}

fn network_sample(text: &str) -> Option<(u64, u64)> {
    let mut total = (0_u64, 0_u64);
    for line in text.lines().skip(2) {
        let (interface, values) = line.split_once(':')?;
        if interface.trim() == "lo" {
            continue;
        }
        let values = values.split_whitespace().collect::<Vec<_>>();
        total.0 = total.0.checked_add(values.first()?.parse::<u64>().ok()?)?;
        total.1 = total.1.checked_add(values.get(8)?.parse::<u64>().ok()?)?;
    }
    Some(total)
}

fn deltas(previous: Option<&Sample>, current: &Sample) -> (Option<f64>, Option<u64>, Option<u64>) {
    let Some(previous) = previous else {
        return (None, None, None);
    };
    let cpu = previous.cpu.zip(current.cpu).and_then(|(old, new)| {
        let total = new.0.checked_sub(old.0)?;
        let idle = new.1.checked_sub(old.1)?;
        (total > 0 && idle <= total).then(|| (total - idle) as f64 * 100.0 / total as f64)
    });
    let seconds = current.time.duration_since(previous.time).as_secs_f64();
    let network = previous
        .network
        .zip(current.network)
        .and_then(|(old, new)| {
            if seconds <= 0.0 {
                return None;
            }
            Some((
                (new.0.checked_sub(old.0)? as f64 / seconds) as u64,
                (new.1.checked_sub(old.1)? as f64 / seconds) as u64,
            ))
        });
    (
        cpu,
        network.map(|value| value.0),
        network.map(|value| value.1),
    )
}

fn memory() -> Option<Value> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let read = |name| {
        text.lines()
            .find(|line| line.starts_with(name))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()
    };
    let total = read("MemTotal:")?;
    let available = read("MemAvailable:")?;
    Some(json!({"total_mb": total / 1024, "used_mb": total.checked_sub(available)? / 1024}))
}

#[cfg(target_os = "linux")]
fn storage() -> Option<Value> {
    let mut value = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(c"/".as_ptr(), value.as_mut_ptr()) } != 0 {
        return None;
    }
    let value = unsafe { value.assume_init() };
    let total = value.f_blocks.checked_mul(value.f_frsize)?;
    let available = value.f_bavail.checked_mul(value.f_frsize)?;
    Some(json!({"total_mb": total / (1024 * 1024),
        "used_mb": total.checked_sub(available)? / (1024 * 1024)}))
}

#[cfg(not(target_os = "linux"))]
fn storage() -> Option<Value> {
    None
}

fn resources() -> Result<Value, BrokerError> {
    static PREVIOUS: OnceLock<Mutex<Option<Sample>>> = OnceLock::new();
    let mut previous = PREVIOUS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| BrokerError::unavailable("Resource observation is unavailable"))?;
    let current = Sample {
        time: Instant::now(),
        cpu: fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|value| cpu_sample(&value)),
        network: fs::read_to_string("/proc/net/dev")
            .ok()
            .and_then(|value| network_sample(&value)),
    };
    let (cpu, down, up) = deltas(previous.as_ref(), &current);
    *previous = Some(current);
    Ok(json!({"summary": {
        "cpu_percent": cpu, "memory": memory(), "storage": storage(),
        "network_down_bps": down, "network_up_bps": up, "fallback": false,
    }}))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/observations.rs"
    ));
}
