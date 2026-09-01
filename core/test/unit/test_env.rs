// Process-wide test utilities. cfg(test)-only.
//
// Several modules' tests mutate global env vars (`COS_DATA_DIR`,
// `COS_SESSION`, etc.). cargo runs all tests in the same binary on
// a thread pool, so each test module owning its *own* `Mutex<()>`
// is not enough — two modules can race. Anything that touches
// env vars in tests must take this single shared lock.

use std::sync::{Mutex, MutexGuard};
use std::{ffi::OsString, path::Path};

use crate::caps::{Cap, Role, Scope, Verb};
use crate::proc::SessionInfo;

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn lock_env() -> MutexGuard<'static, ()> {
    // Recover from a poisoned mutex so a single panicked test doesn't
    // cascade into N "PoisonError" failures that obscure the real cause.
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII guard that sets `COS_PERMS_MODE=permissive` while held and
/// restores the previous value (including "unset") on drop. Use this
/// in tool/runtime tests that do not bootstrap a real session but
/// still call into capability-gated code paths (`ai.chat`,
/// `sys.kernel`, …). The cap layer treats permissive mode as
/// "allow-all + audit"; that is exactly what these tests want.
pub(crate) struct PermissiveModeGuard {
    prev: Option<std::ffi::OsString>,
}

impl PermissiveModeGuard {
    pub(crate) fn new() -> Self {
        let prev = std::env::var_os("COS_PERMS_MODE");
        std::env::set_var("COS_PERMS_MODE", "permissive");
        Self { prev }
    }
}

impl Drop for PermissiveModeGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => std::env::set_var("COS_PERMS_MODE", v),
            None => std::env::remove_var("COS_PERMS_MODE"),
        }
    }
}

pub(crate) struct TestSessionGuard {
    session_id: String,
    previous_session: Option<OsString>,
    previous_proc_dir: Option<OsString>,
}

pub(crate) struct TestEnvVarGuard {
    name: &'static str,
    previous: Option<OsString>,
}

impl TestEnvVarGuard {
    pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    pub(crate) fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

impl Drop for TestEnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

impl TestSessionGuard {
    pub(crate) fn admin(proc_dir: &Path) -> Self {
        let previous_session = std::env::var_os("COS_SESSION");
        let previous_proc_dir = std::env::var_os("COS_PROC_DATA_DIR");
        std::env::set_var("COS_PROC_DATA_DIR", proc_dir);

        let session_id = format!("test-parent-{}", uuid::Uuid::new_v4().simple());
        let role = Role::Admin;
        let mut caps =
            role.caps_with_scopes(Some(Scope::Wild), Some(Scope::Wild), Some(Scope::Wild));
        caps.insert(Cap::new(Verb::SYS_KERNEL, Scope::Wild));
        crate::proc::register_session(SessionInfo {
            session_id: session_id.clone(),
            pid: std::process::id(),
            command: vec!["cargo test".to_string()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: Some("test".to_string()),
            parent: None,
            workdir: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            exit_code: None,
            ended_at: None,
            tier: Some(role.credential_tier()),
            scope: Some("test".to_string()),
            priority: None,
            caps: Some(caps),
            transient_caps: None,
            role: Some(role.name().to_string()),
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            client: crate::session::SessionClient::default(),
        })
        .expect("register test parent session");
        std::env::set_var("COS_SESSION", &session_id);

        Self {
            session_id,
            previous_session,
            previous_proc_dir,
        }
    }
}

impl Drop for TestSessionGuard {
    fn drop(&mut self) {
        crate::proc::deregister_session(&self.session_id);
        match self.previous_session.take() {
            Some(value) => std::env::set_var("COS_SESSION", value),
            None => std::env::remove_var("COS_SESSION"),
        }
        match self.previous_proc_dir.take() {
            Some(value) => std::env::set_var("COS_PROC_DATA_DIR", value),
            None => std::env::remove_var("COS_PROC_DATA_DIR"),
        }
    }
}
