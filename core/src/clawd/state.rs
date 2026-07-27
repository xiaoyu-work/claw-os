use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::session::{LeaseGuard, SessionId};

#[derive(Clone)]
pub struct DaemonState {
    inner: Arc<DaemonStateInner>,
}

struct DaemonStateInner {
    started_at: DateTime<Utc>,
    started_instant: Instant,
    context: Mutex<BTreeMap<String, ContextEntry>>,
    transactions: Mutex<BTreeMap<String, TransactionHandle>>,
}

#[derive(Debug, Clone)]
pub struct ContextEntry {
    pub source: String,
    pub updated_at: DateTime<Utc>,
    pub payload: Value,
    pub metadata: Value,
}

pub struct TransactionHandle {
    pub session_id: SessionId,
    pub purpose: String,
    pub started_at: DateTime<Utc>,
    pub owner_uid: u32,
    pub lease: LeaseGuard,
}

#[derive(Debug, Clone)]
pub struct TransactionSummary {
    pub id: String,
    pub purpose: String,
    pub started_at: DateTime<Utc>,
    pub owner_uid: u32,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DaemonStateInner {
                started_at: Utc::now(),
                started_instant: Instant::now(),
                context: Mutex::new(BTreeMap::new()),
                transactions: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.inner.started_at
    }

    pub fn uptime_millis(&self) -> u128 {
        self.inner.started_instant.elapsed().as_millis()
    }

    pub fn update_context(&self, source: String, payload: Value, metadata: Value) {
        let entry = ContextEntry {
            source: source.clone(),
            updated_at: Utc::now(),
            payload,
            metadata,
        };
        self.inner
            .context
            .lock()
            .expect("clawd context lock poisoned")
            .insert(source, entry);
    }

    pub fn context_snapshot(&self) -> Vec<ContextEntry> {
        self.inner
            .context
            .lock()
            .expect("clawd context lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn insert_transaction(&self, handle: TransactionHandle) -> Result<(), String> {
        let id = handle.session_id.as_str().to_string();
        let mut transactions = self
            .inner
            .transactions
            .lock()
            .expect("clawd transaction lock poisoned");
        if transactions.contains_key(&id) {
            return Err(format!("transaction already active: {id}"));
        }
        transactions.insert(id, handle);
        Ok(())
    }

    pub fn require_transaction_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> Result<(), String> {
        let transactions = self
            .inner
            .transactions
            .lock()
            .expect("clawd transaction lock poisoned");
        let handle = transactions
            .get(id)
            .ok_or_else(|| format!("transaction is not active: {id}"))?;
        if let Some(uid) = owner_uid {
            if handle.owner_uid != uid {
                return Err(format!("transaction is not owned by uid {uid}"));
            }
        }
        Ok(())
    }

    pub fn take_transaction_for_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> Result<Option<TransactionHandle>, String> {
        let mut transactions = self
            .inner
            .transactions
            .lock()
            .expect("clawd transaction lock poisoned");
        let Some(handle) = transactions.get(id) else {
            return Ok(None);
        };
        if let Some(uid) = owner_uid {
            if handle.owner_uid != uid {
                return Err(format!("transaction is not owned by uid {uid}"));
            }
        }
        Ok(transactions.remove(id))
    }

    pub fn list_transactions_for_owner(
        &self,
        owner_uid: Option<u32>,
    ) -> Vec<TransactionSummary> {
        self.inner
            .transactions
            .lock()
            .expect("clawd transaction lock poisoned")
            .values()
            .filter(|handle| match owner_uid {
                None => true,
                Some(uid) => handle.owner_uid == uid,
            })
            .map(|handle| TransactionSummary {
                id: handle.session_id.as_str().to_string(),
                purpose: handle.purpose.clone(),
                started_at: handle.started_at,
                owner_uid: handle.owner_uid,
            })
            .collect()
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
