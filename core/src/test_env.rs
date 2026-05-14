//! Process-wide test utilities. cfg(test)-only.
//!
//! Several modules' tests mutate global env vars (`COS_DATA_DIR`,
//! `COS_SESSION`, etc.). cargo runs all tests in the same binary on
//! a thread pool, so each test module owning its *own* `Mutex<()>`
//! is not enough — two modules can race. Anything that touches
//! env vars in tests must take this single shared lock.

#![cfg(test)]

use std::sync::{Mutex, MutexGuard};

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
