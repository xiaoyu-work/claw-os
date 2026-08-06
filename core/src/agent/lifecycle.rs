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

#[cfg(test)]
use crate::session;
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
    use super::*;
    use std::env;

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_env::lock_env()
    }

    struct DataDirGuard {
        prev: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => env::set_var("COS_DATA_DIR", v),
                None => env::remove_var("COS_DATA_DIR"),
            }
        }
    }

    fn redirect_data_dir() -> DataDirGuard {
        let tmp = tempfile::tempdir().unwrap();
        let prev = env::var_os("COS_DATA_DIR");
        env::set_var("COS_DATA_DIR", tmp.path());
        DataDirGuard { prev, _tmp: tmp }
    }

    #[test]
    fn ls_returns_empty_when_no_sessions() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let v = ls(&[]).unwrap();
        assert_eq!(v["n"], 0);
        assert!(v["tasks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn ls_lists_all_sessions_with_status_and_lease() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let s1 = session::create("first").unwrap();
        let s2 = session::create("second").unwrap();
        // Hold a lease on s1 so we can confirm it shows up.
        let _g = session::try_acquire(&s1).unwrap();

        let v = ls(&[]).unwrap();
        assert_eq!(v["n"], 2);
        let tasks = v["tasks"].as_array().unwrap();
        let s1_row = tasks.iter().find(|r| r["id"] == s1.as_str()).unwrap();
        let s2_row = tasks.iter().find(|r| r["id"] == s2.as_str()).unwrap();
        assert_eq!(s1_row["purpose"], "first");
        assert!(s1_row["lease"].is_object(), "s1 has lease");
        assert!(s2_row["lease"].is_null(), "s2 has no lease");
    }

    #[test]
    fn show_returns_404_style_error_for_missing_sid() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        // Valid-looking sid that does not exist.
        let bogus = "ses_0000000000000_000000000000".to_string();
        let err = show(&[bogus]).unwrap_err();
        assert!(err.contains("read meta"), "got {err}");
    }

    #[test]
    fn show_summarises_turns_and_mutations() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("with content").unwrap();
        session::append_turn(&sid, session::Turn::text(session::TurnRole::User, "hi")).unwrap();
        session::record_mutation(
            &sid,
            session::MutationRecord::new(session::Mutation::FsRename {
                from: "/a".into(),
                to: "/b".into(),
            }),
        )
        .unwrap();

        let v = show(&[sid.as_str().into()]).unwrap();
        assert_eq!(v["turns"]["count"], 1);
        assert_eq!(v["mutations"]["count"], 1);
        assert_eq!(v["mutations"]["by_kind"]["fs.rename"], 1);
    }

    #[test]
    fn owner_filtered_lifecycle_hides_and_blocks_other_users() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let alice = session::create("alice").unwrap();
        let bob = session::create("bob").unwrap();
        session::update_meta(&alice, |meta| meta.owner_uid = Some(1001)).unwrap();
        session::update_meta(&bob, |meta| meta.owner_uid = Some(1002)).unwrap();

        let listed = ls_for_owner(&[], 1001).unwrap();
        assert_eq!(listed["n"], 1);
        assert_eq!(listed["tasks"][0]["id"], alice.as_str());
        let error = show_for_owner(&[bob.as_str().into()], 1001).unwrap_err();
        assert_eq!(error, "task not found");
        let error = stop_for_owner(&[bob.as_str().into()], 1001).unwrap_err();
        assert_eq!(error, "task not found");
        assert!(!stop_sentinel(&bob).exists());
    }

    #[test]
    fn stop_with_no_holder_marks_session_paused() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("idle").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Running).unwrap();

        let v = stop(&[sid.as_str().into()]).unwrap();
        assert_eq!(v["action"], "marked-paused");
        let meta = session::get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Paused);
    }

    #[test]
    fn stop_with_live_holder_only_writes_sentinel() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("running").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
        let _g = session::try_acquire(&sid).unwrap();

        let v = stop(&[sid.as_str().into()]).unwrap();
        assert_eq!(v["action"], "sentinel-written");
        // Meta status NOT flipped because someone is making progress.
        let meta = session::get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Running);
        assert!(stop_sentinel(&sid).exists());
    }

    #[test]
    fn undo_dry_run_lists_mutations_newest_first() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("undo dry").unwrap();
        for i in 0..3 {
            session::record_mutation(
                &sid,
                session::MutationRecord::new(session::Mutation::Opaque {
                    verb: format!("step.{i}"),
                    forward: json!({"i": i}),
                    inverse: json!({}),
                }),
            )
            .unwrap();
        }

        let v = undo(&[sid.as_str().into(), "--dry-run".into()]).unwrap();
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["n"], 3);
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries[0]["seq"], 2, "newest first");
        assert_eq!(entries[2]["seq"], 0);
    }

    #[test]
    fn resume_flips_paused_to_pending_and_clears_sentinel() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("paused task").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Paused).unwrap();
        // Pretend a previous stop wrote the sentinel.
        let s = stop_sentinel(&sid);
        std::fs::create_dir_all(s.parent().unwrap()).ok();
        std::fs::write(&s, b"x").unwrap();

        let v = resume(&[sid.as_str().into()]).unwrap();
        assert_eq!(v["status"], "pending");
        let meta = session::get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Pending);
        assert!(!s.exists(), "sentinel cleared");
    }

    #[test]
    fn resume_refuses_running_or_terminal_sessions() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("running").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
        let err = resume(&[sid.as_str().into()]).unwrap_err();
        assert!(err.contains("cannot resume"), "got {err}");
    }

    #[test]
    fn resume_refuses_when_lease_still_held() {
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("paused but locked").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Paused).unwrap();
        let _g = session::try_acquire(&sid).unwrap();
        let err = resume(&[sid.as_str().into()]).unwrap_err();
        assert!(err.contains("lease"), "got {err}");
    }

    #[test]
    fn stop_no_toctou() {
        // Regression: two concurrent `cos agent stop` calls on the
        // same session must serialize through the stop.lock flock.
        // Without it, both observe `lease.is_none() && active` from
        // their independent reads and both write a meta — one of
        // those updates is then silently lost. With the lock, the
        // second call sees Status::Paused inside the lock and
        // chooses the "sentinel-written" branch instead.
        let _l = lock_env();
        let _d = redirect_data_dir();
        let sid = session::create("toctou").unwrap();
        session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
        let sid_a = sid.clone();
        let sid_b = sid.clone();
        let h1 = std::thread::spawn(move || stop(&[sid_a.as_str().into()]).unwrap());
        let h2 = std::thread::spawn(move || stop(&[sid_b.as_str().into()]).unwrap());
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        // Exactly one caller observed the transition to Paused. The
        // other ran *after* the lock and so saw Paused already and
        // returned "sentinel-written".
        let actions: Vec<String> = [&r1, &r2]
            .iter()
            .map(|v| v["action"].as_str().unwrap_or("").to_string())
            .collect();
        let paused = actions
            .iter()
            .filter(|a| a.as_str() == "marked-paused")
            .count();
        let sentinels = actions
            .iter()
            .filter(|a| a.as_str() == "sentinel-written")
            .count();
        assert_eq!(
            paused, 1,
            "exactly one stop() should flip Paused, got actions={actions:?}"
        );
        assert_eq!(
            sentinels, 1,
            "the loser should report sentinel-written, got actions={actions:?}"
        );
        // Final state must be Paused regardless of ordering.
        let meta = session::get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Paused);
    }
}
