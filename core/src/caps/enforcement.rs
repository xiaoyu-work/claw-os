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
use super::denial::Denial;
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
}

#[derive(Deserialize, Default)]
struct Registry {
    #[serde(default)]
    sessions: Vec<SessionRow>,
}

fn registry_path() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("proc")
        .join("registry.json")
}

fn load_registry() -> Registry {
    std::fs::read_to_string(registry_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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
/// via [`crate::audit::log_cap_decision`]. Suppress with
/// `COS_CAPS_AUDIT=0`.
pub fn require(verb: Verb, scope: Scope) -> Result<(), Denial> {
    let mode = Mode::from_env();
    let session_id = std::env::var("COS_SESSION").ok();
    let result = require_impl(verb, scope.clone(), mode, session_id.as_deref());
    crate::audit::log_cap_decision(build_cap_audit_record(
        verb,
        &scope,
        mode,
        session_id.as_deref(),
        &result,
    ));
    result
}

fn require_impl(
    verb: Verb,
    scope: Scope,
    mode: Mode,
    session_id: Option<&str>,
) -> Result<(), Denial> {
    let requested = Cap::new(verb, scope.clone());

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

    // PID-ancestry check: the caller must live in the session's process
    // tree. This is the OS-level defence against COS_SESSION spoofing.
    // We perform this check on every platform where we know how to
    // walk the process tree. On unsupported platforms we **fail
    // closed** in strict mode rather than silently skipping the
    // check — a stolen `COS_SESSION` should not become "free admin"
    // just because the user is on a less-tested OS.
    let caller_pid = std::process::id();
    if session.pid != 0 && caller_pid != session.pid {
        match pid_ancestry::is_descendant_of(caller_pid, session.pid) {
            AncestryResult::Yes => {}
            AncestryResult::No => {
                return Err(
                    Denial::pid_ancestry_mismatch(verb, scope, caller_pid, session.pid).with_hint(
                        "the COS_SESSION env var does not match the process tree; \
                     do not set it manually",
                    ),
                );
            }
            AncestryResult::Unsupported => {
                if matches!(mode, Mode::Strict) {
                    return Err(Denial::pid_ancestry_mismatch(
                        verb,
                        scope,
                        caller_pid,
                        session.pid,
                    )
                    .with_hint(
                        "pid-ancestry checking is not implemented on this platform; \
                         strict-mode caps refuse to skip the check",
                    ));
                }
            }
        }
    }

    let caps = match session.caps.as_ref() {
        Some(c) => c,
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

    if caps.covers(&requested) {
        Ok(())
    } else if approved_grant_covers(session_id, verb, &scope) {
        Ok(())
    } else if caps.verbs().contains(&verb) {
        // Verb is held but at a scope that doesn't cover this request.
        Err(Denial::scope_out_of_range(verb, scope, caps))
    } else {
        Err(Denial::verb_not_granted(verb, scope))
    }
}

fn approved_grant_covers(session_id: &str, verb: Verb, scope: &Scope) -> bool {
    match crate::approvals::consume_matching_grant(session_id, verb, scope) {
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
    use super::*;
    use std::io::Write;

    /// Test guard that sets `COS_DATA_DIR` to a fresh tmp dir, writes a
    /// registry JSON, sets `COS_SESSION` + `COS_PERMS_MODE`, and
    /// restores the previous env on drop.
    ///
    /// Because Rust runs unit tests in parallel by default and env vars
    /// are process-global, callers must serialise via the EnvLock
    /// mutex below.
    struct EnvGuard {
        prev_data_dir: Option<String>,
        prev_log_dir: Option<String>,
        prev_session: Option<String>,
        prev_mode: Option<String>,
        _tmp: tempfile::TempDir,
    }

    impl EnvGuard {
        fn new(registry_json: &str, session: Option<&str>, mode: Option<&str>) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let proc_dir = tmp.path().join("proc");
            std::fs::create_dir_all(&proc_dir).unwrap();
            let mut f = std::fs::File::create(proc_dir.join("registry.json")).unwrap();
            f.write_all(registry_json.as_bytes()).unwrap();

            let prev_data_dir = std::env::var("COS_DATA_DIR").ok();
            let prev_log_dir = std::env::var("COS_LOG_DIR").ok();
            let prev_session = std::env::var("COS_SESSION").ok();
            let prev_mode = std::env::var("COS_PERMS_MODE").ok();

            std::env::set_var("COS_DATA_DIR", tmp.path());
            // Redirect caps.jsonl writes into the test tmpdir so the
            // audit hook doesn't litter the host's logs dir.
            std::env::set_var("COS_LOG_DIR", tmp.path());
            match session {
                Some(s) => std::env::set_var("COS_SESSION", s),
                None => std::env::remove_var("COS_SESSION"),
            }
            match mode {
                Some(m) => std::env::set_var("COS_PERMS_MODE", m),
                None => std::env::remove_var("COS_PERMS_MODE"),
            }
            Self {
                prev_data_dir,
                prev_log_dir,
                prev_session,
                prev_mode,
                _tmp: tmp,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev_data_dir {
                Some(v) => std::env::set_var("COS_DATA_DIR", v),
                None => std::env::remove_var("COS_DATA_DIR"),
            }
            match &self.prev_log_dir {
                Some(v) => std::env::set_var("COS_LOG_DIR", v),
                None => std::env::remove_var("COS_LOG_DIR"),
            }
            match &self.prev_session {
                Some(v) => std::env::set_var("COS_SESSION", v),
                None => std::env::remove_var("COS_SESSION"),
            }
            match &self.prev_mode {
                Some(v) => std::env::set_var("COS_PERMS_MODE", v),
                None => std::env::remove_var("COS_PERMS_MODE"),
            }
        }
    }

    // Single-threaded test mutex — these tests mutate process env.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn registry_with_caps(sid: &str, caps_json: &str) -> String {
        // pid=0 disables the ancestry check (see the require() body).
        format!(
            r#"{{
              "sessions": [
                {{
                  "session_id": "{sid}",
                  "pid": 0,
                  "caps": {caps_json}
                }}
              ]
            }}"#
        )
    }

    #[test]
    fn permissive_allows_when_no_session() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, Some("permissive"));
        assert!(require(Verb::FS_READ, Scope::path("/etc")).is_ok());
    }

    #[test]
    fn strict_denies_when_no_session() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, Some("strict"));
        let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::NoSession
        ));
    }

    #[test]
    fn strict_denies_unknown_session() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::new(r#"{"sessions":[]}"#, Some("missing"), Some("strict"));
        let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::NoSession
        ));
    }

    #[test]
    fn allows_when_session_caps_cover_request() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        assert!(require(Verb::FS_READ, Scope::path("/home/jay/notes.md")).is_ok());
    }

    #[test]
    fn denies_with_scope_out_of_range_when_verb_held_but_path_outside() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let err = require(Verb::FS_READ, Scope::path("/etc/passwd")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::ScopeOutOfRange
        ));
        // The granted_scopes echo back exactly what the session holds.
        assert_eq!(err.granted_scopes.len(), 1);
    }

    #[test]
    fn denies_with_verb_not_granted_when_verb_missing() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let err = require(Verb::FS_DELETE, Scope::path("/home/jay/x")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::VerbNotGranted
        ));
    }

    #[test]
    fn approved_once_grant_allows_exactly_one_denied_request() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let id = crate::approvals::submit(
            Verb::FS_WRITE,
            Scope::path("/tmp/granted/**"),
            "s1",
            "test grant",
            None,
        )
        .unwrap();
        crate::approvals::approve(&id, crate::approvals::GrantDuration::Once, None, None).unwrap();

        assert!(require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).is_ok());
        let err = require(Verb::FS_WRITE, Scope::path("/tmp/granted/file")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::VerbNotGranted
        ));
    }

    #[test]
    fn permissive_allows_session_without_caps_field() {
        let _lock = ENV_LOCK.lock().unwrap();
        let reg = r#"{
          "sessions": [{"session_id":"s1","pid":0}]
        }"#;
        let _g = EnvGuard::new(reg, Some("s1"), Some("permissive"));
        assert!(require(Verb::FS_READ, Scope::path("/etc")).is_ok());
    }

    #[test]
    fn strict_is_the_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _g = EnvGuard::new(r#"{"sessions":[]}"#, None, None);
        // No COS_PERMS_MODE set → strict by default → denies.
        let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::NoSession
        ));
    }

    #[test]
    fn strict_denies_session_without_caps_field() {
        let _lock = ENV_LOCK.lock().unwrap();
        let reg = r#"{
          "sessions": [{"session_id":"s1","pid":0}]
        }"#;
        let _g = EnvGuard::new(reg, Some("s1"), Some("strict"));
        let err = require(Verb::FS_READ, Scope::path("/etc")).unwrap_err();
        assert!(matches!(
            err.reason,
            super::super::denial::DenialReason::VerbNotGranted
        ));
    }

    #[test]
    fn json_envelope_matches_denial_shape() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let err = require_or_json(Verb::FS_DELETE, Scope::path("/etc")).unwrap_err();
        assert_eq!(err["error"], "permission denied");
        assert_eq!(err["verb"], "fs.delete");
    }

    // ----- audit-record shape ------------------------------------------------

    #[test]
    fn audit_record_allow_carries_decision_verb_and_target() {
        let scope = Scope::path("/home/jay/notes.md");
        let rec = build_cap_audit_record(Verb::FS_READ, &scope, Mode::Strict, Some("s1"), &Ok(()));
        assert_eq!(rec["decision"], "allow");
        assert_eq!(rec["verb"], "fs.read");
        assert_eq!(rec["session_id"], "s1");
        assert_eq!(rec["mode"], "strict");
        assert_eq!(rec["target_resource"], "/home/jay/notes.md");
        assert_eq!(rec["scope"]["kind"], "path");
        assert_eq!(rec["scope"]["value"], "/home/jay/notes.md");
        assert!(rec["reason"].is_null());
        assert!(rec["hint"].is_null());
    }

    #[test]
    fn audit_record_deny_emits_reason_and_hint() {
        let scope = Scope::path("/etc/passwd");
        let denial = super::super::denial::Denial::verb_not_granted(Verb::FS_DELETE, scope.clone())
            .with_hint("ask the user");
        let rec = build_cap_audit_record(Verb::FS_DELETE, &scope, Mode::Strict, None, &Err(denial));
        assert_eq!(rec["decision"], "deny");
        assert_eq!(rec["reason"], "verb-not-granted");
        assert_eq!(rec["hint"], "ask the user");
        assert!(rec["session_id"].is_null());
    }

    #[test]
    fn audit_record_wild_scope_renders_as_star() {
        let scope = Scope::wild();
        let rec = build_cap_audit_record(Verb::FS_READ, &scope, Mode::Permissive, None, &Ok(()));
        assert_eq!(rec["target_resource"], "*");
        assert_eq!(rec["scope"]["kind"], "wild");
        assert_eq!(rec["mode"], "permissive");
    }

    #[test]
    fn require_writes_to_caps_jsonl() {
        let _lock = ENV_LOCK.lock().unwrap();
        let prev_audit = std::env::var_os("COS_CAPS_AUDIT");
        std::env::remove_var("COS_CAPS_AUDIT");

        let caps = r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));

        // EnvGuard redirects COS_LOG_DIR to its tempdir, so writes
        // land at <tmp>/caps.jsonl. One allow + one deny → two lines.
        let _ = require(Verb::FS_READ, Scope::path("/home/jay/x"));
        let _ = require(Verb::FS_DELETE, Scope::path("/home/jay/x"));

        let body = std::fs::read_to_string(crate::paths::caps_audit_log_path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "expected two audit lines, got {body:?}");
        let allow: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let deny: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(allow["decision"], "allow");
        assert_eq!(allow["verb"], "fs.read");
        assert_eq!(deny["decision"], "deny");
        assert_eq!(deny["verb"], "fs.delete");
        assert_eq!(deny["reason"], "verb-not-granted");

        match prev_audit {
            Some(v) => std::env::set_var("COS_CAPS_AUDIT", v),
            None => std::env::remove_var("COS_CAPS_AUDIT"),
        }
    }
}
