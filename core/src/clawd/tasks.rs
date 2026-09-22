use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::time::sleep;

use crate::agent::service::{load_activity, resolve_activity_id, Job, JobStatus, Store};
use crate::caps::Role;
use crate::session::{self, SessionClient, SessionOrigin, SessionSource};

use super::client_identity::ClientIdentity;

const SUBMISSION_PRESENCE_TTL_MS: u64 = 30_000;

#[derive(Clone, Copy)]
struct PendingPresence {
    owner_uid: u32,
    pid: u32,
    start_time_ticks: u64,
    expires_at_ms: u64,
}

fn presence_leases() -> &'static Mutex<HashMap<String, PendingPresence>> {
    static LEASES: OnceLock<Mutex<HashMap<String, PendingPresence>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn submit(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    if owner_uid == 0 {
        // The agent runtime executes in an unprivileged `claw-agentd`
        // worker owned by the submitter. Root has no account to drop to,
        // so a root-owned task is refused at submission rather than
        // silently running the model as root.
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.to_string());
    }
    let prompt = required_string(&params, "prompt")?;
    let requested_model = params
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    crate::agent::service::validate_requested_model(requested_model.as_deref())?;
    let context = params
        .get("context")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let branch_context = params
        .get("branch_context")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
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
    let use_memory = params
        .get("use_memory")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let requested_workspace = params.get("workspace").and_then(Value::as_str);
    let requested_activity = optional_activity_id(&params)?;
    let session_meta = session_id
        .as_deref()
        .map(|session_id| task_session_meta(session_id, owner_uid))
        .transpose()?;
    let activity_id = resolve_task_activity(
        owner_uid,
        session_meta.as_ref(),
        requested_activity.as_deref(),
    )?;
    // Activity admission precedes session creation, capability refresh, and
    // queue publication. Its planning fields never supply capabilities.
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let workspace = crate::agent::workspace::resolve(owner_uid, requested_workspace)?;
    let owner_gid = client
        .gid
        .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
    crate::storage::ensure_owner_agent_state_dir(owner_uid, owner_gid)
        .map_err(|err| format!("prepare owner agent state: {err}"))?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let session_client = SessionClient::new(SessionSource::BrokerTask, false, true);
    let session_id = match session_id {
        Some(session_id) => {
            prepare_task_session_with_client(&session_id, owner_uid, &owner_home, session_client)?;
            Some(session_id)
        }
        None => Some(create_task_session_with_client(
            &prompt,
            owner_uid,
            &owner_home,
            session_client,
        )?),
    };
    let mut job = Job::new_pending_with_client(
        prompt,
        context,
        branch_context,
        session_id,
        max_turns,
        Some(owner_uid),
        Some(owner_home.to_string_lossy().into_owned()),
        session_client,
    );
    job.use_memory = use_memory;
    job.activity_id = activity_id;
    job.requested_model = requested_model;
    job.workspace = Some(workspace.to_string_lossy().into_owned());
    let task_id = job.id.clone();
    let job = with_presence_publication(&task_id, client, unix_now_ms(), || store.publish(job))
        .map_err(|err| err.to_string())?;
    Ok(job_value(job))
}

pub fn workspace(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let owner_uid = client.require_uid()?;
    if owner_uid == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.to_string());
    }
    let requested = params.get("path").and_then(Value::as_str);
    let workspace = crate::agent::workspace::resolve(owner_uid, requested)?;
    Ok(json!({ "workspace": workspace.to_string_lossy() }))
}

fn create_task_session(
    prompt: &str,
    owner_uid: u32,
    owner_home: &std::path::Path,
) -> Result<String, String> {
    create_task_session_with_client(
        prompt,
        owner_uid,
        owner_home,
        SessionClient::new(SessionSource::BrokerTask, false, true),
    )
}

fn create_task_session_with_client(
    prompt: &str,
    owner_uid: u32,
    owner_home: &std::path::Path,
    client: SessionClient,
) -> Result<String, String> {
    let purpose = format!("agent task: {}", preview(prompt, 80));
    create_agent_session_with_client(purpose, owner_uid, owner_home, client, |_| Ok(()))
        .map(|(id, ())| id)
}

/// Issue an empty system-agent session without submitting or executing a job.
pub(super) fn create_agent_session_with<T>(
    purpose: String,
    owner_uid: u32,
    owner_home: &std::path::Path,
    initialize: impl FnOnce(&session::SessionId) -> Result<T, String>,
) -> Result<(String, T), String> {
    create_agent_session_with_client(
        purpose,
        owner_uid,
        owner_home,
        SessionClient::new(SessionSource::BrokerTask, false, true),
        initialize,
    )
}

fn create_agent_session_with_client<T>(
    purpose: String,
    owner_uid: u32,
    owner_home: &std::path::Path,
    client: SessionClient,
    initialize: impl FnOnce(&session::SessionId) -> Result<T, String>,
) -> Result<(String, T), String> {
    let sid = session::create(purpose).map_err(|err| err.to_string())?;
    let prepared = initialize(&sid).and_then(|value| {
        configure_task_session(&sid, owner_uid, owner_home, client)?;
        Ok(value)
    });
    match prepared {
        Ok(value) => Ok((sid.into_string(), value)),
        Err(error) => {
            session::end(&sid, session::Status::Failed)
                .map_err(|cleanup| format!("initialize agent session {sid}: {error}; {cleanup}"))?;
            Err(format!("initialize agent session {sid}: {error}"))
        }
    }
}

fn configure_task_session(
    sid: &session::SessionId,
    owner_uid: u32,
    owner_home: &std::path::Path,
    client: SessionClient,
) -> Result<(), String> {
    session::update_meta(sid, |meta| {
        meta.creator_runtime = Some("clawd".to_string());
        meta.role = Some(Role::Observer);
        meta.owner_uid = Some(owner_uid);
        meta.origin = Some(SessionOrigin::SystemAgentTask);
        meta.client = client;
    })
    .map_err(|err| err.to_string())?;
    let caps = super::system_caps::system_agent_caps(owner_uid, owner_home);
    session::set_caps(sid, &caps).map_err(|err| err.to_string())?;
    Ok(())
}

fn task_session_meta(session_id: &str, owner_uid: u32) -> Result<session::SessionMeta, String> {
    let sid = session_id
        .parse::<session::SessionId>()
        .map_err(|err| format!("invalid task session id: {err}"))?;
    let meta =
        session::get_meta(&sid).map_err(|_| format!("task session not found: {session_id}"))?;
    if meta.id != sid {
        return Err(format!(
            "task session metadata has a different id: {session_id}"
        ));
    }
    // The capabilities below are re-derived for `owner_uid`, and the
    // conversation history is read from that uid's memory database, so
    // the recorded owner has to be the same account — including for
    // root. Root peers keep an administrative *view* of every task
    // (see `owner_filter`), but resuming one means running as its
    // owner, and resuming somebody else's would rewrite their session
    // to a different account's policy.
    if meta.owner_uid != Some(owner_uid) {
        return Err(format!(
            "task session is not owned by uid {owner_uid}: {session_id}"
        ));
    }
    if meta.creator_runtime.as_deref() != Some("clawd") {
        return Err(format!("session is not a system-agent task: {session_id}"));
    }
    Ok(meta)
}

fn prepare_task_session(
    session_id: &str,
    owner_uid: u32,
    owner_home: &std::path::Path,
) -> Result<(), String> {
    prepare_task_session_with_client(
        session_id,
        owner_uid,
        owner_home,
        SessionClient::new(SessionSource::BrokerTask, false, true),
    )
}

fn prepare_task_session_with_client(
    session_id: &str,
    owner_uid: u32,
    owner_home: &std::path::Path,
    client: SessionClient,
) -> Result<(), String> {
    let sid = task_session_meta(session_id, owner_uid)?.id;
    let caps = super::system_caps::system_agent_caps(owner_uid, owner_home);
    session::set_caps(&sid, &caps).map_err(|err| format!("refresh task capabilities: {err}"))?;
    // A resumed task is ambient conversation, never an unattended
    // delegation: re-stamp the provenance so a session that acquired a
    // delegation marker cannot be replayed as one.
    session::update_meta(&sid, |meta| {
        meta.origin = Some(SessionOrigin::SystemAgentTask);
        meta.client = client;
    })
    .map_err(|err| format!("refresh task provenance: {err}"))
}

fn resolve_task_activity(
    owner_uid: u32,
    session_meta: Option<&session::SessionMeta>,
    requested: Option<&str>,
) -> Result<Option<String>, String> {
    let inherited = session_meta.and_then(|meta| meta.activity_id.as_deref());
    resolve_activity_id(owner_uid, inherited, requested).map_err(|error| error.to_string())
}

fn preview(value: &str, max: usize) -> String {
    let value = value.replace('\n', " ");
    if value.chars().count() <= max {
        value
    } else {
        format!("{}...", value.chars().take(max).collect::<String>())
    }
}

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let activity_id = optional_activity_id(&params)?;
    let activity_id = activity_id
        .as_deref()
        .map(|id| load_activity(client.require_uid()?, id).map_err(|error| error.to_string()))
        .transpose()?
        .map(|activity| activity.id);
    let owner_uid = owner_filter(client)?;
    let status = optional_status(&params)?;
    let limit = optional_limit(&params)?;
    let summary = params
        .get("summary")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let mut jobs = Vec::new();

    if let Some(activity_id) = activity_id.as_deref() {
        jobs = store
            .list_for_activity(
                client.require_uid()?,
                activity_id,
                if status.is_some() { usize::MAX } else { limit },
            )
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|job| status.is_none_or(|status| job.status == status))
            .take(limit)
            .map(job_value)
            .collect();
    } else {
        let buckets: &[JobStatus] = match status {
            Some(JobStatus::Pending) => &[JobStatus::Pending],
            Some(JobStatus::Running) => &[JobStatus::Running],
            Some(JobStatus::WaitingApproval) => &[JobStatus::WaitingApproval],
            Some(JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled) => &[JobStatus::Ok],
            None => &[
                JobStatus::Pending,
                JobStatus::Running,
                JobStatus::WaitingApproval,
                JobStatus::Ok,
            ],
        };
        for &bucket in buckets {
            collect_jobs(&store, bucket, status, limit, owner_uid, &mut jobs)?;
        }
    }

    jobs.sort_by(|left, right| {
        right
            .get("created_at")
            .and_then(Value::as_str)
            .cmp(&left.get("created_at").and_then(Value::as_str))
    });
    jobs.truncate(limit);
    if summary {
        jobs = jobs
            .into_iter()
            .map(|job| task_summary_value(&job))
            .collect();
    }

    Ok(json!({ "jobs": jobs }))
}

pub fn get(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let Some((_status, job)) = store
        .locate_for_owner(&id, owner_filter(client)?)
        .map_err(|err| err.to_string())?
    else {
        return Err(format!("task not found: {id}"));
    };
    Ok(job_value(job))
}

pub async fn result(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let owner_uid = owner_filter(client)?;
    let cursor = params
        .get("cursor")
        .and_then(Value::as_u64)
        .map(|value| usize::try_from(value).map_err(|_| format!("cursor is too large: {value}")))
        .transpose()?;
    if let Some(cursor) = cursor {
        return stream_events(id, cursor, &params, owner_uid).await;
    }

    let timeout_ms = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let store = Store::open_default().map_err(|err| err.to_string())?;

    loop {
        let Some((_status, job)) = store
            .locate_for_owner(&id, owner_uid)
            .map_err(|err| err.to_string())?
        else {
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

async fn stream_events(
    id: String,
    mut cursor: usize,
    params: &Value,
    owner_uid: Option<u32>,
) -> Result<Value, String> {
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let store = Store::open_default().map_err(|err| err.to_string())?;

    loop {
        let Some((_status, job)) = store
            .locate_for_owner(&id, owner_uid)
            .map_err(|err| err.to_string())?
        else {
            return Err(format!("task not found: {id}"));
        };
        let (next_cursor, events) = store
            .read_stream_events(&id, cursor)
            .map_err(|err| err.to_string())?;
        cursor = next_cursor;
        let terminal = matches!(
            job.status,
            JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
        );
        if !events.is_empty() || terminal || timeout_ms == 0 || Instant::now() >= deadline {
            return Ok(json!({
                "id": id,
                "cursor": cursor,
                "events": events,
                "job": job_value(job),
                "terminal": terminal,
            }));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

pub fn cancel(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let owner_uid = owner_filter(client)?;
    if let Some((job, immediate)) = store
        .request_cancel_for_owner(&id, owner_uid)
        .map_err(|err| err.to_string())?
    {
        drop_presence(&id);
        if !immediate {
            crate::agent::runtime::interrupt::signal(&id);
        }
        // Cancelling a task ends the authority its session carried.
        // Doing it here rather than only at teardown means a cancel
        // that races a tool call cannot leave the tool holding a live
        // grant while the task is already being reported cancelled.
        if let Some(session_id) = job.session_id.as_deref() {
            match job.owner_uid {
                Some(uid) => super::authority::revoke_session_for_owner(session_id, uid),
                None => super::authority::revoke_session(session_id),
            }
        }
        let mut value = job_value(job);
        value["cancelled"] = json!(immediate);
        value["cancel_requested"] = json!(!immediate);
        return Ok(value);
    }

    let Some((_status, job)) = store
        .locate_for_owner(&id, owner_uid)
        .map_err(|err| err.to_string())?
    else {
        return Err(format!("task not found: {id}"));
    };

    Ok(json!({
        "id": job.id,
        "status": job.status,
        "activity_id": job.activity_id,
        "cancelled": false,
        "reason": "task is already terminal",
    }))
}

pub fn retry(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let id = required_string(&params, "id")?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let Some((_bucket, original)) = store
        .locate_for_owner(&id, owner_filter(client)?)
        .map_err(|err| err.to_string())?
    else {
        return Err(format!("task not found: {id}"));
    };
    if !matches!(
        original.status,
        JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
    ) {
        return Err(format!(
            "task is not terminal and cannot be retried: {}",
            original.status.as_str()
        ));
    }
    crate::agent::service::validate_requested_model(original.requested_model.as_deref())?;
    let requested_model = original.requested_model.clone();
    let owner_uid = original
        .owner_uid
        .ok_or_else(|| "task has no recorded owner and cannot be retried".to_string())?;
    if owner_uid == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.to_string());
    }
    let session_meta = original
        .session_id
        .as_deref()
        .map(|session_id| task_session_meta(session_id, owner_uid))
        .transpose()?;
    let activity_id = resolve_task_activity(
        client.require_uid()?,
        session_meta.as_ref(),
        original.activity_id.as_deref(),
    )?;
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let session_client = SessionClient::new(SessionSource::BrokerTask, false, true);
    if let Some(session_id) = original.session_id.as_deref() {
        prepare_task_session_with_client(session_id, owner_uid, &owner_home, session_client)?;
    }
    let mut retried = Job::new_pending_with_client(
        original.prompt,
        original.context,
        original.branch_context,
        original.session_id,
        original.max_turns,
        Some(owner_uid),
        Some(owner_home.to_string_lossy().into_owned()),
        session_client,
    );
    retried.use_memory = original.use_memory;
    retried.activity_id = activity_id;
    retried.requested_model = requested_model;
    retried.workspace = original.workspace;
    let task_id = retried.id.clone();
    let retried =
        with_presence_publication(&task_id, client, unix_now_ms(), || store.publish(retried))
            .map_err(|err| err.to_string())?;
    Ok(job_value(retried))
}

pub fn counts(client: &ClientIdentity) -> Result<Value, String> {
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let owner_uid = owner_filter(client)?;
    let pending = store
        .list_bucket_for_owner(JobStatus::Pending, None, owner_uid)
        .map_err(|err| err.to_string())?
        .len();
    let running = store
        .list_bucket_for_owner(JobStatus::Running, None, owner_uid)
        .map_err(|err| err.to_string())?
        .len();
    let waiting_approval = store
        .list_bucket_for_owner(JobStatus::WaitingApproval, None, owner_uid)
        .map_err(|err| err.to_string())?
        .len();
    let done = store
        .list_bucket_for_owner(JobStatus::Ok, None, owner_uid)
        .map_err(|err| err.to_string())?;
    let done_total = done.len();
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
        "waiting_approval": waiting_approval,
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
    owner_uid: Option<u32>,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    for job in store
        .list_bucket_for_owner(bucket, Some(limit), owner_uid)
        .map_err(|err| err.to_string())?
    {
        if status_filter.is_some_and(|status| status != job.status) {
            continue;
        }
        out.push(job_value(job));
    }
    Ok(())
}

fn owner_filter(client: &ClientIdentity) -> Result<Option<u32>, String> {
    let uid = client.require_uid()?;
    Ok((uid != 0).then_some(uid))
}

fn with_presence_publication<T, E>(
    task_id: &str,
    client: &ClientIdentity,
    now_ms: u64,
    publish: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let mut leases = presence_leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    leases.retain(|_, lease| lease.expires_at_ms >= now_ms);
    let (Some(owner_uid), Some(pid), Some(start_time_ticks)) =
        (client.uid, client.pid, client.start_time_ticks)
    else {
        return publish();
    };
    if client.attended_local {
        leases.insert(
            task_id.to_string(),
            PendingPresence {
                owner_uid,
                pid,
                start_time_ticks,
                expires_at_ms: now_ms.saturating_add(SUBMISSION_PRESENCE_TTL_MS),
            },
        );
    }

    let result = publish();
    if result.is_err() {
        leases.remove(task_id);
    }
    result
}

pub(crate) fn claim_job_with_presence(
    store: &Store,
    execution_ttl: Duration,
) -> std::io::Result<Option<(Job, Option<crate::session::SessionPresence>)>> {
    claim_job_with_presence_at(
        store,
        unix_now_ms(),
        execution_ttl.as_millis() as u64,
        crate::proc::process_identity_is_live,
    )
}

fn claim_job_with_presence_at(
    store: &Store,
    now_ms: u64,
    execution_ttl_ms: u64,
    process_is_live: impl Fn(u32, u64, u32) -> bool,
) -> std::io::Result<Option<(Job, Option<crate::session::SessionPresence>)>> {
    let mut leases = presence_leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    leases.retain(|_, lease| lease.expires_at_ms >= now_ms);
    let Some(job) = store.claim_one()? else {
        return Ok(None);
    };
    let presence = leases.remove(&job.id).and_then(|pending| {
        (job.recovery_count == 0
            && job.owner_uid == Some(pending.owner_uid)
            && now_ms <= pending.expires_at_ms
            && process_is_live(pending.pid, pending.start_time_ticks, pending.owner_uid))
        .then_some(crate::session::SessionPresence {
            owner_uid: pending.owner_uid,
            pid: pending.pid,
            start_time_ticks: pending.start_time_ticks,
            expires_at_ms: now_ms.saturating_add(execution_ttl_ms),
        })
    });
    Ok(Some((job, presence)))
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn drop_presence(task_id: &str) {
    presence_leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(task_id);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/tasks.rs"
    ));
}

fn job_value(job: Job) -> Value {
    serde_json::to_value(job).unwrap_or_else(|err| {
        json!({
            "status": "error",
            "error": format!("failed to serialize job: {err}"),
        })
    })
}

fn task_summary_value(job: &Value) -> Value {
    json!({
        "id": job.get("id").cloned().unwrap_or(Value::Null),
        "title": job
            .get("prompt")
            .and_then(Value::as_str)
            .map(|prompt| preview(prompt, 80))
            .unwrap_or_else(|| "Agent task".to_string()),
        "status": job.get("status").cloned().unwrap_or(Value::Null),
        "created_at": job.get("created_at").cloned().unwrap_or(Value::Null),
        "started_at": job.get("started_at").cloned().unwrap_or(Value::Null),
        "finished_at": job.get("finished_at").cloned().unwrap_or(Value::Null),
        "session_id": job.get("session_id").cloned().unwrap_or(Value::Null),
        "activity_id": job.get("activity_id").cloned().unwrap_or(Value::Null),
        "workspace": job.get("workspace").cloned().unwrap_or(Value::Null),
        "waiting_on": job.get("waiting_on").cloned().unwrap_or_else(|| json!([])),
        "cancel_requested": job
            .get("cancel_requested_at")
            .is_some_and(|value| !value.is_null()),
        "error": job
            .get("error")
            .and_then(Value::as_str)
            .map(|error| preview(error, 512)),
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
        "waiting" | "waiting_approval" | "approval" => Ok(Some(JobStatus::WaitingApproval)),
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

fn optional_activity_id(params: &Value) -> Result<Option<String>, String> {
    match params.get("activity_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if !id.trim().is_empty() => Ok(Some(id.clone())),
        Some(_) => Err("activity_id must be a non-empty string".to_string()),
    }
}
