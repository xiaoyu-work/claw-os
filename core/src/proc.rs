/// Agent-aware process session manager.
///
/// Tracks processes by session ID with persistent registry,
/// output buffering with caps, and queryable status.
/// Registry is stored on disk so sessions survive cos restarts.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::caps::{require_or_json, Scope, Verb};

tokio::task_local! {
    static SESSION_OVERRIDE: String;
    static TRUSTED_SESSION_OVERRIDE: SessionInfo;
}

pub async fn with_session_override<F, R>(session_id: String, future: F) -> R
where
    F: std::future::Future<Output = R>,
{
    SESSION_OVERRIDE.scope(session_id, future).await
}

pub async fn with_trusted_session_override<F, R>(session: SessionInfo, future: F) -> R
where
    F: std::future::Future<Output = R>,
{
    TRUSTED_SESSION_OVERRIDE.scope(session, future).await
}

pub fn current_session_id() -> Option<String> {
    TRUSTED_SESSION_OVERRIDE
        .try_with(|session| session.session_id.clone())
        .ok()
        .or_else(|| {
            SESSION_OVERRIDE
                .try_with(|value| value.clone())
                .ok()
        })
        .or_else(|| {
            std::env::var("COS_SESSION")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .or_else(bound_app_session_id)
        .filter(|value| !value.is_empty())
}

const MAX_OUTPUT_BYTES: usize = 2_000_000;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub pid: u32,
    pub command: Vec<String>,
    pub started_at: String,
    pub stdout_path: String,
    pub stderr_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// Capability set the session may exercise. Populated by `--role`
    /// or `--caps` on `cos proc spawn`. The kernel caps gate (see
    /// `caps::require`) consults this field to authorise gated
    /// operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caps: Option<crate::caps::CapSet>,
    /// Call-scoped capabilities temporarily installed for a serialized
    /// App MCP request. Cleared when the request finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transient_caps: Option<crate::caps::CapSet>,
    /// Role label used to generate `caps`, kept for audit / display.
    /// Has no enforcement effect — `caps` is the source of truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Kernel-attested App identity for child App sessions. `None` for
    /// ordinary user, agent and system sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// True while the launcher has registered the App identity but has
    /// not yet bound it to the spawned process.
    #[serde(default)]
    pub pending_bind: bool,
    /// Linux kernel clock ticks at which the process started
    /// (`/proc/<pid>/stat` field 22). Used to detect pid-recycle
    /// — kernels reuse pids, so an aliveness check on `pid` alone
    /// can falsely report a recycled-by-another-program pid as "our
    /// session still alive." Treat the session as exited if the
    /// kernel-reported start time no longer matches what we stored
    /// at spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time_ticks: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Registry {
    sessions: Vec<SessionInfo>,
}

/// Resolve the `proc/` directory used for capability session state.
/// `COS_PROC_DATA_DIR` can pin routed user jobs and their App/MCP
/// children to the same registry even when their general data dirs
/// differ.
fn proc_dir() -> PathBuf {
    crate::paths::proc_data_dir().join("proc")
}

fn registry_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    if crate::paths::current_owner_uid_override().is_none()
        && unsafe { libc::geteuid() } != 0
        && crate::caps::enforcement::process_has_no_new_privs()
    {
        let uid = unsafe { libc::geteuid() as u32 };
        let routed = PathBuf::from("/run/cos/caps").join(uid.to_string());
        if std::env::var_os("COS_PROC_DATA_DIR")
            .map(PathBuf::from)
            .as_deref()
            == Some(routed.as_path())
        {
            return routed.join("proc").join("registry.json");
        }
    }
    if let Some(path) = bound_app_registry_path() {
        return path;
    }
    proc_dir().join("registry.json")
}

fn bound_app_registry_path() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let uid = unsafe { libc::geteuid() as u32 };
        let path = PathBuf::from("/run/cos/caps")
            .join(uid.to_string())
            .join("proc")
            .join("registry.json");
        let data = fs::read_to_string(&path).ok()?;
        let registry: Registry = serde_json::from_str(&data).ok()?;
        let caller = std::process::id();
        if registry.sessions.iter().any(|session| {
            session.app_id.is_some()
                && !session.pending_bind
                && session.pid != 0
                && session
                    .start_time_ticks
                    .is_some_and(|expected| {
                        read_start_time_ticks(session.pid) == Some(expected)
                    })
                && process_descends_from(caller, session.pid)
        }) {
            return Some(path);
        }

    }
    None
}

fn bound_app_session_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let uid = unsafe { libc::geteuid() as u32 };
        let path = PathBuf::from("/run/cos/caps")
            .join(uid.to_string())
            .join("proc")
            .join("registry.json");
        let data = fs::read_to_string(path).ok()?;
        let registry: Registry = serde_json::from_str(&data).ok()?;
        let caller = std::process::id();
        registry
            .sessions
            .into_iter()
            .find(|session| {
                session.app_id.is_some()
                    && !session.pending_bind
                    && session.pid != 0
                    && session.start_time_ticks.is_some_and(|expected| {
                        read_start_time_ticks(session.pid) == Some(expected)
                    })
                    && process_descends_from(caller, session.pid)
            })
            .map(|session| session.session_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_descends_from(mut child: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if child == ancestor {
            return true;
        }
        if child <= 1 {
            return false;
        }
        let status = match fs::read_to_string(format!("/proc/{child}/status")) {
            Ok(status) => status,
            Err(_) => return false,
        };
        let Some(parent) = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<u32>().ok())
        }) else {
            return false;
        };
        child = parent;
    }
    false
}

/// Public alias used by [`crate::caps::enforcement`] to read the same
/// registry path `cos` writes to. Lives here so the resolution logic
/// in [`proc_dir`] has exactly one definition.
pub(crate) fn registry_path_for_caps() -> PathBuf {
    registry_path()
}

pub fn current_session_info_for_caps() -> Option<SessionInfo> {
    if let Ok(session) = TRUSTED_SESSION_OVERRIDE.try_with(Clone::clone) {
        return Some(session);
    }
    let session_id = current_session_id()?;
    let path = registry_path_for_caps();
    let data = crate::filelock::read_locked(&path).ok()??;
    let registry: Registry = serde_json::from_str(&data).ok()?;
    registry
        .sessions
        .into_iter()
        .find(|session| session.session_id == session_id)
}

pub(crate) fn current_trusted_session_for_caps() -> Option<SessionInfo> {
    TRUSTED_SESSION_OVERRIDE.try_with(Clone::clone).ok()
}

pub(crate) fn session_info_by_id(session_id: &str) -> Option<SessionInfo> {
    let data = crate::filelock::read_locked(&registry_path()).ok()??;
    let registry: Registry = serde_json::from_str(&data).ok()?;
    registry
        .sessions
        .into_iter()
        .find(|session| session.session_id == session_id)
}

/// Snapshot every row in the registry resolved for the currently
/// active owner/home override.
///
/// The trusted App-session authority in `clawd` uses this to locate a
/// connecting process's launcher context inside the root-owned routed
/// registry, instead of believing a session the caller describes.
pub(crate) fn registry_sessions() -> Vec<SessionInfo> {
    let Ok(Some(data)) = crate::filelock::read_locked(&registry_path()) else {
        return Vec::new();
    };
    serde_json::from_str::<Registry>(&data)
        .map(|registry| registry.sessions)
        .unwrap_or_default()
}

/// True only after the current process has been bound to the expected
/// session identity. Used by the launcher shim to keep third-party code
/// from running during the pid-binding window.
pub fn current_session_is_bound(expected_app_id: Option<&str>) -> bool {
    let Some(session) = current_session_info_for_caps() else {
        return false;
    };
    if session.app_id.as_deref() != expected_app_id
        || session.pending_bind
        || session.pid != std::process::id()
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Some(expected_start) = session.start_time_ticks else {
            return false;
        };
        read_start_time_ticks(session.pid) == Some(expected_start)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn load_registry() -> Registry {
    let path = registry_path();
    match crate::filelock::read_locked(&path) {
        Ok(Some(data)) => serde_json::from_str(&data).unwrap_or_default(),
        _ => Registry::default(),
    }
}

/// Atomic read-modify-write on the proc registry. Every mutation goes
/// through this helper so a stale reader cannot overwrite a concurrent
/// spawn, bind, status update, or cleanup.
fn update_registry<F>(transform: F) -> Result<(), String>
where
    F: FnOnce(Registry) -> Registry,
{
    let path = registry_path();
    let owner_uid = crate::paths::current_owner_uid_override();
    prepare_registry_path(&path, owner_uid)?;
    update_registry_path(&path, owner_uid, transform)
}

fn owner_registry_path(uid: u32) -> PathBuf {
    PathBuf::from("/run/cos/caps")
        .join(uid.to_string())
        .join("proc")
        .join("registry.json")
}

fn prepare_registry_path(path: &std::path::Path, owner_uid: Option<u32>) -> Result<(), String> {
    if let Some(uid) = owner_uid {
        let root = path
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or_else(|| "owner registry path is invalid".to_string())?;
        crate::storage::ensure_routed_caps_dir(root, uid)
            .map_err(|error| format!("prepare routed caps dir: {error}"))
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| "registry path has no parent".to_string())?;
        crate::storage::ensure_private_dir(parent)
            .map_err(|error| format!("create proc registry dir: {error}"))
    }
}

fn update_registry_path<F>(
    path: &std::path::Path,
    owner_uid: Option<u32>,
    transform: F,
) -> Result<(), String>
where
    F: FnOnce(Registry) -> Registry,
{
    crate::filelock::update_locked_with_prepare::<_, String, _>(
        path,
        |existing| {
            let reg: Registry = match existing {
                Some(s) => serde_json::from_str(&s)
                    .map_err(|e| format!("parse proc registry: {e}"))?,
                None => Registry::default(),
            };
            let next = transform(reg);
            serde_json::to_string_pretty(&next)
                .map_err(|e| format!("serialize: {e}"))
        },
        |tmp| match owner_uid {
            Some(uid) => crate::storage::set_group_readable_file(tmp, uid),
            None => crate::storage::set_private_file(tmp),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn update_owner_registry<F>(uid: u32, transform: F) -> Result<(), String>
where
    F: FnOnce(Registry) -> Registry,
{
    let path = owner_registry_path(uid);
    prepare_registry_path(&path, Some(uid))?;
    update_registry_path(&path, Some(uid), transform)
}

/// Register a freshly-built [`SessionInfo`] into the on-disk registry.
///
/// Used by interactive entry points (e.g. the user-CLI session
/// bootstrap in `caps::bootstrap`) to create a session row without
/// spawning a child process the way `proc spawn` does. Creates the
/// data dir on first call. Best-effort: returns `Err` only if the
/// registry write itself failed.
pub fn register_session(info: SessionInfo) -> Result<(), String> {
    update_registry(|mut registry| {
        registry
            .sessions
            .retain(|session| session.session_id != info.session_id);
        registry.sessions.push(info);
        registry
    })
}

pub fn register_session_for_owner(info: SessionInfo, uid: u32) -> Result<(), String> {
    update_owner_registry(uid, |mut registry| {
        registry
            .sessions
            .retain(|session| session.session_id != info.session_id);
        registry.sessions.push(info);
        registry
    })
}

/// Remove a session row by id. No-op if it is not present.
/// Counterpart to [`register_session`], called on process exit by
/// the CLI session guard so the registry doesn't accumulate ghosts.
pub fn deregister_session(session_id: &str) {
    let _ = update_registry(|mut registry| {
        registry
            .sessions
            .retain(|session| session.session_id != session_id);
        registry
    });
}

pub fn deregister_session_for_owner(session_id: &str, uid: u32) {
    let _ = update_owner_registry(uid, |mut registry| {
        registry
            .sessions
            .retain(|session| session.session_id != session_id);
        registry
    });
}

/// Remove a session only when it is still bound to the calling process
/// and carries the expected group label. This lets a detached child clean
/// up its own row without turning an inherited environment variable into
/// an arbitrary session-deletion primitive.
pub fn deregister_current_process_session(
    session_id: &str,
    expected_group: &str,
) {
    let pid = std::process::id();
    let current_start = read_start_time_ticks(pid);
    let _ = update_registry(|mut registry| {
        registry.sessions.retain(|session| {
            if session.session_id != session_id
                || session.pid != pid
                || session.group.as_deref() != Some(expected_group)
            {
                return true;
            }
            #[cfg(target_os = "linux")]
            {
                session.start_time_ticks != current_start
                    || current_start.is_none()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        });
        registry
    });
}

pub fn bind_session_process_for_owner(
    session_id: &str,
    pid: u32,
    uid: u32,
) -> Result<(), String> {
    if pid == 0 {
        return Err("cannot bind a session to pid 0".to_string());
    }
    let start_time_ticks = read_start_time_ticks(pid);
    #[cfg(target_os = "linux")]
    if start_time_ticks.is_none() {
        return Err(format!(
            "cannot bind session `{session_id}` to missing process {pid}"
        ));
    }
    let mut found = false;
    update_owner_registry(uid, |mut registry| {
        if let Some(session) = registry
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.pid = pid;
            session.start_time_ticks = start_time_ticks;
            session.pending_bind = false;
            session.exit_code = None;
            session.ended_at = None;
            found = true;
        }
        registry
    })?;
    if found {
        Ok(())
    } else {
        Err(format!("session not found while binding process: {session_id}"))
    }
}

pub fn bind_session_process(session_id: &str, pid: u32) -> Result<(), String> {
    if pid == 0 {
        return Err("cannot bind a session to pid 0".to_string());
    }
    let start_time_ticks = read_start_time_ticks(pid);
    #[cfg(target_os = "linux")]
    if start_time_ticks.is_none() {
        return Err(format!(
            "cannot bind session `{session_id}` to missing process {pid}"
        ));
    }
    let mut found = false;
    update_registry(|mut registry| {
        if let Some(session) = registry
            .sessions
            .iter_mut()
            .find(|session| session.session_id == session_id)
        {
            session.pid = pid;
            session.start_time_ticks = start_time_ticks;
            session.pending_bind = false;
            session.exit_code = None;
            session.ended_at = None;
            found = true;
        }
        registry
    })?;
    if found {
        Ok(())
    } else {
        Err(format!("session not found while binding process: {session_id}"))
    }
}

pub fn set_app_session_transient_caps(
    session_id: &str,
    caps: Option<crate::caps::CapSet>,
) -> Result<(), String> {
    swap_app_session_transient_caps(session_id, caps).map(|_| ())
}

/// Replace an App session's transient capabilities and return what was
/// there before.
///
/// The read and the write happen inside one locked registry update, so
/// the returned value is exactly the state a caller has to restore if a
/// later step of the same operation fails. Widening a session's
/// capabilities and then failing to re-derive the matching authority
/// grant must not leave the wider set behind, and a caller cannot
/// reconstruct the previous value safely by reading first — another
/// call could land between the read and the write.
pub fn swap_app_session_transient_caps(
    session_id: &str,
    caps: Option<crate::caps::CapSet>,
) -> Result<Option<crate::caps::CapSet>, String> {
    let mut found = false;
    let mut previous: Option<crate::caps::CapSet> = None;
    update_registry(|mut registry| {
        if let Some(session) = registry.sessions.iter_mut().find(|session| {
            session.session_id == session_id && session.app_id.is_some()
        }) {
            previous = session.transient_caps.take();
            session.transient_caps = caps;
            found = true;
        }
        registry
    })?;
    if found {
        Ok(previous)
    } else {
        Err(format!(
            "App session not found while updating transient caps: {session_id}"
        ))
    }
}

/// Public cross-uid-safe aliveness check for a pid. Returns `true` when
/// the process exists (including when it belongs to another uid). Used
/// by the agent job recovery path to decide whether a job stuck in
/// `running/` belongs to a worker that is still alive or to one that
/// crashed. See [`is_alive`] for the EPERM rationale.
pub fn is_pid_alive(pid: u32) -> bool {
    is_alive(pid)
}

/// Cross-uid safe aliveness check. `kill(pid, 0)` alone returns
/// -1/EPERM for a pid that exists but belongs to a different uid,
/// which the old code interpreted as "process is gone" — that
/// allowed a low-privileged process to "reclaim" a high-privileged
/// agent's recorded PID and (via cmd_kill) try to SIGTERM whatever
/// landed on that pid next. Treat EPERM as "alive (not ours)".
fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error();
        err.raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        Command::new("cmd")
            .args(["/c", &format!("tasklist /FI \"PID eq {pid}\" /NH")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// Read field 22 (`starttime` in clock ticks since boot) of
/// `/proc/<pid>/stat`. Used by [`is_alive_for_info`] to detect a
/// pid-recycle race where another process now owns the pid we
/// previously recorded for the session.
///
/// `comm` (field 2) may itself contain spaces or parens, so the
/// only safe way to split is to find the LAST `)` and parse the
/// post-comm region as whitespace-separated fields. starttime is
/// field 22, which is index 19 of the post-comm slice (state=0,
/// ppid=1, …, starttime=19).
fn read_start_time_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rparen = stat.rfind(')')?;
        let tail = stat.get(rparen + 1..)?.trim();
        let fields: Vec<&str> = tail.split_whitespace().collect();
        // post-comm: state(0) ppid(1) pgrp(2) sid(3) tty(4) tpgid(5)
        //   flags(6) min(7) cmin(8) maj(9) cmaj(10) utime(11)
        //   stime(12) cutime(13) cstime(14) prio(15) nice(16)
        //   nthreads(17) itrealvalue(18) starttime(19)
        fields.get(19).and_then(|s| s.parse::<u64>().ok())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Aliveness check qualified by start-time identity. If we recorded
/// the kernel's reported starttime when the session was spawned,
/// only count the pid as our session if its current starttime
/// still matches. Otherwise — including the case where the pid was
/// recycled into a different process — report exited.
fn is_alive_for_info(info: &SessionInfo) -> bool {
    if !is_alive(info.pid) {
        return false;
    }
    match info.start_time_ticks {
        Some(expected) => match read_start_time_ticks(info.pid) {
            Some(now) => now == expected,
            // Couldn't read /proc/<pid>/stat (cross-uid, permission
            // denied, or non-Linux) → fall back to the basic pid
            // check which already returned true above.
            None => true,
        },
        None => true,
    }
}

fn pending_bind_is_fresh(info: &SessionInfo) -> bool {
    if !info.pending_bind
        || info.pid != 0
        || !matches!(info.group.as_deref(), Some("app" | "mcp" | "cron"))
    {
        return false;
    }
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(&info.started_at)
    else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(started);
    age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(30)
}

fn registry_session_is_active(info: &SessionInfo) -> bool {
    pending_bind_is_fresh(info) || is_alive_for_info(info)
}

/// Crate-internal: read field 22 (`starttime`) of `/proc/<pid>/stat`.
/// Other kernel-core modules (e.g. `service`) use this to stamp pid
/// files at spawn time so they can later detect a recycled pid.
/// Returns `None` on non-Linux or when /proc can't be read.
pub(crate) fn read_start_time_ticks_pub(pid: u32) -> Option<u64> {
    read_start_time_ticks(pid)
}

/// Crate-internal: aliveness qualified by a previously recorded
/// `starttime` (clock ticks). When `expected` is `None`, behaves like
/// the basic `kill(pid, 0)` aliveness check (legacy pid file with no
/// starttime). When `expected` is `Some`, additionally verifies the
/// current pid's `/proc/<pid>/stat` field 22 still matches — if it
/// doesn't, the pid was recycled into a different process and we
/// must NOT treat it as our session/service.
pub(crate) fn is_alive_with_start_time(pid: u32, expected: Option<u64>) -> bool {
    if !is_alive(pid) {
        return false;
    }
    match expected {
        Some(want) => match read_start_time_ticks(pid) {
            Some(now) => now == want,
            // Couldn't read starttime (non-Linux or perm denied).
            // Fall back to the basic aliveness verdict above.
            None => true,
        },
        None => true,
    }
}

/// Result of [`pgrp_uid_scope_check`]: list of `(pid, uid)` pairs for
/// processes in the target process group whose UID does NOT match the
/// caller's UID. An empty Vec means the entire pgrp is owned by the
/// caller; a non-empty Vec means we must NOT broadcast a group kill
/// or we'd signal someone else's processes.
#[cfg(target_os = "linux")]
type ForeignProcs = Vec<(u32, u32)>;

/// Read the `pgrp` field of `/proc/<pid>/stat` (post-comm index 2).
/// Mirrors [`read_start_time_ticks`]'s last-`)` parsing trick so it
/// is safe against `comm` strings containing spaces or parens.
#[cfg(target_os = "linux")]
fn read_pgrp(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rparen = stat.rfind(')')?;
    let tail = stat.get(rparen + 1..)?.trim();
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // post-comm: state(0) ppid(1) pgrp(2) …
    fields.get(2).and_then(|s| s.parse::<i64>().ok())
}

/// Read the real UID (first column of `Uid:`) of `/proc/<pid>/status`.
#[cfg(target_os = "linux")]
fn read_real_uid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            return parts.first().and_then(|s| s.parse::<u32>().ok());
        }
    }
    None
}

/// Walk `/proc/*/stat` and return every pid whose pgid == `leader_pid`.
/// On non-Linux returns an empty Vec — the helper is best-effort and
/// the caller falls back to letting `kill(-pid, …)` operate.
#[cfg(target_os = "linux")]
fn pids_in_pgrp(leader_pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let s = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match s.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(pgrp) = read_pgrp(pid) {
            if pgrp == leader_pid as i64 {
                out.push(pid);
            }
        }
    }
    out
}

/// Verify that every process in the kernel pgrp led by `leader_pid` is
/// owned by `expected_uid`. Returns Ok(()) when the pgrp is exclusively
/// the caller's, or Err with the list of foreign (pid, uid) pairs.
///
/// Why this matters: `kill_process` sends `kill(-pid, SIGTERM)` to
/// signal the entire process group. If a session leader exited and
/// the kernel recycled its pgid (or a setuid child re-parented into
/// the pgrp), broadcasting a SIGTERM would hit processes the caller
/// does not own — a privilege confusion bug flagged HIGH in the
/// kernel audit. By pre-checking pgrp membership and UIDs we abort
/// the kill cleanly before signalling anything we shouldn't.
///
/// Linux-only — on other OSes pgrp/UID introspection isn't available
/// via /proc, so this helper returns Ok(()) and the caller proceeds
/// with the old behaviour (the audit scoped the fix to Linux).
#[cfg(target_os = "linux")]
fn pgrp_uid_scope_check(leader_pid: u32, expected_uid: u32) -> Result<(), ForeignProcs> {
    let mut foreign: ForeignProcs = Vec::new();
    let members = pids_in_pgrp(leader_pid);
    // Always include the leader itself, even if /proc/<leader>/stat
    // couldn't be read (e.g. it just exited).
    let mut seen_leader = false;
    for pid in &members {
        if *pid == leader_pid {
            seen_leader = true;
        }
        if let Some(uid) = read_real_uid(*pid) {
            if uid != expected_uid {
                foreign.push((*pid, uid));
            }
        }
    }
    if !seen_leader {
        if let Some(uid) = read_real_uid(leader_pid) {
            if uid != expected_uid {
                foreign.push((leader_pid, uid));
            }
        }
    }
    if foreign.is_empty() {
        Ok(())
    } else {
        Err(foreign)
    }
}

#[cfg(not(target_os = "linux"))]
fn pgrp_uid_scope_check(_leader_pid: u32, _expected_uid: u32) -> Result<(), Vec<(u32, u32)>> {
    Ok(())
}

/// Caller's real UID. On non-Unix returns 0 (and the scope check is
/// a no-op there anyway).
fn caller_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() as u32 }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn signal_owner_uid() -> u32 {
    crate::paths::current_owner_uid_override().unwrap_or_else(caller_uid)
}

fn validate_signal_target(info: &SessionInfo, process_group: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let expected_start = info.start_time_ticks.ok_or_else(|| {
            format!(
                "session `{}` has no process start-time identity",
                info.session_id
            )
        })?;
        if read_start_time_ticks(info.pid) != Some(expected_start) {
            return Err(format!(
                "session `{}` process {} is missing or its PID was recycled",
                info.session_id, info.pid
            ));
        }
        let expected_uid = signal_owner_uid();
        if read_real_uid(info.pid) != Some(expected_uid) {
            return Err(format!(
                "session `{}` process {} is not owned by uid {}",
                info.session_id, info.pid, expected_uid
            ));
        }
        if process_group {
            if read_pgrp(info.pid) != Some(info.pid as i64) {
                return Err(format!(
                    "session `{}` process {} is not its process-group leader",
                    info.session_id, info.pid
                ));
            }
            if let Err(foreign) = pgrp_uid_scope_check(info.pid, expected_uid) {
                return Err(format!(
                    "session `{}` process group contains foreign processes: {:?}",
                    info.session_id, foreign
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (info, process_group);
        Err("identity-safe process signaling requires Linux".to_string())
    }
}

#[cfg(unix)]
fn primary_gid(uid: u32) -> Result<u32, String> {
    const BUFFER_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUFFER_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(format!("passwd lookup failed for uid {uid}"));
    }
    Ok(passwd.pw_gid)
}

fn validate_session_identifier(session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.len() > 128
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "session id must contain only alphanumerics, '-' or '_' (max 128 bytes)"
                .to_string(),
        );
    }
    Ok(())
}

fn open_process_output(path: &std::path::Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "spawn" => cmd_spawn(args),
        "status" => cmd_status(args),
        "output" => cmd_output(args),
        "kill" => cmd_kill(args),
        "list" => cmd_list(args),
        "wait" => cmd_wait(args),
        "signal" => cmd_signal(args),
        "result" => cmd_result(args),
        "stats" => cmd_stats(args),
        "renice" => cmd_renice(args),
        _ => Err(format!("unknown proc command: {command}")),
    }
}

fn cmd_spawn(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SPAWN, Scope::wild()).map_err(|v| v.to_string())?;
    let parent_info = current_session_info_for_caps()
        .ok_or_else(|| "proc spawn requires a registered parent session".to_string())?;
    crate::caps::enforcement::require_current_session_identity(
        &parent_info.session_id,
        parent_info.pid,
    )
    .map_err(|error| format!("proc parent identity check failed: {error}"))?;
    let parent_caps = parent_info
        .caps
        .clone()
        .ok_or_else(|| "proc parent session has no capabilities".to_string())?;
    let mut session_id = None;
    let mut group = None;
    let mut parent = None;
    let mut workdir = None;
    let mut tier: Option<u8> = None;
    let mut scope: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut isolated_workspace = false;
    let mut role_name: Option<String> = None;
    let mut caps_arg: Option<String> = None;
    let mut scope_path: Option<String> = None;
    let mut scope_host: Option<String> = None;
    let mut scope_name: Option<String> = None;
    let mut cmd_start = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" if i + 1 < args.len() => {
                session_id = Some(args[i + 1].clone());
                i += 2;
            }
            "--group" if i + 1 < args.len() => {
                group = Some(args[i + 1].clone());
                i += 2;
            }
            "--parent" if i + 1 < args.len() => {
                parent = Some(args[i + 1].clone());
                i += 2;
            }
            "--workdir" if i + 1 < args.len() => {
                workdir = Some(args[i + 1].clone());
                i += 2;
            }
            "--workspace" if i + 1 < args.len() && args[i + 1] == "isolated" => {
                isolated_workspace = true;
                i += 2;
            }
            "--tier" if i + 1 < args.len() => {
                tier = Some(
                    args[i + 1]
                        .parse::<u8>()
                        .map_err(|_| "tier must be 0-3".to_string())?,
                );
                i += 2;
            }
            "--role" if i + 1 < args.len() => {
                role_name = Some(args[i + 1].clone());
                i += 2;
            }
            "--caps" if i + 1 < args.len() => {
                caps_arg = Some(args[i + 1].clone());
                i += 2;
            }
            "--scope-path" if i + 1 < args.len() => {
                scope_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--scope-host" if i + 1 < args.len() => {
                scope_host = Some(args[i + 1].clone());
                i += 2;
            }
            "--scope-name" if i + 1 < args.len() => {
                scope_name = Some(args[i + 1].clone());
                i += 2;
            }
            "--scope" if i + 1 < args.len() => {
                scope = Some(args[i + 1].clone());
                i += 2;
            }
            "--priority" if i + 1 < args.len() => {
                let p = args[i + 1].to_lowercase();
                if !["low", "normal", "high", "realtime"].contains(&p.as_str()) {
                    return Err("priority must be: low, normal, high, realtime".into());
                }
                priority = Some(p);
                i += 2;
            }
            "--" => {
                cmd_start = i + 1;
                break;
            }
            _ => {
                cmd_start = i;
                break;
            }
        }
    }

    if cmd_start >= args.len() {
        return Err("no command specified".into());
    }

    if let Some(requested_parent) = parent.as_deref() {
        if requested_parent != parent_info.session_id.as_str() {
            return Err(format!(
                "proc parent `{requested_parent}` does not match current session `{}`",
                parent_info.session_id
            ));
        }
    }
    parent = Some(parent_info.session_id.clone());

    let command_args = &args[cmd_start..];

    // Validate tier value (0-3 only)
    if let Some(t) = tier {
        if t > 3 {
            return Err("tier must be 0-3 (0=ROOT, 1=OPERATE, 2=CREATE, 3=OBSERVE)".into());
        }
    }

    // Resolve --role / --caps into a CapSet. The kernel caps gate
    // (`caps::require`) consults this field; `--tier` is retained
    // alongside it for back-compat with the legacy tier API.
    let cap_set: Option<crate::caps::CapSet> = match (role_name.as_deref(), caps_arg.as_deref()) {
        (Some(_), Some(_)) => {
            return Err("--role and --caps are mutually exclusive".into());
        }
        (Some(name), None) => {
            let role = crate::caps::Role::parse(name).ok_or_else(|| {
                format!(
                    "unknown role `{name}`; valid: observer, worker, curator, connector, automator, agent-host, admin"
                )
            })?;
            if role == crate::caps::Role::Kernel {
                return Err("the kernel role cannot be assigned to a child process".to_string());
            }
            let path_s = scope_path.as_deref().map(crate::caps::Scope::path);
            let host_s = scope_host.as_deref().map(crate::caps::Scope::host);
            let name_s = scope_name.as_deref().map(crate::caps::Scope::name);
            if tier.is_none() {
                tier = Some(role.credential_tier());
            }
            Some(role.caps_with_scopes(path_s, host_s, name_s))
        }
        (None, Some(list)) => {
            let mut set = crate::caps::CapSet::new();
            for tok in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let verb = crate::caps::Verb::parse(tok)
                    .ok_or_else(|| format!("unknown verb `{tok}` in --caps list"))?;
                let scope = crate::caps::catalog::lookup(verb)
                    .map(|m| m.scope_kind)
                    .map(|k| match k {
                        crate::caps::scope::ScopeKind::Path => scope_path
                            .as_deref()
                            .map(crate::caps::Scope::path)
                            .unwrap_or(crate::caps::Scope::Wild),
                        crate::caps::scope::ScopeKind::Host => scope_host
                            .as_deref()
                            .map(crate::caps::Scope::host)
                            .unwrap_or(crate::caps::Scope::Wild),
                        crate::caps::scope::ScopeKind::Name
                        | crate::caps::scope::ScopeKind::SelfRef => scope_name
                            .as_deref()
                            .map(crate::caps::Scope::name)
                            .unwrap_or(crate::caps::Scope::Wild),
                        _ => crate::caps::Scope::Wild,
                    })
                    .unwrap_or(crate::caps::Scope::Wild);
                set.insert(crate::caps::Cap::new(verb, scope));
            }
            Some(set)
        }
        (None, None) => None,
    };

    match (parent_info.tier, tier) {
        (Some(parent_tier), Some(child_tier)) if child_tier < parent_tier => {
            return Err(format!(
                "cannot escalate tier: parent '{}' has tier {} but child requested tier {}",
                parent_info.session_id, parent_tier, child_tier
            ));
        }
        (Some(parent_tier), None) => tier = Some(parent_tier),
        (None, Some(_)) => {
            return Err("cannot assign a child credential tier when the parent has none".to_string());
        }
        _ => {}
    }

    if let (Some(parent_scope), Some(child_scope)) = (&parent_info.scope, &scope) {
        if !child_scope.starts_with(parent_scope.as_str()) {
            return Err(format!(
                "cannot widen scope: parent '{}' is scoped to '{}' but child requested '{}'",
                parent_info.session_id, parent_scope, child_scope
            ));
        }
    } else if scope.is_none() {
        scope = parent_info.scope.clone();
    }

    let cap_set = match cap_set {
        Some(requested) => {
            if !parent_caps.covers_all(&requested) {
                return Err(format!(
                    "cannot widen caps: parent '{}' does not cover every requested child capability",
                    parent_info.session_id
                ));
            }
            Some(requested)
        }
        None => Some(parent_caps),
    };
    if role_name.is_none() {
        role_name = parent_info.role.clone();
    }

    // Guardrails: check for rapid respawn and destructive commands
    let reg_check = load_registry();
    let rapid_warning = check_rapid_respawn(&reg_check, command_args);
    let destructive_warning = check_destructive(command_args);
    drop(reg_check);

    let sid = session_id.unwrap_or_else(|| format!("proc-{}", short_id()));
    validate_session_identifier(&sid)?;
    let dir = proc_dir();
    fs::create_dir_all(&dir)
        .map_err(|error| format!("create proc directory {}: {error}", dir.display()))?;

    #[cfg(unix)]
    let routed_identity = match crate::paths::current_owner_uid_override() {
        Some(uid) if uid != 0 => {
            let home = crate::paths::verified_home_for_uid(uid)?;
            let gid = primary_gid(uid)?;
            let euid = unsafe { libc::geteuid() as u32 };
            if euid != 0 && euid != uid {
                return Err(format!("cannot spawn routed owner uid {uid} as uid {euid}"));
            }
            if isolated_workspace {
                return Err(
                    "isolated proc workspaces are unavailable for routed user jobs".to_string(),
                );
            }
            if let Some(path) = workdir.as_deref() {
                let canonical = PathBuf::from(path)
                    .canonicalize()
                    .map_err(|error| format!("canonicalize proc workdir: {error}"))?;
                if !canonical.starts_with(&home) {
                    return Err(format!(
                        "proc workdir {} escapes owner home {}",
                        canonical.display(),
                        home.display()
                    ));
                }
                workdir = Some(canonical.to_string_lossy().into_owned());
            } else {
                workdir = Some(home.to_string_lossy().into_owned());
            }
            Some((uid, gid, home))
        }
        _ => None,
    };

    // Handle isolated workspace
    if isolated_workspace {
        let ws_dir = crate::paths::data_dir()
            .join("sessions")
            .join(&sid)
            .join("workspace");
        fs::create_dir_all(&ws_dir)
            .map_err(|e| format!("failed to create isolated workspace: {e}"))?;
        workdir = Some(ws_dir.to_string_lossy().to_string());
    }

    let stdout_path = dir.join(format!("{sid}.stdout"));
    let stderr_path = dir.join(format!("{sid}.stderr"));

    let stdout_file = open_process_output(&stdout_path)
        .map_err(|e| format!("failed to create stdout file: {e}"))?;
    let stderr_file = match open_process_output(&stderr_path) {
        Ok(file) => file,
        Err(error) => {
            drop(stdout_file);
            let _ = fs::remove_file(&stdout_path);
            return Err(format!("failed to create stderr file: {error}"));
        }
    };

    // Apply process priority via nice (Unix only)
    #[cfg(unix)]
    let (actual_cmd, actual_args) = if let Some(ref prio) = priority {
        let nice_val = match prio.as_str() {
            "low" => "10",
            "normal" => "0",
            "high" => "-5",
            "realtime" => "-10",
            _ => "0",
        };
        let mut nice_args = vec!["-n".to_string(), nice_val.to_string()];
        nice_args.extend_from_slice(command_args);
        ("nice".to_string(), nice_args)
    } else {
        (command_args[0].clone(), command_args[1..].to_vec())
    };

    #[cfg(not(unix))]
    let (actual_cmd, actual_args) = (command_args[0].clone(), command_args[1..].to_vec());

    let mut cmd = Command::new(&actual_cmd);
    cmd.args(&actual_args)
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        // Agent-native: suppress all interactive prompts
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("PIP_NO_INPUT", "1")
        .env("NPM_CONFIG_YES", "true")
        .env("PYTHONDONTWRITEBYTECODE", "1");

    if let Some(ref wd) = workdir {
        cmd.current_dir(wd);
    }
    #[cfg(unix)]
    if let Some((_, _, home)) = routed_identity.as_ref() {
        cmd.env("HOME", home).env("COS_HOME", home);
    }

    // Inject session ID so child process can be identified by policy module
    cmd.env("COS_SESSION", &sid)
        .env("COS_PROC_DATA_DIR", crate::paths::proc_data_dir());

    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        let identity = routed_identity
            .as_ref()
            .map(|(uid, gid, _)| (*uid, *gid));
        let euid = libc::geteuid() as u32;
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some((uid, gid)) = identity {
                if euid == 0
                    && (libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(gid) != 0
                        || libc::setuid(uid) != 0)
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;

    let pid = child.id();
    // Capture the kernel-reported start time IMMEDIATELY after
    // spawn. Stored on disk in SessionInfo so future aliveness
    // checks can detect a pid-recycle (kernels reuse pids; if the
    // start time differs, the pid now refers to a different
    // process).
    let start_time_ticks = read_start_time_ticks(pid);
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let info = SessionInfo {
        session_id: sid.clone(),
        pid,
        command: command_args.to_vec(),
        started_at: now.clone(),
        stdout_path: stdout_path.to_string_lossy().to_string(),
        stderr_path: stderr_path.to_string_lossy().to_string(),
        group: group.clone(),
        parent: parent.clone(),
        workdir: workdir.clone(),
        exit_code: None,
        ended_at: None,
        tier,
        scope: scope.clone(),
        priority: priority.clone(),
        caps: cap_set.clone(),
        transient_caps: None,
        role: role_name.clone(),
        app_id: None,
        pending_bind: false,
        start_time_ticks,
    };

    let info_for_registry = info.clone();
    update_registry(|mut reg| {
        reg.sessions.push(info_for_registry);
        reg
    })?;

    // Reap the child in a background thread. Replacing the old
    // `std::mem::forget(child)` — which left every exited child as
    // <defunct> in the cos parent's process table — preserves the
    // detached-spawn semantics (process keeps running after `cos
    // proc spawn` returns) while freeing the kernel PID slot once
    // the child exits.
    thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    let mut result = json!({
        "session_id": sid,
        "pid": pid,
        "command": command_args,
        "started_at": now,
    });
    if let Some(g) = group {
        result["group"] = json!(g);
    }
    if let Some(p) = parent {
        result["parent"] = json!(p);
    }
    if let Some(w) = workdir {
        result["workdir"] = json!(w);
    }
    if let Some(t) = tier {
        result["tier"] = json!(t);
    }
    if let Some(ref s) = scope {
        result["scope"] = json!(s);
    }
    if let Some(ref pr) = priority {
        result["priority"] = json!(pr);
    }
    if let Some(ref r) = role_name {
        result["role"] = json!(r);
    }
    if let Some(ref c) = cap_set {
        result["caps"] = json!(c);
    }
    let mut warnings = Vec::new();
    if let Some(w) = rapid_warning {
        warnings.push(w);
    }
    if let Some(w) = destructive_warning {
        warnings.push(w);
    }
    if !warnings.is_empty() {
        result["warnings"] = json!(warnings);
    }

    Ok(result)
}

fn cmd_status(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc status <session-id>")?;
    let mut reg = load_registry();
    let idx = reg
        .sessions
        .iter()
        .position(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let binding = pending_bind_is_fresh(&reg.sessions[idx]);
    let alive = is_alive_for_info(&reg.sessions[idx]);
    let status = if binding {
        "binding"
    } else if alive {
        "running"
    } else {
        "exited"
    };

    // Auto-capture ended_at when process is first detected as dead
    if !binding && !alive && reg.sessions[idx].ended_at.is_none() {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let sid = sid.to_string();
        let now_for_registry = now.clone();
        update_registry(|mut latest| {
            if let Some(info) = latest
                .sessions
                .iter_mut()
                .find(|info| info.session_id == sid)
            {
                if info.ended_at.is_none() && !registry_session_is_active(info)
                {
                    info.ended_at = Some(now_for_registry);
                }
            }
            latest
        })?;
        reg.sessions[idx].ended_at = Some(now);
    }

    let info = &reg.sessions[idx];
    let mut result = json!({
        "session_id": info.session_id,
        "pid": info.pid,
        "status": status,
        "command": info.command,
        "started_at": info.started_at,
    });
    if let Some(ref ended) = info.ended_at {
        result["ended_at"] = json!(ended);
    }
    if let Some(t) = info.tier {
        result["tier"] = json!(t);
    }
    if let Some(ref s) = info.scope {
        result["scope"] = json!(s);
    }

    Ok(result)
}

fn cmd_output(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc output <session-id>")?;
    let mut tail_lines: Option<usize> = None;
    let mut stream = "both".to_string();
    let mut follow = false;
    let mut since_offset: Option<u64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" if i + 1 < args.len() => {
                tail_lines = args[i + 1].parse().ok();
                i += 2;
            }
            "--stream" if i + 1 < args.len() => {
                stream = args[i + 1].clone();
                i += 2;
            }
            "--follow" => {
                follow = true;
                i += 1;
            }
            "--since-offset" if i + 1 < args.len() => {
                since_offset = args[i + 1].parse().ok();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let reg = load_registry();
    let info = reg
        .sessions
        .iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    // --since-offset mode: incremental reading from byte offset
    if let Some(offset) = since_offset {
        let (stdout_data, stdout_offset) = if stream == "stdout" || stream == "both" {
            read_from_offset(&info.stdout_path, offset)
        } else {
            (String::new(), offset)
        };
        let (stderr_data, stderr_offset) = if stream == "stderr" || stream == "both" {
            read_from_offset(&info.stderr_path, offset)
        } else {
            (String::new(), offset)
        };
        return Ok(json!({
            "session_id": sid,
            "stdout": stdout_data,
            "stderr": stderr_data,
            "stdout_offset": stdout_offset,
            "stderr_offset": stderr_offset,
            "status": if is_alive(info.pid) { "running" } else { "exited" },
        }));
    }

    // --follow mode: block until process exits, then return all output
    if follow {
        let stdout_path = info.stdout_path.clone();
        let stderr_path = info.stderr_path.clone();
        let pid = info.pid;
        drop(reg);

        while is_alive(pid) {
            thread::sleep(Duration::from_millis(250));
        }

        let mut result = json!({
            "session_id": sid,
            "status": "exited",
        });
        if stream == "stdout" || stream == "both" {
            result["stdout"] = json!(read_capped(&stdout_path, None));
        }
        if stream == "stderr" || stream == "both" {
            result["stderr"] = json!(read_capped(&stderr_path, None));
        }
        return Ok(result);
    }

    // Default mode: read current output
    let mut result = json!({
        "session_id": sid,
        "status": if is_alive(info.pid) { "running" } else { "exited" },
    });

    if stream == "stdout" || stream == "both" {
        result["stdout"] = json!(read_capped(&info.stdout_path, tail_lines));
    }
    if stream == "stderr" || stream == "both" {
        result["stderr"] = json!(read_capped(&info.stderr_path, tail_lines));
    }

    Ok(result)
}

fn cmd_kill(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SIGNAL, Scope::wild()).map_err(|v| v.to_string())?;
    // --group mode: kill all sessions in a group
    if args.len() >= 2 && args[0] == "--group" {
        let group_name = &args[1];
        let reg = load_registry();
        let group_sessions: Vec<&SessionInfo> = reg
            .sessions
            .iter()
            .filter(|s| s.group.as_deref() == Some(group_name.as_str()))
            .collect();
        if group_sessions.is_empty() {
            return Err(format!("no sessions in group: {group_name}"));
        }

        // Per-leader UID scope check. `kill_process` sends
        // `kill(-pid, SIGTERM)` which fans the signal across the
        // entire kernel process group; if a setuid descendant or a
        // recycled-pid intruder is in that pgrp we must NOT signal
        // it. We skip any session whose pgrp contains a foreign UID
        // and report the reason in the JSON response.
        let mut killed = Vec::new();
        let mut skipped = Vec::new();
        for info in &group_sessions {
            match validate_signal_target(info, true) {
                Ok(()) => {
                    match kill_process(info.pid) {
                        Ok(()) => killed.push(json!({
                            "session_id": info.session_id,
                            "pid": info.pid,
                        })),
                        Err(error) => skipped.push(json!({
                            "session_id": info.session_id,
                            "pid": info.pid,
                            "reason": "signal_failed",
                            "error": error,
                        })),
                    }
                }
                Err(error) => {
                    skipped.push(json!({
                        "session_id": info.session_id,
                        "pid": info.pid,
                        "reason": "identity_mismatch",
                        "error": error,
                    }));
                }
            }
        }
        if killed.is_empty() {
            return Err(format!(
                "no process in group `{group_name}` passed identity checks: {}",
                serde_json::to_string(&skipped).unwrap_or_else(|_| "unknown".to_string())
            ));
        }
        let mut resp = json!({
            "group": group_name,
            "status": if skipped.is_empty() { "killed" } else { "partial" },
            "sessions": killed,
        });
        if !skipped.is_empty() {
            resp["skipped"] = json!(skipped);
        }
        return Ok(resp);
    }

    let sid = args.first().ok_or("usage: cos proc kill <session-id>")?;
    let reg = load_registry();
    let info = reg
        .sessions
        .iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    validate_signal_target(info, true)?;
    kill_process(info.pid)?;

    Ok(json!({
        "session_id": sid,
        "status": "killed",
        "pid": info.pid,
    }))
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let reg = load_registry();
    let mut group_filter: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" if i + 1 < args.len() => {
                group_filter = Some(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let infos: Vec<Value> = reg
        .sessions
        .iter()
        .filter(|s| {
            if let Some(g) = group_filter {
                s.group.as_deref() == Some(g)
            } else {
                true
            }
        })
        .map(|s| {
            let mut v = json!({
                "session_id": s.session_id,
                "pid": s.pid,
                "command": s.command,
                "status": if pending_bind_is_fresh(s) {
                    "binding"
                } else if is_alive_for_info(s) {
                    "running"
                } else {
                    "exited"
                },
                "started_at": s.started_at,
            });
            if let Some(ref g) = s.group {
                v["group"] = json!(g);
            }
            if let Some(ref p) = s.parent {
                v["parent"] = json!(p);
            }
            if let Some(ref w) = s.workdir {
                v["workdir"] = json!(w);
            }
            if let Some(t) = s.tier {
                v["tier"] = json!(t);
            }
            if let Some(ref sc) = s.scope {
                v["scope"] = json!(sc);
            }
            v
        })
        .collect();

    // Prune only from the latest registry snapshot. A stale list read must
    // not overwrite a concurrent App bind or newly registered session.
    update_registry(|mut latest| {
        latest.sessions.retain(registry_session_is_active);
        latest
    })?;

    Ok(json!({ "sessions": infos, "count": infos.len() }))
}

fn kill_process(pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        // Negative PID sends signal to the process group (works with setsid)
        if unsafe { libc::kill(-(pid as i32), libc::SIGTERM) } != 0 {
            return Err(format!(
                "failed to signal process group {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let status = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .map_err(|error| format!("failed to start taskkill: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("taskkill exited with {}", status.code().unwrap_or(-1)))
        }
    }
}

fn cmd_wait(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let mut timeout: Option<u64> = None;
    let mut group_name: Option<&str> = None;
    let mut session_id: Option<&str> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" if i + 1 < args.len() => {
                timeout = args[i + 1].parse().ok();
                i += 2;
            }
            "--group" if i + 1 < args.len() => {
                group_name = Some(&args[i + 1]);
                i += 2;
            }
            _ => {
                if session_id.is_none() {
                    session_id = Some(&args[i]);
                }
                i += 1;
            }
        }
    }

    let reg = load_registry();

    // Collect PIDs and session IDs to wait on
    let targets: Vec<(String, u32)> = if let Some(g) = group_name {
        reg.sessions
            .iter()
            .filter(|s| s.group.as_deref() == Some(g))
            .map(|s| (s.session_id.clone(), s.pid))
            .collect()
    } else if let Some(sid) = session_id {
        let info = reg
            .sessions
            .iter()
            .find(|s| s.session_id == sid)
            .ok_or_else(|| format!("session not found: {sid}"))?;
        vec![(info.session_id.clone(), info.pid)]
    } else {
        return Err("usage: cos proc wait <session-id> [--timeout N] or --group <name>".into());
    };

    drop(reg);

    if targets.is_empty() {
        return Err("no matching sessions to wait on".into());
    }

    let start = SystemTime::now();
    let timeout_dur = timeout.map(Duration::from_secs);

    loop {
        let all_dead = targets.iter().all(|(_, pid)| !is_alive(*pid));
        if all_dead {
            // Auto-capture ended_at for all exited sessions
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let target_ids = targets
                .iter()
                .map(|(sid, _)| sid.clone())
                .collect::<Vec<_>>();
            update_registry(|mut latest| {
                for sid in &target_ids {
                    if let Some(info) =
                        latest.sessions.iter_mut().find(|s| &s.session_id == sid)
                    {
                        if info.ended_at.is_none()
                            && !registry_session_is_active(info)
                        {
                            info.ended_at = Some(now.clone());
                        }
                    }
                }
                latest
            })?;

            // Build results with output tails for each exited session
            let reg = load_registry();
            let results: Vec<Value> = targets
                .iter()
                .map(|(sid, pid)| {
                    let mut v = json!({
                        "session_id": sid,
                        "pid": pid,
                        "status": "exited",
                    });
                    if let Some(info) = reg.sessions.iter().find(|s| &s.session_id == sid) {
                        let stdout_tail = read_capped(&info.stdout_path, Some(10));
                        let stderr_tail = read_capped(&info.stderr_path, Some(10));
                        if !stdout_tail.is_empty() {
                            v["stdout_tail"] = json!(stdout_tail);
                        }
                        if !stderr_tail.is_empty() {
                            v["stderr_tail"] = json!(stderr_tail);
                        }
                    }
                    v
                })
                .collect();
            return Ok(json!({
                "status": "exited",
                "sessions": results,
            }));
        }

        if let Some(td) = timeout_dur {
            let elapsed = start.elapsed().unwrap_or_default();
            if elapsed >= td {
                let results: Vec<Value> = targets
                    .iter()
                    .map(|(sid, pid)| {
                        json!({
                            "session_id": sid,
                            "pid": pid,
                            "status": if is_alive(*pid) { "running" } else { "exited" },
                        })
                    })
                    .collect();
                return Ok(json!({
                    "status": "timeout",
                    "elapsed_secs": elapsed.as_secs(),
                    "sessions": results,
                }));
            }
        }

        thread::sleep(Duration::from_millis(250));
    }
}

fn cmd_signal(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SIGNAL, Scope::wild()).map_err(|v| v.to_string())?;
    if args.len() < 2 {
        return Err("usage: cos proc signal <session-id> <signal-name>".into());
    }
    let sid = &args[0];
    let signal_name = args[1].to_uppercase();

    let reg = load_registry();
    let info = reg
        .sessions
        .iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let pid = info.pid;
    validate_signal_target(info, false)?;

    #[cfg(unix)]
    {
        let signum = match signal_name.as_str() {
            "TERM" => libc::SIGTERM,
            "KILL" => libc::SIGKILL,
            "HUP" => libc::SIGHUP,
            "USR1" => libc::SIGUSR1,
            "USR2" => libc::SIGUSR2,
            "STOP" => libc::SIGSTOP,
            "CONT" => libc::SIGCONT,
            _ => return Err(format!(
                "unsupported signal: {signal_name}. Supported: TERM, KILL, HUP, USR1, USR2, STOP, CONT"
            )),
        };
        let ret = unsafe { libc::kill(pid as i32, signum) };
        if ret != 0 {
            return Err(format!(
                "failed to send signal {signal_name} to pid {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    #[cfg(not(unix))]
    {
        match signal_name.as_str() {
            "TERM" | "KILL" => {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .output();
            }
            _ => return Err(format!("signal {signal_name} not supported on Windows")),
        }
    }

    Ok(json!({
        "session_id": sid,
        "pid": pid,
        "signal": signal_name,
        "status": "sent",
    }))
}

fn cmd_result(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc result <session-id>")?;
    let mut reg = load_registry();
    let idx = reg
        .sessions
        .iter()
        .position(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let binding = pending_bind_is_fresh(&reg.sessions[idx]);
    let alive = is_alive_for_info(&reg.sessions[idx]);
    let status = if binding {
        "binding"
    } else if alive {
        "running"
    } else {
        "exited"
    };

    // Auto-capture ended_at if process is dead and not yet recorded
    if !binding && !alive && reg.sessions[idx].ended_at.is_none() {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let sid = sid.to_string();
        let now_for_registry = now.clone();
        update_registry(|mut latest| {
            if let Some(info) = latest
                .sessions
                .iter_mut()
                .find(|info| info.session_id == sid)
            {
                if info.ended_at.is_none() && !registry_session_is_active(info)
                {
                    info.ended_at = Some(now_for_registry);
                }
            }
            latest
        })?;
        reg.sessions[idx].ended_at = Some(now);
    }

    let info = &reg.sessions[idx];
    let stdout_tail = read_capped(&info.stdout_path, Some(20));
    let stderr_tail = read_capped(&info.stderr_path, Some(20));
    let stdout_bytes = fs::metadata(&info.stdout_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let stderr_bytes = fs::metadata(&info.stderr_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // Heuristic: likely success if stderr is empty/small AND stdout doesn't contain error indicators
    let stdout_has_error =
        stdout_tail.contains("\"error\"") || stdout_tail.contains("permission denied");
    let likely_success = !stdout_has_error
        && (stderr_bytes == 0 || (stdout_bytes > 0 && stderr_bytes < stdout_bytes / 10));

    let mut result = json!({
        "session_id": info.session_id,
        "status": status,
        "started_at": info.started_at,
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
        "likely_success": likely_success,
    });

    if let Some(ref ended) = info.ended_at {
        result["ended_at"] = json!(ended);
        // Calculate duration
        if let Ok(start) =
            chrono::DateTime::parse_from_rfc3339(&info.started_at.replace('Z', "+00:00"))
        {
            if let Ok(end) = chrono::DateTime::parse_from_rfc3339(&ended.replace('Z', "+00:00")) {
                let duration = end.signed_duration_since(start);
                result["duration_secs"] = json!(duration.num_seconds());
            }
        }
    }

    if !stdout_tail.is_empty() {
        result["stdout_tail"] = json!(stdout_tail);
    }
    if !stderr_tail.is_empty() {
        result["stderr_tail"] = json!(stderr_tail);
    }

    Ok(result)
}

/// Get resource usage stats for a process session.
///
/// Reads from /proc/<pid>/stat and /proc/<pid>/status on Linux.
/// Usage: cos proc stats <session-id>
fn cmd_stats(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_OBSERVE, Scope::wild()).map_err(|v| v.to_string())?;
    let sid = args.first().ok_or("usage: cos proc stats <session-id>")?;

    let reg = load_registry();
    let info = reg
        .sessions
        .iter()
        .find(|s| &s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let pid = info.pid;
    let alive = is_alive(pid);

    let mut result = json!({
        "session_id": sid,
        "pid": pid,
        "alive": alive,
    });

    // Read stdout/stderr sizes as I/O proxy
    let stdout_bytes = fs::metadata(&info.stdout_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let stderr_bytes = fs::metadata(&info.stderr_path)
        .map(|m| m.len())
        .unwrap_or(0);
    result["io"] = json!({
        "stdout_bytes": stdout_bytes,
        "stderr_bytes": stderr_bytes,
    });

    #[cfg(target_os = "linux")]
    if alive {
        // Read /proc/<pid>/stat for CPU time
        let stat_path = format!("/proc/{pid}/stat");
        if let Ok(stat_content) = fs::read_to_string(&stat_path) {
            let fields: Vec<&str> = stat_content.split_whitespace().collect();
            // Fields: pid, comm, state, ... utime(14), stime(15), ... vsize(23), rss(24)
            if fields.len() > 24 {
                let utime = fields[13].parse::<u64>().unwrap_or(0);
                let stime = fields[14].parse::<u64>().unwrap_or(0);
                let vsize = fields[22].parse::<u64>().unwrap_or(0);
                let rss_pages = fields[23].parse::<i64>().unwrap_or(0);
                let page_size: u64 = 4096;

                result["cpu"] = json!({
                    "user_ticks": utime,
                    "system_ticks": stime,
                    "total_ticks": utime + stime,
                    "user_ms": utime * 10, // assuming 100 Hz
                    "system_ms": stime * 10,
                    "total_ms": (utime + stime) * 10,
                });
                result["memory"] = json!({
                    "virtual_bytes": vsize,
                    "virtual_mb": vsize / (1024 * 1024),
                    "rss_bytes": (rss_pages as u64) * page_size,
                    "rss_mb": ((rss_pages as u64) * page_size) / (1024 * 1024),
                });
            }
        }

        // Read /proc/<pid>/io for I/O stats
        let io_path = format!("/proc/{pid}/io");
        if let Ok(io_content) = fs::read_to_string(&io_path) {
            let mut read_bytes: u64 = 0;
            let mut write_bytes: u64 = 0;
            for line in io_content.lines() {
                if let Some(val) = line.strip_prefix("read_bytes: ") {
                    read_bytes = val.trim().parse().unwrap_or(0);
                } else if let Some(val) = line.strip_prefix("write_bytes: ") {
                    write_bytes = val.trim().parse().unwrap_or(0);
                }
            }
            result["io"]["read_bytes"] = json!(read_bytes);
            result["io"]["write_bytes"] = json!(write_bytes);
            result["io"]["read_mb"] = json!(read_bytes / (1024 * 1024));
            result["io"]["write_mb"] = json!(write_bytes / (1024 * 1024));
        }

        // Read /proc/<pid>/status for thread count
        let status_path = format!("/proc/{pid}/status");
        if let Ok(status_content) = fs::read_to_string(&status_path) {
            for line in status_content.lines() {
                if let Some(val) = line.strip_prefix("Threads:") {
                    if let Ok(threads) = val.trim().parse::<u32>() {
                        result["threads"] = json!(threads);
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if alive {
            result["note"] = json!("detailed resource stats require Linux /proc filesystem");
        }
    }

    Ok(result)
}

/// Change the priority of a running process session.
///
/// Usage: cos proc renice <session-id> --priority low|normal|high|realtime
fn cmd_renice(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SIGNAL, Scope::wild()).map_err(|v| v.to_string())?;

    let mut session_id: Option<&str> = None;
    let mut priority: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--priority" if i + 1 < args.len() => {
                let p = args[i + 1].to_lowercase();
                if !["low", "normal", "high", "realtime"].contains(&p.as_str()) {
                    return Err("priority must be: low, normal, high, realtime".into());
                }
                priority = Some(p);
                i += 2;
            }
            _ => {
                if session_id.is_none() {
                    session_id = Some(&args[i]);
                }
                i += 1;
            }
        }
    }

    let sid = session_id.ok_or("usage: cos proc renice <session-id> --priority <level>")?;
    let prio = priority.ok_or("--priority is required")?;

    let reg = load_registry();
    let info = reg
        .sessions
        .iter()
        .find(|s| s.session_id == sid)
        .ok_or_else(|| format!("session not found: {sid}"))?;

    let pid = info.pid;
    if !is_alive(pid) {
        return Err(format!("session {sid} is not running"));
    }

    #[cfg(unix)]
    {
        let nice_val: i32 = match prio.as_str() {
            "low" => 10,
            "normal" => 0,
            "high" => -5,
            "realtime" => -10,
            _ => 0,
        };

        let output = Command::new("renice")
            .args(["-n", &nice_val.to_string(), "-p", &pid.to_string()])
            .output()
            .map_err(|e| format!("failed to renice: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("renice failed: {stderr}"));
        }

        let sid_for_registry = sid.to_string();
        let prio_for_registry = prio.clone();
        update_registry(|mut latest| {
            if let Some(info) = latest.sessions.iter_mut().find(|info| {
                info.session_id == sid_for_registry && info.pid == pid
            }) {
                info.priority = Some(prio_for_registry);
            }
            latest
        })?;

        Ok(json!({
            "session_id": sid,
            "pid": pid,
            "priority": prio,
            "nice_value": nice_val,
        }))
    }

    #[cfg(not(unix))]
    {
        Err("renice requires Unix".into())
    }
}

fn check_rapid_respawn(reg: &Registry, command_args: &[String]) -> Option<Value> {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::seconds(60);
    let recent_same = reg
        .sessions
        .iter()
        .filter(|s| s.command == command_args)
        .filter(|s| {
            chrono::DateTime::parse_from_rfc3339(&s.started_at.replace('Z', "+00:00"))
                .map(|dt| dt > cutoff)
                .unwrap_or(false)
        })
        .count();
    if recent_same >= 5 {
        Some(json!({
            "warning": "rapid_respawn",
            "message": format!(
                "This command has been spawned {} times in the last 60 seconds. Possible infinite loop.",
                recent_same
            ),
            "count": recent_same,
        }))
    } else {
        None
    }
}

fn check_destructive(command_args: &[String]) -> Option<Value> {
    let cmd_str = command_args.join(" ");
    let patterns = [
        ("rm -rf /", "deleting root filesystem"),
        ("rm -rf /*", "deleting root filesystem contents"),
        ("mkfs", "formatting disk"),
        ("dd if=", "raw disk write"),
        ("> /dev/sd", "writing to disk device"),
    ];
    for (pattern, reason) in patterns {
        if cmd_str.contains(pattern) {
            return Some(json!({
                "warning": "destructive_command",
                "message": format!("Potentially destructive operation detected: {reason}"),
                "pattern": pattern,
            }));
        }
    }
    None
}

fn read_from_offset(path: &str, offset: u64) -> (String, u64) {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return (String::new(), offset),
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if offset >= file_len {
        return (String::new(), file_len);
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return (String::new(), offset);
    }
    let to_read = (file_len - offset).min(MAX_OUTPUT_BYTES as u64) as usize;
    let mut buf = vec![0u8; to_read];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return (String::new(), offset),
    };
    buf.truncate(n);
    let content = String::from_utf8_lossy(&buf).to_string();
    (content, offset + n as u64)
}

fn read_capped(path: &str, tail_lines: Option<usize>) -> String {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let content = if content.len() > MAX_OUTPUT_BYTES {
        let truncated = &content[content.len() - MAX_OUTPUT_BYTES..];
        format!(
            "[truncated, showing last {}KB]\n{truncated}",
            MAX_OUTPUT_BYTES / 1024
        )
    } else {
        content
    };

    if let Some(n) = tail_lines {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > n {
            return lines[lines.len() - n..].join("\n");
        }
    }
    content
}

fn short_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}", t & 0xFFFFFFFF)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/proc.rs"
    ));
}
