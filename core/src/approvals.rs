//! Approval queue — interactive consent for gated capability requests.
//!
//! When a gated operation is denied (or when an agent wants to
//! pre-emptively ask before attempting one), it writes a request to
//! `$COS_DATA_DIR/approvals/pending/<id>.json`. The user (via
//! `cos perms approve` / `cos perms deny`, or via the GUI applet)
//! moves the file to `approved/` or `denied/`. The requester polls
//! until the file moves or the deadline passes.
//!
//! Layout:
//!
//! ```text
//! $COS_DATA_DIR/approvals/
//!     pending/<id>.json    # full Request
//!     approved/<id>.json   # Request + Outcome
//!     consumed/<id>.json   # approved once-grants after use
//!     denied/<id>.json
//! ```
//!
//! Both the CLI ([`crate::perms`]) and the GUI applet
//! (`desktop/applets/cosmic-applet-approval-gate`) read and write this
//! directory; rendering and notification are owned by them. This
//! module is just the storage + waiter layer.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::caps::{Scope, Verb};

/// How long a grant lasts after the user approves it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantDuration {
    /// One-shot: the grant covers this single request and nothing more.
    Once,
    /// Lasts for the lifetime of the requesting session.
    Session,
    /// Persisted until the user revokes it; still bound to the approved request.
    Forever,
}

impl GrantDuration {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "once" | "one" | "1" => Some(Self::Once),
            "session" => Some(Self::Session),
            "forever" | "always" => Some(Self::Forever),
            _ => None,
        }
    }
}

/// What ends up in `pending/<id>.json` and gets carried forward
/// (unchanged) into `approved/<id>.json` or `denied/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub verb: String,
    pub scope: Scope,
    pub session: String,
    pub reason: String,
    pub requested_at: u64,
    /// Process that asked. Helps the user distinguish "the file
    /// manager I just opened" from "some background cron job."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester: Option<String>,
}

/// What the approver adds when they decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub outcome: Outcome,
    pub decided_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<GrantDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Approved,
    Denied,
}

/// Combined record persisted to `approved/` or `denied/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    #[serde(flatten)]
    pub request: Request,
    pub decision: Decision,
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

fn root() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("approvals")
}

fn pending_dir() -> PathBuf {
    root().join("pending")
}
fn approved_dir() -> PathBuf {
    root().join("approved")
}
fn denied_dir() -> PathBuf {
    root().join("denied")
}
fn consumed_dir() -> PathBuf {
    root().join("consumed")
}
/// Holding area for requests that have been claimed by a resolver
/// (atomically renamed out of `pending/`) but not yet written to
/// `approved/` or `denied/`. A crash between the claim and the final
/// write leaves an orphan here; that is acceptable because the
/// alternative is the request being silently lost or — worse — being
/// resolved by two callers simultaneously.
fn scratch_dir() -> PathBuf {
    root().join("scratch")
}

fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(pending_dir())?;
    fs::create_dir_all(approved_dir())?;
    fs::create_dir_all(denied_dir())?;
    fs::create_dir_all(consumed_dir())?;
    fs::create_dir_all(scratch_dir())?;
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn short_id() -> String {
    // 12 hex chars from a timestamp + entropy enough to avoid clashes
    // within the same session. We don't need cryptographic strength.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::process::id().hash(&mut h);
    format!("{:012x}", h.finish() & 0xFFFFFFFFFFFF)
}

/// Atomically write `data` to `path`. Writes go through a sibling
/// `.tmp.<nonce>` file, are fsynced, and are then renamed over the
/// final path. On Linux + POSIX this guarantees a reader sees either
/// the previous bytes or the new bytes — never a truncated payload.
/// The parent directory is also fsynced so the rename survives an
/// abrupt power loss on filesystems where directory metadata is not
/// auto-flushed (ext4 with `data=writeback`, xfs, …).
///
/// Why we need this: the approval queue is the trust boundary
/// between a gated agent action and the user's consent. A partial
/// write of `pending/<id>.json` followed by a process kill would
/// leave a non-parseable file; the read side filters parse errors
/// silently, so the user's request would just disappear. With the
/// tmp-write + rename pattern that scenario is impossible.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    fs::create_dir_all(parent)?;

    let leaf = path.file_name().and_then(|s| s.to_str()).unwrap_or("anon");
    // Hidden tmp name so partial writes never appear in directory
    // listings. The .tmp suffix + nonce keep concurrent writers from
    // racing on the same scratch path.
    let tmp_path = parent.join(format!(".{leaf}.tmp.{}", short_id()));

    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(data)?;
        // fsync the data + metadata of the tmp file before linking
        // it into place under the user-visible name.
        f.sync_all()?;
    }

    // Atomic rename is unconditional and overwrites the destination
    // on POSIX. After this call a reader sees the new bytes; before
    // it, the old bytes.
    if let Err(e) = fs::rename(&tmp_path, path) {
        // Best-effort cleanup — the tmp file is hidden, so leaving
        // it behind is at worst a tiny disk-space leak.
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Best-effort: fsync the parent directory so the rename
    // survives a crash. Not all filesystems require this but it
    // costs ~one syscall and makes the durability guarantee
    // unambiguous.
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

// ---------------------------------------------------------------------------
// Public API — write side
// ---------------------------------------------------------------------------

/// Submit a pending approval request. Returns the request id the
/// caller can use with [`wait`].
pub fn submit(
    verb: Verb,
    scope: Scope,
    session: impl Into<String>,
    reason: impl Into<String>,
    requester: Option<String>,
) -> Result<String, String> {
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    let req = Request {
        id: format!("ap-{}", short_id()),
        verb: verb.as_str().to_string(),
        scope,
        session: session.into(),
        reason: reason.into(),
        requested_at: now_secs(),
        requester,
    };
    let path = pending_dir().join(format!("{}.json", req.id));
    let data = serde_json::to_string_pretty(&req).map_err(|e| e.to_string())?;
    write_atomic(&path, data.as_bytes()).map_err(|e| format!("write pending: {e}"))?;
    Ok(req.id)
}

pub fn approve(
    id: &str,
    duration: GrantDuration,
    decided_by: Option<String>,
    note: Option<String>,
) -> Result<Resolved, String> {
    resolve(id, Outcome::Approved, Some(duration), decided_by, note)
}

pub fn deny(
    id: &str,
    decided_by: Option<String>,
    note: Option<String>,
) -> Result<Resolved, String> {
    resolve(id, Outcome::Denied, None, decided_by, note)
}

fn resolve(
    id: &str,
    outcome: Outcome,
    duration: Option<GrantDuration>,
    decided_by: Option<String>,
    note: Option<String>,
) -> Result<Resolved, String> {
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    let pending = pending_dir().join(format!("{id}.json"));

    // Atomically claim the request: rename `pending/<id>.json` out of
    // the pending directory into our process-private scratch path.
    // POSIX rename is atomic, so concurrent resolvers (CLI + GUI
    // applet, two reviewers, …) see exactly ONE winner — the one
    // whose rename succeeded. Everyone else gets ENOENT and a clean
    // "no pending request" error.
    //
    // The scratch path includes a per-resolver nonce so two simultaneous
    // resolvers do not race on the same destination either.
    let scratch = scratch_dir().join(format!("{id}.{}.json", short_id()));
    if let Some(parent) = scratch.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("scratch dir: {e}"))?;
    }
    match fs::rename(&pending, &scratch) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no pending request with id `{id}`"));
        }
        Err(e) => return Err(format!("claim pending {id}: {e}")),
    }

    // From here on we exclusively own `scratch`. A crash between this
    // point and the final write leaves an orphan file in scratch_dir
    // that can be inspected post-mortem; correctness is preserved
    // (the request is not in pending/, not in approved/, not in
    // denied/ — it is "in flight" and recoverable by hand).
    let data = match fs::read_to_string(&scratch) {
        Ok(s) => s,
        Err(e) => return Err(format!("read claimed {id}: {e}")),
    };
    let request: Request =
        serde_json::from_str(&data).map_err(|e| format!("parse claimed {id}: {e}"))?;

    let decision = Decision {
        outcome,
        decided_at: now_secs(),
        decided_by,
        duration,
        note,
    };
    let resolved = Resolved { request, decision };
    let dest_dir = match outcome {
        Outcome::Approved => approved_dir(),
        Outcome::Denied => denied_dir(),
    };
    let dest = dest_dir.join(format!("{id}.json"));
    let payload = serde_json::to_string_pretty(&resolved).map_err(|e| e.to_string())?;
    write_atomic(&dest, payload.as_bytes())
        .map_err(|e| format!("write {} {id}: {e}", outcome_dir_name(outcome)))?;

    // Best-effort cleanup of the scratch file. If this fails the
    // authoritative copy is already in approved/ or denied/ and the
    // scratch file is harmless.
    let _ = fs::remove_file(&scratch);

    Ok(resolved)
}

fn outcome_dir_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Approved => "approved",
        Outcome::Denied => "denied",
    }
}

// ---------------------------------------------------------------------------
// Public API — read side
// ---------------------------------------------------------------------------

pub fn list_pending() -> Vec<Request> {
    list_dir(&pending_dir())
        .into_iter()
        .filter_map(|p| {
            let data = fs::read_to_string(&p).ok()?;
            serde_json::from_str::<Request>(&data).ok()
        })
        .collect()
}

pub fn list_recent(limit: usize) -> Vec<Resolved> {
    let mut out = Vec::new();
    for dir in [approved_dir(), consumed_dir(), denied_dir()] {
        for p in list_dir(&dir) {
            if let Ok(data) = fs::read_to_string(&p) {
                if let Ok(r) = serde_json::from_str::<Resolved>(&data) {
                    out.push(r);
                }
            }
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.decision.decided_at));
    out.truncate(limit);
    out
}

pub fn lookup_pending(id: &str) -> Option<Request> {
    let p = pending_dir().join(format!("{id}.json"));
    let data = fs::read_to_string(&p).ok()?;
    serde_json::from_str(&data).ok()
}

/// Return an approved grant for `session`/`verb`/`scope`, consuming it
/// atomically when it was approved for `once`.
pub fn consume_matching_grant(
    session: &str,
    verb: Verb,
    requested_scope: &Scope,
) -> Result<Option<GrantDuration>, String> {
    ensure_dirs().map_err(|e| format!("approvals dir: {e}"))?;
    for path in list_dir(&approved_dir()) {
        let Ok(data) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(resolved) = serde_json::from_str::<Resolved>(&data) else {
            continue;
        };
        if resolved.request.session != session {
            continue;
        }
        if Verb::parse(&resolved.request.verb) != Some(verb) {
            continue;
        }
        if resolved.decision.outcome != Outcome::Approved {
            continue;
        }
        if !resolved.request.scope.covers(requested_scope) {
            continue;
        }

        let duration = resolved.decision.duration.unwrap_or(GrantDuration::Once);
        if duration != GrantDuration::Once {
            return Ok(Some(duration));
        }

        let Some(file_name) = path.file_name() else {
            continue;
        };
        let dest = consumed_dir().join(file_name);
        match fs::rename(&path, &dest) {
            Ok(()) => {
                sync_dir(&approved_dir());
                sync_dir(&consumed_dir());
                return Ok(Some(duration));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(format!("consume approved grant {}: {err}", path.display()));
            }
        }
    }
    Ok(None)
}

fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut paths: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|s| s == "json").unwrap_or(false))
        .collect();
    paths.sort();
    paths
}

/// Block-polling waiter. Used by requesters who want a synchronous
/// answer. Polls every 200 ms, gives up after `timeout`.
pub fn wait(id: &str, timeout: Duration) -> Result<Decision, String> {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(200);
    loop {
        let approved = approved_dir().join(format!("{id}.json"));
        let denied = denied_dir().join(format!("{id}.json"));
        if approved.exists() {
            let data = fs::read_to_string(&approved).map_err(|e| e.to_string())?;
            let r: Resolved = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            return Ok(r.decision);
        }
        if denied.exists() {
            let data = fs::read_to_string(&denied).map_err(|e| e.to_string())?;
            let r: Resolved = serde_json::from_str(&data).map_err(|e| e.to_string())?;
            return Ok(r.decision);
        }
        if Instant::now() >= deadline {
            return Err("approval request timed out".into());
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct IsolatedEnv {
        _lock: MutexGuard<'static, ()>,
        prev_data_dir: Option<OsString>,
        _tmp: tempfile::TempDir,
    }

    impl Drop for IsolatedEnv {
        fn drop(&mut self) {
            match self.prev_data_dir.take() {
                Some(value) => std::env::set_var("COS_DATA_DIR", value),
                None => std::env::remove_var("COS_DATA_DIR"),
            }
        }
    }

    fn isolated_env() -> IsolatedEnv {
        let lock = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_data_dir = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", tmp.path());
        IsolatedEnv {
            _lock: lock,
            prev_data_dir,
            _tmp: tmp,
        }
    }

    #[test]
    fn submit_then_approve_writes_to_approved_dir() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::FS_WRITE,
            Scope::path("/tmp/foo"),
            "sess-a",
            "want to write hosts file",
            None,
        )
        .unwrap();
        assert!(pending_dir().join(format!("{id}.json")).exists());
        let resolved = approve(&id, GrantDuration::Once, None, None).unwrap();
        assert_eq!(resolved.decision.outcome, Outcome::Approved);
        assert!(!pending_dir().join(format!("{id}.json")).exists());
        assert!(approved_dir().join(format!("{id}.json")).exists());
    }

    #[test]
    fn approved_once_grant_is_consumed() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::FS_WRITE,
            Scope::path("/tmp/approved/**"),
            "sess-a",
            "write requested",
            None,
        )
        .unwrap();
        approve(&id, GrantDuration::Once, None, None).unwrap();

        let first = consume_matching_grant(
            "sess-a",
            Verb::FS_WRITE,
            &Scope::path("/tmp/approved/file.txt"),
        )
        .unwrap();
        assert_eq!(first, Some(GrantDuration::Once));
        assert!(!approved_dir().join(format!("{id}.json")).exists());
        assert!(consumed_dir().join(format!("{id}.json")).exists());

        let second = consume_matching_grant(
            "sess-a",
            Verb::FS_WRITE,
            &Scope::path("/tmp/approved/file.txt"),
        )
        .unwrap();
        assert_eq!(second, None);

        let recent = list_recent(10);
        assert!(recent.iter().any(|resolved| resolved.request.id == id));
    }

    #[test]
    fn approved_session_grant_is_reusable() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::SYS_PACKAGE,
            Scope::name("git"),
            "sess-a",
            "install git",
            None,
        )
        .unwrap();
        approve(&id, GrantDuration::Session, None, None).unwrap();

        for _ in 0..2 {
            let grant =
                consume_matching_grant("sess-a", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap();
            assert_eq!(grant, Some(GrantDuration::Session));
        }
        assert!(approved_dir().join(format!("{id}.json")).exists());
    }

    #[test]
    fn deny_moves_to_denied_dir() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::FS_DELETE,
            Scope::Wild,
            "sess-b",
            "trying to wipe",
            None,
        )
        .unwrap();
        let resolved = deny(&id, Some("operator".into()), None).unwrap();
        assert_eq!(resolved.decision.outcome, Outcome::Denied);
        assert!(denied_dir().join(format!("{id}.json")).exists());
    }

    #[test]
    fn list_pending_returns_submitted_requests() {
        let _tmp = isolated_env();
        let id1 = submit(Verb::FS_READ, Scope::path("/a"), "s", "r", None).unwrap();
        let id2 = submit(Verb::FS_WRITE, Scope::path("/b"), "s", "r", None).unwrap();
        let pending = list_pending();
        let ids: Vec<&str> = pending.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
    }

    #[test]
    fn grant_duration_parse() {
        assert_eq!(GrantDuration::parse("once"), Some(GrantDuration::Once));
        assert_eq!(
            GrantDuration::parse("Session"),
            Some(GrantDuration::Session)
        );
        assert_eq!(
            GrantDuration::parse("FOREVER"),
            Some(GrantDuration::Forever)
        );
        assert_eq!(GrantDuration::parse("nope"), None);
    }

    /// `submit` must never leave a partially-written file behind. We
    /// can't easily simulate a process kill, but we can assert (a)
    /// the temp file is gone after `submit` returns and (b) the
    /// resulting pending/<id>.json parses cleanly.
    #[test]
    fn submit_writes_atomically_no_tmp_leftovers() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::FS_WRITE,
            Scope::path("/etc/hosts"),
            "sess",
            "want to edit hosts",
            None,
        )
        .unwrap();
        let path = pending_dir().join(format!("{id}.json"));
        let parsed: Request = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.id, id);

        // No hidden `.<id>.json.tmp.*` siblings should remain.
        for e in fs::read_dir(pending_dir()).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains(".tmp."),
                "leftover tmp file in pending/: {name}"
            );
        }
    }

    /// Two concurrent resolvers on the same request id (e.g. CLI
    /// approve racing the GUI applet's deny) must NOT both succeed.
    /// Exactly one wins; the other gets "no pending request".
    /// Before the rename-claim fix this race could leave the same id
    /// in BOTH approved/ and denied/.
    #[test]
    fn concurrent_approve_and_deny_only_one_wins() {
        let _tmp = isolated_env();
        let id = submit(
            Verb::FS_WRITE,
            Scope::path("/race"),
            "sess",
            "race target",
            None,
        )
        .unwrap();

        // Run approve and deny on background threads. Whichever
        // rename-claims first writes its outcome and the other has
        // to fail. We don't care which side wins; we care that
        // exactly one side is recorded.
        let id_a = id.clone();
        let id_b = id.clone();
        let h_a = std::thread::spawn(move || approve(&id_a, GrantDuration::Once, None, None));
        let h_b = std::thread::spawn(move || deny(&id_b, None, None));
        let r_a = h_a.join().unwrap();
        let r_b = h_b.join().unwrap();

        let approved_exists = approved_dir().join(format!("{id}.json")).exists();
        let denied_exists = denied_dir().join(format!("{id}.json")).exists();
        let pending_exists = pending_dir().join(format!("{id}.json")).exists();

        assert!(
            !pending_exists,
            "pending file must be gone after either resolver wins"
        );
        assert_ne!(
            approved_exists, denied_exists,
            "exactly one of approved/ or denied/ must exist (got approved={approved_exists}, denied={denied_exists})"
        );
        // Exactly one of the two calls succeeded.
        assert_eq!(
            r_a.is_ok() ^ r_b.is_ok(),
            true,
            "exactly one resolver should have succeeded; got approve={:?}, deny={:?}",
            r_a,
            r_b
        );
        let loser_err = if r_a.is_err() {
            r_a.unwrap_err()
        } else {
            r_b.unwrap_err()
        };
        assert!(
            loser_err.contains("no pending request"),
            "loser should see 'no pending request', got: {loser_err}"
        );
    }

    /// Resolving the same id twice in serial (legitimate retry, not a
    /// race) must error the second time with a clear message — not
    /// crash, not double-write.
    #[test]
    fn second_resolve_after_approve_errors_cleanly() {
        let _tmp = isolated_env();
        let id = submit(Verb::FS_READ, Scope::path("/x"), "s", "r", None).unwrap();
        approve(&id, GrantDuration::Once, None, None).unwrap();
        let err = deny(&id, None, None).unwrap_err();
        assert!(
            err.contains("no pending request"),
            "expected 'no pending request', got: {err}"
        );
        // Approve outcome is preserved; no denied/<id>.json appears.
        assert!(approved_dir().join(format!("{id}.json")).exists());
        assert!(!denied_dir().join(format!("{id}.json")).exists());
    }

    /// Approval queue should survive a power-loss simulation: if a
    /// pending file's tmp sibling appears mid-write, the read side
    /// must NOT mistake it for a real pending request.
    #[test]
    fn list_pending_ignores_tmp_files() {
        let _tmp = isolated_env();
        fs::create_dir_all(pending_dir()).unwrap();
        // Simulate an in-flight atomic write: hidden tmp file with
        // a `.tmp.` infix. list_dir already filters by `.json`
        // extension, but the leading `.` and the `.tmp.` infix
        // double-protect us.
        fs::write(
            pending_dir().join(".ap-xyz.json.tmp.abc"),
            r#"not real json"#,
        )
        .unwrap();
        let pending = list_pending();
        assert!(
            pending.is_empty(),
            "should ignore tmp file, got {pending:?}"
        );
    }
}
