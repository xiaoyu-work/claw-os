//! Auto-bootstrap a user-CLI session when `cos` is invoked
//! interactively (or by any tool) without an upstream `COS_SESSION`.
//!
//! ## Why
//!
//! Strict-mode caps enforcement requires every gated call to come from
//! a known session in the proc registry. The canonical session-creation
//! path is `cos proc spawn`, which sets `COS_SESSION` for its child.
//! But when a real human runs `cos agent setup` or `cos agent chat`
//! from a shell, there is no upstream session: the user just typed the
//! command at a TTY. Without a fix every gated call would deny with
//! "Permission denied (no active session)".
//!
//! ## What this does
//!
//! At process start [`bootstrap_user_cli_session`] checks whether
//! `COS_SESSION` is unset. If so it:
//!
//! 1. Builds a [`crate::proc::SessionInfo`] with `role = Admin` and a
//!    wild-scoped [`crate::caps::CapSet`] (single-user desktop OS:
//!    anyone with shell access already has full power).
//! 2. Writes it into the proc registry so
//!    [`crate::caps::enforcement::require`] can find it.
//! 3. Sets `COS_SESSION` in the process environment so subsequent
//!    capability checks (in this process and any direct children that
//!    inherit the env) pick it up.
//! 4. Returns a [`SessionGuard`] whose `Drop` impl removes the row
//!    from the registry on clean exit. Crashes leave a ghost row;
//!    `cos proc list` GCs stale entries via `is_alive(pid)`, so this
//!    self-heals over time.
//!
//! ## Fallback
//!
//! If the registry write fails (e.g. the data dir is read-only or
//! does not yet exist and cannot be created), the bootstrap silently
//! falls back to setting `COS_PERMS_MODE=permissive`. That preserves
//! the user experience — gated calls succeed — at the cost of the
//! strict-mode audit trail for this one process.

use std::env;

use crate::caps::role::Role;
use crate::caps::scope::Scope;
use crate::proc::{deregister_session, register_session, SessionInfo};

/// RAII guard that removes the bootstrapped session row from the
/// proc registry on `Drop`. Hold it for the lifetime of `main()` so
/// the row exists exactly as long as the CLI process does.
pub struct SessionGuard {
    session_id: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        deregister_session(&self.session_id);
        env::remove_var("COS_SESSION");
    }
}

/// Bootstrap a CLI session if `COS_SESSION` is unset. Returns a guard
/// holding the registered session id, or `None` if no work was done
/// (because `COS_SESSION` was already set by an upstream caller).
///
/// Idempotent: if invoked twice the second call no-ops.
pub fn bootstrap_user_cli_session() -> Option<SessionGuard> {
    if env::var_os("COS_SESSION").is_some_and(|v| !v.is_empty()) {
        return None;
    }

    let pid = std::process::id();
    let session_id = format!("cli-{}-{}", pid, short_random_suffix());

    let caps = Role::Admin.caps_with_scopes(
        Some(Scope::Wild),
        Some(Scope::Wild),
        Some(Scope::Wild),
    );

    let info = SessionInfo {
        session_id: session_id.clone(),
        pid,
        command: env::args().collect(),
        started_at: now_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: None,
        parent: None,
        workdir: env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string()),
        exit_code: None,
        ended_at: None,
        tier: None,
        scope: None,
        priority: None,
        caps: Some(caps),
        role: Some(Role::Admin.name().to_string()),
    };

    match register_session(info) {
        Ok(()) => {
            env::set_var("COS_SESSION", &session_id);
            Some(SessionGuard { session_id })
        }
        Err(_) => {
            // Registry write failed (e.g. read-only data dir). Fall
            // back to permissive mode so the user still gets a
            // working CLI. The strict-mode audit trail is lost for
            // this one process; that is the explicit trade-off.
            env::set_var("COS_PERMS_MODE", "permissive");
            None
        }
    }
}

fn short_random_suffix() -> String {
    // 8 hex chars from the system clock + pid mixed in. Not
    // cryptographically random; we just need to avoid id collisions
    // when two cos processes start in the same second.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}", nanos ^ pid)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Bootstrap mutates global env vars; serialise tests on a mutex so
    // they don't race each other (and don't trample the surrounding
    // env that the rest of the test binary inherits).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Returns a fresh tempdir and sets `COS_DATA_DIR` to point at it.
    /// Restores any previous value when the guard is dropped.
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("COS_DATA_DIR");
        env::set_var("COS_DATA_DIR", tmp.path());
        DataDirGuard { prev, _tmp: tmp }
    }

    #[test]
    fn bootstrap_is_noop_when_session_already_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _data = redirect_data_dir();
        let prev = env::var("COS_SESSION").ok();
        env::set_var("COS_SESSION", "outer-session-id");

        let guard = bootstrap_user_cli_session();
        assert!(guard.is_none(), "should not bootstrap when COS_SESSION is set");
        assert_eq!(env::var("COS_SESSION").unwrap(), "outer-session-id");

        match prev {
            Some(v) => env::set_var("COS_SESSION", v),
            None => env::remove_var("COS_SESSION"),
        }
    }

    #[test]
    fn bootstrap_registers_session_and_grants_ai_chat() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _data = redirect_data_dir();
        env::remove_var("COS_SESSION");
        env::remove_var("COS_PERMS_MODE");

        let guard = bootstrap_user_cli_session().expect("bootstrap should succeed");
        let sid = env::var("COS_SESSION").expect("COS_SESSION should be set");
        assert!(sid.starts_with("cli-"), "session id format: {sid}");

        // The freshly-bootstrapped session must satisfy the caps gate
        // for ai.chat — the failure mode that motivated this whole
        // module. We can't call `caps::require` directly here because
        // it requires the audit log dir to exist and resolves the
        // session via the same registry we just wrote, so we just
        // sanity-check that the admin verbs are present in the row.
        use crate::caps::Verb;
        let reg_path = crate::paths::data_dir().join("proc/registry.json");
        let raw = std::fs::read_to_string(&reg_path).expect("registry exists");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let caps = v["sessions"][0]["caps"].as_array().expect("caps array");
        let has_ai_chat = caps.iter().any(|c| {
            c["verb"].as_str() == Some(Verb::AI_CHAT.as_str())
        });
        assert!(has_ai_chat, "admin session should hold ai.chat");

        drop(guard);
        // After drop the row is gone and COS_SESSION is cleared.
        assert!(env::var("COS_SESSION").is_err());
        let raw_after = std::fs::read_to_string(&reg_path).unwrap();
        let v_after: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
        assert_eq!(
            v_after["sessions"].as_array().map(|a| a.len()).unwrap_or(0),
            0
        );
    }
}
