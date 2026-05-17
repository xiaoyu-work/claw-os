use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::time::sleep;

use crate::agent::service::{Job, JobStatus, Store};

pub async fn submit(params: Value) -> Result<Value, String> {
    let prompt = required_string(&params, "prompt")?;
    let session_id = params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let max_turns = params
        .get("max_turns")
        .and_then(Value::as_u64)
        .map(|value| u32::try_from(value).map_err(|_| format!("max_turns is too large: {value}")))
        .transpose()?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let job = store
        .submit(prompt, session_id, max_turns)
        .map_err(|err| err.to_string())?;
    Ok(job_value(job))
}

pub fn list(params: Value) -> Result<Value, String> {
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let status = optional_status(&params)?;
    let limit = optional_limit(&params)?;
    let mut jobs = Vec::new();

    match status {
        Some(JobStatus::Pending) => {
            collect_jobs(&store, JobStatus::Pending, status, limit, &mut jobs)?
        }
        Some(JobStatus::Running) => {
            collect_jobs(&store, JobStatus::Running, status, limit, &mut jobs)?
        }
        Some(JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled) => {
            collect_jobs(&store, JobStatus::Ok, status, limit, &mut jobs)?
        }
        None => {
            collect_jobs(&store, JobStatus::Pending, None, limit, &mut jobs)?;
            collect_jobs(&store, JobStatus::Running, None, limit, &mut jobs)?;
            collect_jobs(&store, JobStatus::Ok, None, limit, &mut jobs)?;
        }
    }

    if jobs.len() > limit {
        jobs.truncate(limit);
    }

    Ok(json!({ "jobs": jobs }))
}

pub fn get(params: Value) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let Some((_status, job)) = store.locate(&id).map_err(|err| err.to_string())? else {
        return Err(format!("task not found: {id}"));
    };
    Ok(job_value(job))
}

pub async fn result(params: Value) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let store = Store::open_default().map_err(|err| err.to_string())?;

    loop {
        let Some((_status, job)) = store.locate(&id).map_err(|err| err.to_string())? else {
            return Err(format!("task not found: {id}"));
        };
        if matches!(
            job.status,
            JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
        ) || timeout_ms == 0
        {
            return Ok(job_value(job));
        }

        if Instant::now() >= deadline {
            return Ok(job_value(job));
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub fn cancel(params: Value) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    if let Some(job) = store.cancel_pending(&id).map_err(|err| err.to_string())? {
        return Ok(job_value(job));
    }

    let Some((_status, job)) = store.locate(&id).map_err(|err| err.to_string())? else {
        return Err(format!("task not found: {id}"));
    };

    Ok(json!({
        "id": job.id,
        "status": job.status,
        "cancelled": false,
        "reason": "only pending tasks can be cancelled",
    }))
}

pub fn counts() -> Result<Value, String> {
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let (pending, running, done_total) = store.counts().map_err(|err| err.to_string())?;
    let done = store
        .list_bucket(JobStatus::Ok, None)
        .map_err(|err| err.to_string())?;
    let ok = done
        .iter()
        .filter(|job| job.status == JobStatus::Ok)
        .count();
    let error = done
        .iter()
        .filter(|job| job.status == JobStatus::Error)
        .count();
    let cancelled = done
        .iter()
        .filter(|job| job.status == JobStatus::Cancelled)
        .count();

    Ok(json!({
        "pending": pending,
        "running": running,
        "done": done_total,
        "ok": ok,
        "error": error,
        "cancelled": cancelled,
    }))
}

fn collect_jobs(
    store: &Store,
    bucket: JobStatus,
    status_filter: Option<JobStatus>,
    limit: usize,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    for job in store
        .list_bucket(bucket, Some(limit))
        .map_err(|err| err.to_string())?
    {
        if status_filter.is_some_and(|status| status != job.status) {
            continue;
        }
        out.push(job_value(job));
    }
    Ok(())
}

fn job_value(job: Job) -> Value {
    serde_json::to_value(job).unwrap_or_else(|err| {
        json!({
            "status": "error",
            "error": format!("failed to serialize job: {err}"),
        })
    })
}

fn optional_limit(params: &Value) -> Result<usize, String> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|limit| {
            usize::try_from(limit)
                .map_err(|_| format!("limit is too large for this platform: {limit}"))
        })
        .transpose()
        .map(|limit| limit.unwrap_or(100))
}

fn optional_status(params: &Value) -> Result<Option<JobStatus>, String> {
    let Some(raw) = params.get("status").and_then(Value::as_str) else {
        return Ok(None);
    };

    match raw.trim().to_ascii_lowercase().as_str() {
        "pending" => Ok(Some(JobStatus::Pending)),
        "running" => Ok(Some(JobStatus::Running)),
        "ok" | "done" | "success" => Ok(Some(JobStatus::Ok)),
        "error" | "failed" => Ok(Some(JobStatus::Error)),
        "cancelled" | "canceled" => Ok(Some(JobStatus::Cancelled)),
        other => Err(format!("unknown task status: {other}")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing required string parameter: {key}"))
}
