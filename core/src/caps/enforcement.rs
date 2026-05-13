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
pub fn require(verb: Verb, scope: Scope) -> Result<(), Denial> {
    let mode = Mode::from_env();
    let requested = Cap::new(verb, scope.clone());

    let session_id = match std::env::var("COS_SESSION") {
        Ok(s) if !s.is_empty() => s,
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
    #[cfg(target_os = "linux")]
    {
        let caller_pid = std::process::id();
        if session.pid != 0 && !is_pid_descendant_of(caller_pid, session.pid) {
            return Err(Denial::pid_ancestry_mismatch(
                verb,
                scope,
                caller_pid,
                session.pid,
            )
            .with_hint(
                "the COS_SESSION env var does not match the process tree; \
                 do not set it manually",
            ));
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
    } else if caps.verbs().contains(&verb) {
        // Verb is held but at a scope that doesn't cover this request.
        Err(Denial::scope_out_of_range(verb, scope, caps))
    } else {
        Err(Denial::verb_not_granted(verb, scope))
    }
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
// PID ancestry (Linux)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn is_pid_descendant_of(child_pid: u32, ancestor_pid: u32) -> bool {
    let mut current = child_pid;
    for _ in 0..64 {
        if current == ancestor_pid {
            return true;
        }
        if current <= 1 {
            return false;
        }
        match read_ppid(current) {
            Some(ppid) => current = ppid,
            None => return false,
        }
    }
    false
}

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
            let prev_session = std::env::var("COS_SESSION").ok();
            let prev_mode = std::env::var("COS_PERMS_MODE").ok();

            std::env::set_var("COS_DATA_DIR", tmp.path());
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
        let caps =
            r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        assert!(require(Verb::FS_READ, Scope::path("/home/jay/notes.md")).is_ok());
    }

    #[test]
    fn denies_with_scope_out_of_range_when_verb_held_but_path_outside() {
        let _lock = ENV_LOCK.lock().unwrap();
        let caps =
            r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
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
        let caps =
            r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let err = require(Verb::FS_DELETE, Scope::path("/home/jay/x")).unwrap_err();
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
        let caps =
            r#"[{"verb":"fs.read","scope":{"kind":"path","value":"/home/jay/**"}}]"#;
        let reg = registry_with_caps("s1", caps);
        let _g = EnvGuard::new(&reg, Some("s1"), Some("strict"));
        let err = require_or_json(Verb::FS_DELETE, Scope::path("/etc")).unwrap_err();
        assert_eq!(err["error"], "permission denied");
        assert_eq!(err["verb"], "fs.delete");
    }
}
