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
//! ## Failure handling (fail-closed)
//!
//! Earlier revisions of this module silently demoted the process to
//! `COS_PERMS_MODE=permissive` if the registry write failed. That is
//! the wrong default for a security-oriented kernel: a corrupted data
//! dir would invisibly turn every gated call into a yes. We now
//! return an error from the bootstrap and let `main()` surface it to
//! the user, who can decide whether to retry, fix permissions, or
//! explicitly run with `COS_PERMS_MODE=permissive` set in the
//! environment.

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
/// The session row is written through [`crate::proc::register_session`],
/// which resolves to `$HOME/.local/share/cos/proc/registry.json` for
/// the cos CLI (a per-user, XDG-compliant location — same pattern as
/// the agent config and credentials store, commit 271ef962). clawd's
/// systemd unit explicitly sets `COS_DATA_DIR=/var/lib/cos` so the
/// system daemon's task registrations remain isolated from any user
/// CLI session.
///
/// On registry-write failure this function returns `None` and **does
/// not** demote to permissive mode — see the module docs.
///
/// Idempotent: if invoked twice the second call no-ops.
pub fn bootstrap_user_cli_session() -> Option<SessionGuard> {
    if env::var_os("COS_SESSION").is_some_and(|v| !v.is_empty()) {
        return None;
    }

    let pid = std::process::id();
    let session_id = format!("cli-{}-{}", pid, fresh_session_suffix());

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
        start_time_ticks: None,
    };

    match register_session(info) {
        Ok(()) => {
            env::set_var("COS_SESSION", &session_id);
            Some(SessionGuard { session_id })
        }
        Err(e) => {
            // Fail closed. We deliberately do **not** demote to
            // permissive mode here. Surfacing the failure means the
            // user sees "Permission denied (no active session)" on
            // gated calls — the *correct* signal that something is
            // wrong with the kernel state, rather than the silent
            // open-door we used to have.
            tracing::error!(
                target: "cos::caps::bootstrap",
                error = %e,
                "failed to register CLI session in proc registry; \
                 gated calls will deny in strict mode"
            );
            None
        }
    }
}

/// Generate a fresh, collision-resistant suffix for the CLI session
/// id. We previously used a 32-bit `(nanos ^ pid)` value that easily
/// collided when two `cos` invocations fired in the same nanosecond
/// or when a forked child reused its parent's pid. UUIDv4 gives us
/// 122 bits of entropy from the OS RNG, which is comfortably above
/// the collision-resistance threshold even if a script kicks off
/// thousands of CLI invocations per second.
fn fresh_session_suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bootstrap mutates global env vars; serialise tests on the
    // shared `caps::test_env_lock` so they don't race other modules
    // (notably `caps::enforcement`) that mutate the same variables.
    use crate::caps::test_env_lock::env_lock;

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
        let _lock = env_lock();
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
        let _lock = env_lock();
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

    /// Per-user data dir override: sets `COS_USER_DATA_DIR` to a
    /// fresh tempdir so the per-user-default test isn't subject to
    /// the real `$HOME/.local/share/cos` contents.
    struct UserDataDirGuard {
        prev: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
    }
    impl Drop for UserDataDirGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => env::set_var("COS_USER_DATA_DIR", v),
                None => env::remove_var("COS_USER_DATA_DIR"),
            }
        }
    }
    fn redirect_user_data_dir() -> UserDataDirGuard {
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("COS_USER_DATA_DIR");
        env::set_var("COS_USER_DATA_DIR", tmp.path());
        UserDataDirGuard { prev, _tmp: tmp }
    }

    /// When `COS_DATA_DIR` is unset (the normal case for a user
    /// invoking `cos` from a shell), the bootstrapped session row
    /// must land in the per-user data dir — *not* `/var/lib/cos`,
    /// which is `root:root 0755` on a real install. This is the
    /// regression that produced
    ///   `Permission denied (no active session): secret.write on *`
    /// when `cos agent setup llm` tried to store the GitHub Copilot
    /// OAuth token.
    #[test]
    fn bootstrap_writes_to_user_data_dir_when_cos_data_dir_unset() {
        let _lock = env_lock();
        let prev_data = env::var_os("COS_DATA_DIR");
        env::remove_var("COS_DATA_DIR");
        let user_data = redirect_user_data_dir();
        env::remove_var("COS_SESSION");
        env::remove_var("COS_PERMS_MODE");

        let guard = bootstrap_user_cli_session()
            .expect("bootstrap should succeed in per-user data dir");
        let sid = env::var("COS_SESSION").expect("COS_SESSION should be set");

        // The row must exist under the user data dir, not under any
        // /var/lib/cos path. Reading user_data_dir() directly to avoid
        // depending on the test guard's internal tempdir handle.
        let user_reg = crate::paths::user_data_dir().join("proc/registry.json");
        let raw = std::fs::read_to_string(&user_reg)
            .expect("user-data-dir registry should exist after bootstrap");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let sessions = v["sessions"].as_array().expect("sessions array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["session_id"].as_str(), Some(sid.as_str()));

        // Caps gate test: the row must still grant ai.chat so a real
        // `cos agent chat` invocation succeeds end-to-end.
        use crate::caps::Verb;
        let caps = sessions[0]["caps"].as_array().expect("caps array");
        let has_ai_chat = caps
            .iter()
            .any(|c| c["verb"].as_str() == Some(Verb::AI_CHAT.as_str()));
        assert!(has_ai_chat, "user-dir session should hold ai.chat");

        drop(guard);
        assert!(env::var("COS_SESSION").is_err());
        let raw_after = std::fs::read_to_string(&user_reg).unwrap();
        let v_after: serde_json::Value = serde_json::from_str(&raw_after).unwrap();
        assert_eq!(
            v_after["sessions"].as_array().map(|a| a.len()).unwrap_or(0),
            0,
            "user-dir registry should be empty after the guard drops"
        );

        drop(user_data);
        match prev_data {
            Some(v) => env::set_var("COS_DATA_DIR", v),
            None => env::remove_var("COS_DATA_DIR"),
        }
    }

    /// Sanity check that `caps::require` succeeds for a session
    /// living in the per-user registry. Guards against future
    /// regressions where `caps::enforcement` accidentally hard-codes
    /// `/var/lib/cos` for its registry read.
    #[test]
    fn enforcement_reads_per_user_registry() {
        let _lock = env_lock();
        let prev_data = env::var_os("COS_DATA_DIR");
        env::remove_var("COS_DATA_DIR");
        let _user_data = redirect_user_data_dir();
        env::remove_var("COS_SESSION");
        env::set_var("COS_PERMS_MODE", "strict");

        let guard = bootstrap_user_cli_session().expect("bootstrap should succeed");
        // require() must see the per-user row.
        use crate::caps::Verb;
        let result = crate::caps::require(Verb::AGENT_INVOKE, Scope::Wild);
        assert!(
            result.is_ok(),
            "expected per-user-registry session to be granted; got {result:?}"
        );

        drop(guard);
        env::remove_var("COS_PERMS_MODE");
        match prev_data {
            Some(v) => env::set_var("COS_DATA_DIR", v),
            None => env::remove_var("COS_DATA_DIR"),
        }
    }
}
