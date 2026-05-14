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
