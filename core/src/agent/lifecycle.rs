//! Durable-session lifecycle CLI verbs.
//!
//! These wrap [`crate::session`] for the user-facing `cos agent`
//! command surface. The point of Phase 5 is that the word
//! "session" never appears in `cos --help`; users think in terms of
//! "agent tasks". So the verbs here are:
//!
//! - `cos agent ls` — list every durable session on disk
//! - `cos agent show <sid>` — full detail for one session
//! - `cos agent stop <sid>` — politely tell the holder to wind down
//! - `cos agent undo <sid>` — replay the inverse mutation log
//! - `cos agent resume <sid>` — flip a paused session back to
//!   pending so a fresh runtime can attach
//!
//! Stop / resume are deliberately cooperative. We do not race against
//! a live runtime by yanking its lease — the lease holder is the
//! source of truth for "am I making progress". `stop` writes a
//! sentinel file the runtime is expected to poll, and falls back to
//! flipping meta.status when no holder exists. `resume` only handles
//! the `Paused -> Pending` transition; actually re-spawning the
//! runtime is the agent stack's job (it can decide which provider /
//! engine to use).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;

use serde_json::{json, Value};

use crate::session::{
    current_lease, get_meta, iter_mutations, iter_turns, list as list_sessions, rollback,
    session_dir, RollbackStatus, SessionId, Status,
};

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

pub fn ls(_args: &[String]) -> Result<Value, String> {
    ls_impl(None)
}

pub fn ls_for_owner(_args: &[String], owner_uid: u32) -> Result<Value, String> {
    ls_impl(Some(owner_uid))
}

fn ls_impl(owner_uid: Option<u32>) -> Result<Value, String> {
    let metas = list_sessions().map_err(|e| format!("list sessions: {e}"))?;
    let mut rows: Vec<Value> = Vec::with_capacity(metas.len());
    for m in metas
        .iter()
        .filter(|meta| session_visible_to(meta, owner_uid))
    {
        let lease = current_lease(&m.id).ok().flatten();
        rows.push(json!({
            "id": m.id.as_str(),
            "purpose": m.purpose,
            "status": m.status,
            "creator_runtime": m.creator_runtime,
            "created_at": m.created_at,
            "ended_at": m.ended_at,
            "lease": lease.as_ref().map(|l| json!({
                "pid": l.pid,
                "runtime": l.runtime,
                "started_at": l.started_at,
                "heartbeat_at": l.heartbeat_at,
            })).unwrap_or(Value::Null),
        }));
    }
    // Newest first — meta.created_at is RFC 3339, lexicographic sort works.
    rows.sort_by(|a, b| {
        b.get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(a.get("created_at").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(json!({
        "n": rows.len(),
        "tasks": rows,
    }))
}

pub fn show(args: &[String]) -> Result<Value, String> {
    show_impl(args, None)
}

pub fn show_for_owner(args: &[String], owner_uid: u32) -> Result<Value, String> {
    show_impl(args, Some(owner_uid))
}

fn show_impl(args: &[String], owner_uid: Option<u32>) -> Result<Value, String> {
    let sid = parse_sid(args, "show")?;
    let meta = get_owned_meta(&sid, owner_uid)?;
    let lease = current_lease(&sid).ok().flatten();
    let turns = iter_turns(&sid).map_err(|e| format!("read turns: {e}"))?;
    let muts = iter_mutations(&sid).map_err(|e| format!("read mutations: {e}"))?;

    // Aggregate mutation kinds into a {kind: count} table so the user
    // can see "this session deleted 3 files and renamed 1" at a glance
    // without paging through the full log.
    let mut by_kind: BTreeMap<&'static str, u64> = BTreeMap::new();
    for rec in &muts {
        let kind = match &rec.mutation {
            crate::session::Mutation::FsWrite { .. } => "fs.write",
            crate::session::Mutation::FsDelete { .. } => "fs.delete",
            crate::session::Mutation::FsRename { .. } => "fs.rename",
            crate::session::Mutation::CredentialStore { .. } => "credential.store",
            crate::session::Mutation::CredentialRevoke { .. } => "credential.revoke",
            crate::session::Mutation::SystemService { .. } => "sys.service",
            crate::session::Mutation::SystemPackage { .. } => "sys.package",
            crate::session::Mutation::Opaque { .. } => "opaque",
        };
        *by_kind.entry(kind).or_insert(0) += 1;
    }

    let by_kind_json: serde_json::Map<String, Value> = by_kind
        .into_iter()
        .map(|(k, v)| (k.to_string(), json!(v)))
        .collect();

    Ok(json!({
        "id": meta.id.as_str(),
        "purpose": meta.purpose,
        "status": meta.status,
        "role": meta.role,
        "parent_session": meta.parent_session,
        "creator_runtime": meta.creator_runtime,
        "budget": meta.budget,
        "created_at": meta.created_at,
        "ended_at": meta.ended_at,
        "lease": lease.as_ref().map(|l| json!({
            "pid": l.pid,
            "runtime": l.runtime,
            "started_at": l.started_at,
            "heartbeat_at": l.heartbeat_at,
        })).unwrap_or(Value::Null),
        "turns": json!({
            "count": turns.len(),
            "first_at": turns.first().map(|t| t.at.clone()),
            "last_at": turns.last().map(|t| t.at.clone()),
        }),
        "mutations": json!({
            "count": muts.len(),
            "by_kind": by_kind_json,
        }),
        "stop_requested": stop_sentinel(&sid).exists(),
    }))
}

pub fn stop(args: &[String]) -> Result<Value, String> {
    stop_impl(args, None)
}

pub fn stop_for_owner(args: &[String], owner_uid: u32) -> Result<Value, String> {
    stop_impl(args, Some(owner_uid))
}

fn stop_impl(args: &[String], owner_uid: Option<u32>) -> Result<Value, String> {
    let sid = parse_sid(args, "stop")?;
    // Validate that the session exists before any side effects.
    let _initial_meta = get_owned_meta(&sid, owner_uid)?;

    // Cooperative: drop a sentinel file the runtime is expected to
    // notice on its next heartbeat. We never yank a held lease.
    let sentinel = stop_sentinel(&sid);
    if let Some(parent) = sentinel.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let mut f =
        fs::File::create(&sentinel).map_err(|e| format!("write {}: {e}", sentinel.display()))?;
    // Body is just an audit string; presence is the actual signal.
    let _ = writeln!(
        f,
        r#"{{"requested_at":"{}","by_pid":{}}}"#,
        now_rfc3339(),
        std::process::id()
    );

    // Hold a flock spanning the `current_lease` check and the
    // `update_meta(Paused)` rewrite below. Without this lock, two
    // concurrent `cos agent stop` calls (or a `stop` racing a
    // `resume`) could both observe "no lease, status active" and
    // both write a meta — the later writer's status wins for the
    // wrong reason. The sentinel is sibling to the session dir so
    // the flock attaches to a stable inode even if meta.json is
    // rewritten via tmp+rename underneath us.
    let _lock = StopLock::acquire(&sid)?;
    // Re-read meta and lease *after* taking the lock to avoid using
    // stale TOCTOU snapshots from before the gate.
    let meta = get_owned_meta(&sid, owner_uid)?;
    let mut action = "sentinel-written";
    let lease = current_lease(&sid).ok().flatten();

    // No live holder + the session is still active — flip the meta
    // ourselves so `cos agent ls` shows it as Paused immediately.
    if lease.is_none() && meta.status.is_active() && meta.status != Status::Paused {
        crate::session::update_meta(&sid, |m| {
            m.status = Status::Paused;
        })
        .map_err(|e| format!("update meta: {e}"))?;
        action = "marked-paused";
    }

    Ok(json!({
        "id": sid.as_str(),
        "action": action,
        "lease_holder": lease.as_ref().map(|l| json!({"pid": l.pid, "runtime": l.runtime})).unwrap_or(Value::Null),
        "hint": if lease.is_some() {
            "stop signaled; the runtime will pause on its next heartbeat. \
             To force-evict, use the OS — e.g. send SIGTERM to the holder pid."
        } else {
            "no live runtime; status set to paused"
        },
    }))
}

pub fn undo(args: &[String]) -> Result<Value, String> {
    undo_impl(args, None)
}

pub fn undo_for_owner(args: &[String], owner_uid: u32) -> Result<Value, String> {
    undo_impl(args, Some(owner_uid))
}

fn undo_impl(args: &[String], owner_uid: Option<u32>) -> Result<Value, String> {
    let mut dry_run = false;
    let mut rest: Vec<String> = Vec::new();
    for a in args {
        if a == "--dry-run" {
            dry_run = true;
        } else {
            rest.push(a.clone());
        }
    }
    let sid = parse_sid(&rest, "undo")?;
    let _meta = get_owned_meta(&sid, owner_uid)?;

    let muts = iter_mutations(&sid).map_err(|e| format!("read mutations: {e}"))?;
    if dry_run {
        let entries: Vec<Value> = muts
            .iter()
            .rev()
            .map(|rec| {
                json!({
                    "seq": rec.seq,
                    "mutation": &rec.mutation,
                })
            })
            .collect();
        return Ok(json!({
            "id": sid.as_str(),
            "dry_run": true,
            "n": entries.len(),
            "entries": entries,
        }));
    }

    let outcomes = rollback(&sid).map_err(|e| format!("rollback: {e}"))?;
    let entries: Vec<Value> = outcomes
        .iter()
        .map(|o| {
            json!({
                "seq": o.seq,
                "verb": o.verb,
                "status": o.status,
                "detail": o.detail,
                "ok": matches!(
                    o.status,
                    RollbackStatus::Restored | RollbackStatus::AlreadyDone
                ),
            })
        })
        .collect();
    let failed = entries
        .iter()
        .filter(|e| !e["ok"].as_bool().unwrap_or(true))
        .count();
    Ok(json!({
        "id": sid.as_str(),
        "dry_run": false,
        "n": entries.len(),
        "failed": failed,
        "entries": entries,
    }))
}

pub fn resume(args: &[String]) -> Result<Value, String> {
    resume_impl(args, None)
}

pub fn resume_for_owner(args: &[String], owner_uid: u32) -> Result<Value, String> {
    resume_impl(args, Some(owner_uid))
}

fn resume_impl(args: &[String], owner_uid: Option<u32>) -> Result<Value, String> {
    let sid = parse_sid(args, "resume")?;
    let meta = get_owned_meta(&sid, owner_uid)?;

    if meta.status != Status::Paused {
        return Err(format!(
            "cannot resume from status {:?}; only paused tasks can be resumed",
            meta.status
        ));
    }

    if let Some(l) = current_lease(&sid).ok().flatten() {
        return Err(format!(
            "task is paused but a lease is still held by pid {}; wait for it to release",
            l.pid
        ));
    }

    // Flip back to Pending so the next runtime that calls
    // session::resume() can attach. We deliberately do NOT take the
    // lease here — the CLI is not the runtime, it can't make progress
    // on the agent loop, so taking the lease would be misleading.
    crate::session::update_meta(&sid, |m| {
        m.status = Status::Pending;
    })
    .map_err(|e| format!("update meta: {e}"))?;

    // Clear any stale stop sentinel — the user is asking us to keep
    // going, so the previous "please pause" is moot.
    let sentinel = stop_sentinel(&sid);
    if sentinel.exists() {
        let _ = fs::remove_file(&sentinel);
    }

    Ok(json!({
        "id": sid.as_str(),
        "status": Status::Pending,
        "hint": "task is ready for re-attachment by a runtime (e.g. `cos agent chat --session <id>`)",
    }))
}

fn session_visible_to(meta: &crate::session::SessionMeta, owner_uid: Option<u32>) -> bool {
    match owner_uid {
        None => true,
        Some(uid) => meta.owner_uid == Some(uid),
    }
}

fn require_session_owner(
    meta: &crate::session::SessionMeta,
    owner_uid: Option<u32>,
) -> Result<(), String> {
    if session_visible_to(meta, owner_uid) {
        Ok(())
    } else {
        Err("task not found".to_string())
    }
}

fn get_owned_meta(
    sid: &SessionId,
    owner_uid: Option<u32>,
) -> Result<crate::session::SessionMeta, String> {
    match get_meta(sid) {
        Ok(meta) => {
            require_session_owner(&meta, owner_uid)?;
            Ok(meta)
        }
        Err(_) if owner_uid.is_some() => Err("task not found".to_string()),
        Err(error) => Err(format!("read meta: {error}")),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn parse_sid(args: &[String], verb: &str) -> Result<SessionId, String> {
    let raw = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or_else(|| format!("usage: cos agent {verb} <task-id>  (or --session <id>)"))?;
    raw.parse()
        .map_err(|e: crate::session::InvalidSessionId| e.to_string())
}

fn stop_sentinel(sid: &SessionId) -> std::path::PathBuf {
    session_dir(sid).join("stop.requested")
}

/// Sibling sentinel that serializes the `current_lease` + `update_meta`
/// pair inside `stop`. Without it, two callers can both observe an
/// active+unleased meta and both write a meta — a transition is then
/// effectively lost. Living next to the session dir means it survives
/// the meta.json tmp+rename inode swap.
fn stop_lock_path(sid: &SessionId) -> std::path::PathBuf {
    session_dir(sid).join("stop.lock")
}

struct StopLock {
    file: fs::File,
}

impl StopLock {
    fn acquire(sid: &SessionId) -> Result<Self, String> {
        let path = stop_lock_path(sid);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| format!("open lock {}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "flock LOCK_EX {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(Self { file: f })
    }
}

impl Drop for StopLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/lifecycle.rs"
    ));
}
