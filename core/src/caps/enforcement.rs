//! The capability enforcement entry point — the *one* function any
//! gated operation must call.
//!
//! ```ignore
//! use crate::caps::{require, Scope, Verb};
//!
//! pub fn read_user_file(path: &str) -> Result<String, Error> {
//!     require(Verb::FS_READ, Scope::path(path))?;
//!     // ... actually read the file
//! }
//! ```
//!
//! The check flow:
//!
//! ```text
//!   read COS_SESSION   ──┐                       ┌─→ Ok(())  if cap covers
//!                        ├→ load session info ──┤
//!   load proc registry ──┘   ├ PID ancestry     └─→ Err(Denial { reason, … })
//!                            ├ caps present?
//!                            └ caps.covers(Cap)?
//! ```
//!
//! ## Strictness
//!
//! Modes are determined by environment, in priority order:
//!
//!   * `COS_PERMS_MODE=strict` (default) — every guarded op requires a
//!     session with caps. Unset `COS_SESSION`, missing session, or
//!     missing `caps` field → deny.
//!   * `COS_PERMS_MODE=permissive` — opt-in escape hatch for first-boot
//!     installer scripts that run before the session registry exists.
//!     Unset `COS_SESSION` is allowed; missing session is allowed;
//!     missing caps is allowed.
//!
//! Strict is the only safe default for an agent-native OS — the kernel
//! must not silently allow operations from contexts the policy layer
//! cannot describe.

use std::path::PathBuf;

use serde::Deserialize;

use super::cap::{Cap, CapSet};
use super::denial::{Denial, DenialReason};
use super::risk::Risk;
use super::scope::Scope;
use super::verb::Verb;

// ---------------------------------------------------------------------------
// Mode
// ---------------------------------------------------------------------------

/// Enforcement mode. Picked up from the `COS_PERMS_MODE` env var at
/// every call (so tests can flip it without restarting the process).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Opt-in escape hatch for development: allow when there is no
    /// session, no registry entry, or no `caps` on the session. Only
    /// sessions that have explicit `caps` are gated. Set
    /// `COS_PERMS_MODE=permissive` to use this.
    Permissive,
    /// Default. All gated operations require a session with caps.
    /// Anything else is denied.
    Strict,
}

impl Mode {
    fn from_env() -> Self {
        if process_has_no_new_privs() {
            return Mode::Strict;
        }
        match std::env::var("COS_PERMS_MODE").as_deref() {
            Ok("permissive") => Mode::Permissive,
            // Includes the explicit "strict" value and any other value
            // (typos default to the safer mode).
            _ => Mode::Strict,
        }
    }

    /// Stable kebab-case label for logs and audit records.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Mode::Permissive => "permissive",
            Mode::Strict => "strict",
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_has_no_new_privs() -> bool {
    unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 0 }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_has_no_new_privs() -> bool {
    false
}

// ---------------------------------------------------------------------------
// Registry view (read-only subset of proc.rs's SessionInfo)
// ---------------------------------------------------------------------------
//
// We deserialise only the fields the cap system cares about. Extra
// fields written by proc.rs (command, stdout path, tier, scope…) are
// silently ignored.

#[derive(Deserialize, Default)]
struct SessionRow {
    session_id: String,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    caps: Option<CapSet>,
    #[serde(default)]
    transient_caps: Option<CapSet>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    pending_bind: bool,
    #[serde(default)]
    start_time_ticks: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Registry {
    #[serde(default)]
    sessions: Vec<SessionRow>,
}

fn registry_path() -> PathBuf {
    crate::proc::registry_path_for_caps()
}

fn load_registry() -> Registry {
    std::fs::read_to_string(registry_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn require_current_session_identity(
    session_id: &str,
    session_pid: u32,
) -> Result<(), String> {
    if crate::proc::current_session_id().as_deref() != Some(session_id) {
        return Err("COS_SESSION does not match the selected registry row".to_string());
    }
    if session_pid == 0 {
        return Err("registered session process is not bound".to_string());
    }
    if let Some(session) = crate::proc::current_trusted_session_for_caps() {
        if session.session_id == session_id
            && session.pid == session_pid
            && session_pid == std::process::id()
        {
            return Ok(());
        }
        return Err("trusted task session identity does not match the caller".to_string());
    }
    let registry = load_registry();
    let session = registry
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .ok_or_else(|| format!("session `{session_id}` is not registered"))?;
    if session.pid != session_pid {
        return Err(format!(
            "session `{session_id}` changed process binding from {session_pid} to {}",
            session.pid
        ));
    }
    validate_process_identity(&registry, session, std::process::id(), true)
}

fn validate_process_identity(
    registry: &Registry,
    session: &SessionRow,
    caller_pid: u32,
    strict: bool,
) -> Result<(), String> {
    if session.pending_bind || (session.app_id.is_some() && session.pid == 0) {
        return Err("App session process has not been bound yet".to_string());
    }
    if let Some(app_id) = session.app_id.as_deref() {
        if std::env::var("COS_APP_ID").ok().as_deref() != Some(app_id) {
            return Err(format!(
                "COS_APP_ID does not match registered App identity `{app_id}`"
            ));
        }
        if !app_session_process_is_current(session) {
            return Err(format!(
                "registered App process {} no longer matches its start time",
                session.pid
            ));
        }
    }

    if session.pid != 0 && caller_pid != session.pid {
        match pid_ancestry::is_descendant_of(caller_pid, session.pid) {
            AncestryResult::Yes => {}
            AncestryResult::No => {
                return Err(format!(
                    "process {caller_pid} is not descended from registered session process {}",
                    session.pid
                ));
            }
            AncestryResult::Unsupported if strict => {
                return Err(
                    "pid-ancestry checking is not implemented on this platform"
                        .to_string(),
                );
            }
            AncestryResult::Unsupported => {}
        }
    }

    match nearest_app_session(registry, caller_pid) {
        Ok(Some(nearest)) if nearest.session_id != session.session_id => {
            Err(format!(
                "process {caller_pid} is bound to nearer App session `{}` ({}) \
                 and cannot select ancestor session `{}`",
                nearest.session_id,
                nearest.app_id.as_deref().unwrap_or("unknown"),
                session.session_id
            ))
        }
        Ok(_) => Ok(()),
        Err(()) if strict => Err(
            "could not determine the nearest App identity in the process tree"
                .to_string(),
        ),
        Err(()) => Ok(()),
    }
}

fn nearest_app_session(
    registry: &Registry,
    caller_pid: u32,
) -> Result<Option<&SessionRow>, ()> {
    if !registry
        .sessions
        .iter()
        .any(|session| session.app_id.is_some() && app_session_process_is_current(session))
    {
        return Ok(None);
    }
    let mut current = caller_pid;
    for _ in 0..64 {
        if let Some(session) = registry.sessions.iter().find(|session| {
            session.app_id.is_some()
                && !session.pending_bind
                && session.pid == current
                && app_session_process_is_current(session)
        }) {
            return Ok(Some(session));
        }
        if current <= 1 {
            return Ok(None);
        }
        current = pid_ancestry::parent_pid(current).ok_or(())?;
    }
    Err(())
}

fn app_session_process_is_current(session: &SessionRow) -> bool {
    if session.pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Some(expected) = session.start_time_ticks else {
            return false;
        };
        crate::proc::read_start_time_ticks_pub(session.pid) == Some(expected)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Check whether the current session may perform `verb` against `scope`.
///
/// Returns `Ok(())` if the cap is held (or if the active enforcement
/// mode permits it). Returns `Err(Denial)` with a structured reason
/// otherwise — callers can re-raise it as a JSON error or feed it into
/// the approval UI.
///
/// Every call emits one structured record to `${log_dir}/caps.jsonl`
/// via [`crate::audit::log_cap_decision`]. Emission is unconditional
/// and covers allows and denials alike; the process being checked has
/// no switch that suppresses either class.
pub fn require(verb: Verb, scope: Scope) -> Result<(), Denial> {
    let mode = Mode::from_env();
    let session_id = crate::proc::current_session_id();
    let mut result = require_impl(verb, scope.clone(), mode, session_id.as_deref());
    if let Err(denial) = &mut result {
        attach_approval_request(denial, mode, session_id.as_deref());
    }
    crate::audit::log_cap_decision(build_cap_audit_record(
        verb,
        &scope,
        mode,
        session_id.as_deref(),
        &result,
    ));
    result
}

fn attach_approval_request(denial: &mut Denial, mode: Mode, session_id: Option<&str>) {
    if mode != Mode::Strict
        || matches!(
            denial.reason,
            DenialReason::NoSession | DenialReason::PidAncestryMismatch { .. }
        )
    {
        return;
    }
    let Some(session_id) = session_id.filter(|value| !value.is_empty()) else {
        return;
    };
    let Some(meta) = super::catalog::lookup(denial.verb) else {
        return;
    };
    if meta.risk < Risk::High {
        return;
    }

    let is_app = crate::proc::current_trusted_session_for_caps()
        .filter(|session| session.session_id == session_id)
        .and_then(|session| session.app_id)
        .is_some()
        || load_registry()
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .and_then(|session| session.app_id.as_ref())
            .is_some();
    if is_app {
        return;
    }

    let owner_uid = crate::paths::current_owner_uid_override().or_else(current_euid);
    let existing = crate::approvals::list_pending_for_owner(owner_uid)
        .into_iter()
        .find(|request| {
            request.session == session_id
                && request.verb == denial.verb.as_str()
                && request.scope.covers(&denial.requested_scope)
        });
    let request_id = match existing {
        Some(request) => Ok(request.id),
        None => crate::approvals::submit_owned(
            denial.verb,
            denial.requested_scope.clone(),
            session_id,
            format!(
                "{}: {}",
                meta.label.current(),
                denial.requested_scope
            ),
            Some("system-agent".to_string()),
            owner_uid,
        ),
    };
    match request_id {
        Ok(id) => {
            denial.hint = Some(format!(
                "approval request {id} is pending; approve it in Claw OS, then retry"
            ));
        }
        Err(error) => {
            denial.hint = Some(format!("could not create approval request: {error}"));
        }
    }
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn current_euid() -> Option<u32> {
    None
}

fn require_impl(
    verb: Verb,
    scope: Scope,
    mode: Mode,
    session_id: Option<&str>,
) -> Result<(), Denial> {
    let session_id = match session_id {
        Some(s) if !s.is_empty() => s,
        _ => {
            return match mode {
                Mode::Permissive => Ok(()),
                Mode::Strict => Err(Denial::no_session(verb, scope)
                    .with_hint("set COS_SESSION before invoking gated operations")),
            }
        }
    };

    if let Some(session) = crate::proc::current_trusted_session_for_caps() {
        if session.session_id != session_id {
            return Err(Denial::no_session(verb, scope).with_hint(
                "trusted task session does not match the selected session id",
            ));
        }
        return authorize_session_caps(
            session_id,
            verb,
            scope,
            mode,
            session.caps.as_ref(),
            session.transient_caps.as_ref(),
            session.app_id.is_some(),
        );
    }

    let registry = load_registry();
    let session = match registry
        .sessions
        .iter()
        .find(|s| s.session_id == session_id)
    {
        Some(s) => s,
        None => {
            return match mode {
                Mode::Permissive => Ok(()),
                Mode::Strict => Err(Denial::no_session(verb, scope).with_hint(format!(
                    "session `{session_id}` is not registered in the process registry"
                ))),
            }
        }
    };

    let caller_pid = std::process::id();
    if let Err(error) = validate_process_identity(
        &registry,
        session,
        caller_pid,
        matches!(mode, Mode::Strict),
    ) {
        return Err(
            Denial::pid_ancestry_mismatch(verb, scope, caller_pid, session.pid)
                .with_hint(error),
        );
    }

    authorize_session_caps(
        session_id,
        verb,
        scope,
        mode,
        session.caps.as_ref(),
        session.transient_caps.as_ref(),
        session.app_id.is_some(),
    )
}

fn authorize_session_caps(
    session_id: &str,
    verb: Verb,
    scope: Scope,
    mode: Mode,
    caps: Option<&CapSet>,
    transient_caps: Option<&CapSet>,
    is_app: bool,
) -> Result<(), Denial> {
    let requested = Cap::new(verb, scope.clone());
    let mut caps = match caps {
        Some(c) => c.clone(),
        None => {
            return match mode {
                Mode::Permissive => Ok(()),
                Mode::Strict => Err(Denial::verb_not_granted(verb, scope).with_hint(format!(
                    "session `{session_id}` has no caps field; spawn it with `cos session start \
                     --role <role>` to enrol it in the new permission system"
                ))),
            }
        }
    };
    if let Some(transient) = transient_caps {
        caps.extend(transient.iter().cloned());
    }

    if caps.covers(&requested)
        || (!is_app && approved_grant_covers(session_id, verb, &scope))
    {
        Ok(())
    } else if caps.verbs().contains(&verb) {
        // Verb is held but at a scope that doesn't cover this request.
        Err(Denial::scope_out_of_range(verb, scope, &caps))
    } else {
        Err(Denial::verb_not_granted(verb, scope))
    }
}

fn approved_grant_covers(session_id: &str, verb: Verb, scope: &Scope) -> bool {
    match crate::approvals::consume_matching_grant_for_owner(
        session_id,
        verb,
        scope,
        crate::paths::current_owner_uid_override(),
    ) {
        Ok(Some(_duration)) => true,
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(
                session_id,
                verb = %verb.as_str(),
                error = %err,
                "failed to inspect approved permission grants"
            );
            false
        }
    }
}

/// Build the JSON record emitted to `caps.jsonl` for one decision.
///
/// Kept side-effect free so unit tests can inspect the structure
/// without touching disk.
fn build_cap_audit_record(
    verb: Verb,
    scope: &Scope,
    mode: Mode,
    session_id: Option<&str>,
    result: &Result<(), Denial>,
) -> serde_json::Value {
    let agent = std::env::var("COS_AGENT_LABEL")
        .ok()
        .or_else(|| std::env::var("COS_APP_ID").ok());

    let (decision, reason, hint) = match result {
        Ok(()) => ("allow", serde_json::Value::Null, serde_json::Value::Null),
        Err(d) => (
            "deny",
            serde_json::to_value(&d.reason).unwrap_or(serde_json::Value::Null),
            d.hint
                .as_ref()
                .map(|h| serde_json::Value::String(h.clone()))
                .unwrap_or(serde_json::Value::Null),
        ),
    };

    let target_resource = match scope {
        Scope::Path(s) | Scope::Host(s) | Scope::Name(s) | Scope::SelfRef(s) => s.clone(),
        Scope::Wild => "*".to_string(),
    };

    serde_json::json!({
        "session_id":      session_id,
        "pid":             std::process::id(),
        "agent":           agent,
        "verb":            verb.as_str(),
        "scope":           scope,
        "target_resource": target_resource,
        "decision":        decision,
        "reason":          reason,
        "hint":            hint,
        "mode":            mode.as_str(),
        "owner_uid":       crate::paths::current_owner_uid_override(),
    })
}

/// Variant that converts the structured [`Denial`] into the same
/// JSON envelope the legacy `policy::require` returns. Call sites
/// migrating from `policy::require(OpType::X)` can swap to
/// `require_or_json(Verb::FS_READ, Scope::path(p))` with minimal
/// downstream churn.
pub fn require_or_json(verb: Verb, scope: Scope) -> Result<(), serde_json::Value> {
    require(verb, scope).map_err(|d| d.to_json())
}

// ---------------------------------------------------------------------------
// PID ancestry (cross-platform)
// ---------------------------------------------------------------------------

mod pid_ancestry {
    /// Outcome of a single descendancy check. Distinguishing
    /// `Unsupported` from `No` matters because the caller chooses to
    /// fail-closed in strict mode on `Unsupported` while still
    /// allowing other paths (e.g. permissive testing) to proceed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AncestryResult {
        Yes,
        No,
        Unsupported,
    }

    pub fn is_descendant_of(child: u32, ancestor: u32) -> AncestryResult {
        let mut current = child;
        for _ in 0..64 {
            if current == ancestor {
                return AncestryResult::Yes;
            }
            if current <= 1 {
                return AncestryResult::No;
            }
            match read_ppid(current) {
                Some(ppid) => current = ppid,
                None if PPID_SUPPORTED => return AncestryResult::No,
                None => return AncestryResult::Unsupported,
            }
        }
        AncestryResult::No
    }

    pub(super) fn parent_pid(pid: u32) -> Option<u32> {
        if !PPID_SUPPORTED {
            return None;
        }
        read_ppid(pid)
    }

    #[cfg(target_os = "linux")]
    const PPID_SUPPORTED: bool = true;
    #[cfg(target_os = "linux")]
    fn read_ppid(pid: u32) -> Option<u32> {
        let path = format!("/proc/{pid}/status");
        let content = std::fs::read_to_string(path).ok()?;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("PPid:") {
                return val.trim().parse().ok();
            }
        }
        None
    }

    /// macOS: sysctl KERN_PROC for PPID. We invoke `libc::sysctl`
    /// rather than depend on a third-party crate. The MIB layout is
    /// stable across macOS releases (documented in
    /// `man 3 sysctl`).
    #[cfg(target_os = "macos")]
    const PPID_SUPPORTED: bool = true;
    #[cfg(target_os = "macos")]
    fn read_ppid(pid: u32) -> Option<u32> {
        // CTL_KERN, KERN_PROC, KERN_PROC_PID, <pid>
        const CTL_KERN: libc::c_int = 1;
        const KERN_PROC: libc::c_int = 14;
        const KERN_PROC_PID: libc::c_int = 1;
        let mut mib: [libc::c_int; 4] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid as libc::c_int];

        // kinfo_proc is large (~648 bytes) and its layout is private
        // to <sys/sysctl.h>; we read into an opaque buffer and use
        // the documented byte offset of kp_eproc.e_ppid. The pid
        // lives at offset 8 inside the embedded extern_proc, then
        // e_ppid is the first field of kp_eproc. To keep this
        // resilient, ask the kernel for the buffer size first.
        let mut size: libc::size_t = 0;
        // SAFETY: mib is a valid 4-element array; passing null
        // oldp/oldlenp to sysctl asks for the required size.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size];
        // SAFETY: buf is sized to `size` as told by the kernel.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        // kinfo_proc layout (from <sys/sysctl.h> kp_eproc.e_ppid):
        //   struct extern_proc kp_proc;   // sizeof = 296 on x86_64,
        //                                 // sizeof = 296 on arm64
        //   struct eproc        kp_eproc; // first field is struct proc * e_paddr
        // The ppid (e_ppid) is at offset 24 inside kp_eproc, which
        // itself starts at offset 296 inside kinfo_proc:
        //   296 + 24 = 320 (x86_64)
        //   296 + 24 = 320 (arm64)
        const EPROC_PPID_OFFSET: usize = 320;
        if size < EPROC_PPID_OFFSET + 4 {
            return None;
        }
        let ppid_bytes: [u8; 4] = buf[EPROC_PPID_OFFSET..EPROC_PPID_OFFSET + 4]
            .try_into()
            .ok()?;
        let ppid = u32::from_ne_bytes(ppid_bytes);
        Some(ppid)
    }

    /// Other Unix targets (BSDs, illumos, etc.): we don't currently
    /// know how to walk the process tree without pulling extra
    /// crates. Report `Unsupported` so strict mode fails closed.
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    const PPID_SUPPORTED: bool = false;
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    fn read_ppid(_pid: u32) -> Option<u32> {
        None
    }

    /// Windows: walk the process tree via `Process32First` /
    /// `Process32Next` over the toolhelp snapshot. Implementation is
    /// gated behind a `windows-sys`-shaped cfg so the rest of the
    /// kernel stays portable; until that dependency lands we report
    /// `Unsupported`, which forces strict mode to deny rather than
    /// silently allow.
    #[cfg(target_os = "windows")]
    const PPID_SUPPORTED: bool = false;
    #[cfg(target_os = "windows")]
    fn read_ppid(_pid: u32) -> Option<u32> {
        None
    }
}

use pid_ancestry::AncestryResult;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/enforcement.rs"
    ));
}
