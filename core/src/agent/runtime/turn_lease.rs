//! Per-session exclusion for conversation-mutating turns.
//!
//! A lease registry rejects a second active turn for the same session while
//! allowing unrelated sessions to run concurrently. The registry lock is held
//! only for insertion or removal; the returned RAII guard owns the lease across
//! async work and releases it on success, error, panic, or task cancellation.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Default)]
pub struct TurnLeaseRegistry {
    inner: Arc<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    active: Mutex<HashSet<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnAlreadyActive;

pub struct TurnLease {
    inner: Arc<RegistryInner>,
    session_id: String,
}

impl TurnLeaseRegistry {
    pub fn try_acquire(
        &self,
        session_id: impl Into<String>,
    ) -> Result<TurnLease, TurnAlreadyActive> {
        let session_id = session_id.into();
        {
            let mut active = lock_active(&self.inner);
            if !active.insert(session_id.clone()) {
                return Err(TurnAlreadyActive);
            }
        }
        Ok(TurnLease {
            inner: self.inner.clone(),
            session_id,
        })
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        lock_active(&self.inner).len()
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        lock_active(&self.inner).remove(&self.session_id);
    }
}

fn lock_active(inner: &RegistryInner) -> MutexGuard<'_, HashSet<String>> {
    inner
        .active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/runtime/turn_lease.rs"
    ));
}
