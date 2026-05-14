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
//!     denied/<id>.json
//! ```
//!
//! Both the CLI ([`crate::perms`]) and the GUI applet
//! (`desktop/applets/cosmic-applet-approval-gate`) read and write this
//! directory; rendering and notification are owned by them. This
//! module is just the storage + waiter layer.

use std::fs;
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
    /// Persisted across sessions until the user revokes it.
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

fn ensure_dirs() -> std::io::Result<()> {
    fs::create_dir_all(pending_dir())?;
    fs::create_dir_all(approved_dir())?;
    fs::create_dir_all(denied_dir())?;
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
    fs::write(&path, data).map_err(|e| e.to_string())?;
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
    if !pending.exists() {
        return Err(format!("no pending request with id `{id}`"));
    }
    let data = fs::read_to_string(&pending).map_err(|e| e.to_string())?;
    let request: Request = serde_json::from_str(&data).map_err(|e| e.to_string())?;
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
    fs::write(
        &dest,
        serde_json::to_string_pretty(&resolved).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::remove_file(&pending).map_err(|e| e.to_string())?;
    Ok(resolved)
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
    for dir in [approved_dir(), denied_dir()] {
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

    fn isolated_env() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("COS_DATA_DIR", tmp.path());
        tmp
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
        assert_eq!(GrantDuration::parse("Session"), Some(GrantDuration::Session));
        assert_eq!(GrantDuration::parse("FOREVER"), Some(GrantDuration::Forever));
        assert_eq!(GrantDuration::parse("nope"), None);
    }
}
