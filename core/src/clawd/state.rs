use std::collections::BTreeMap;
use std::fs;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::session::{self, LeaseGuard, SessionId, Status as SessionStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateErrorKind {
    Unavailable,
    Corrupt,
    Conflict,
    NotFound,
    NotAuthorized,
}

pub struct StateError {
    kind: StateErrorKind,
    operation: &'static str,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl StateError {
    pub fn kind(&self) -> StateErrorKind {
        self.kind
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    fn poisoned(resource: &'static str) -> Self {
        Self::message(
            StateErrorKind::Corrupt,
            "state.lock",
            format!("clawd {resource} state is unavailable because its lock was poisoned"),
        )
    }

    fn corrupt(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(StateErrorKind::Corrupt, operation, message)
    }

    fn conflict(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(StateErrorKind::Conflict, operation, message)
    }

    fn not_found(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(StateErrorKind::NotFound, operation, message)
    }

    fn unauthorized(operation: &'static str, message: impl Into<String>) -> Self {
        Self::message(StateErrorKind::NotAuthorized, operation, message)
    }

    fn io(operation: &'static str, context: impl Into<String>, source: std::io::Error) -> Self {
        let context = context.into();
        Self {
            kind: StateErrorKind::Unavailable,
            operation,
            message: format!("{context}: {source}"),
            source: Some(Box::new(source)),
        }
    }

    fn with_source<E>(operation: &'static str, context: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let context = context.into();
        Self {
            kind: StateErrorKind::Unavailable,
            operation,
            message: format!("{context}: {source}"),
            source: Some(Box::new(source)),
        }
    }

    fn message(kind: StateErrorKind, operation: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            operation,
            message: message.into(),
            source: None,
        }
    }
}

impl std::fmt::Debug for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateError")
            .field("kind", &self.kind)
            .field("operation", &self.operation)
            .field("message", &self.message)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

pub type StateResult<T> = Result<T, StateError>;

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
    pub fn new() -> StateResult<Self> {
        Self::try_new()
    }

    pub fn try_new() -> StateResult<Self> {
        let transactions = recover_transactions()?;
        Ok(Self {
            inner: Arc::new(DaemonStateInner {
                started_at: Utc::now(),
                started_instant: Instant::now(),
                context: Mutex::new(BTreeMap::new()),
                transactions: Mutex::new(transactions),
            }),
        })
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.inner.started_at
    }

    pub fn uptime_millis(&self) -> u128 {
        self.inner.started_instant.elapsed().as_millis()
    }

    pub fn update_context(
        &self,
        source: String,
        payload: Value,
        metadata: Value,
    ) -> StateResult<()> {
        let entry = ContextEntry {
            source: source.clone(),
            updated_at: Utc::now(),
            payload,
            metadata,
        };
        self.inner
            .context
            .lock()
            .map_err(|_| StateError::poisoned("context"))?
            .insert(source, entry);
        Ok(())
    }

    pub fn context_snapshot(&self) -> StateResult<Vec<ContextEntry>> {
        Ok(self
            .inner
            .context
            .lock()
            .map_err(|_| StateError::poisoned("context"))?
            .values()
            .cloned()
            .collect())
    }

    pub fn insert_transaction(&self, handle: TransactionHandle) -> StateResult<()> {
        let id = handle.session_id.as_str().to_string();
        let mut transactions = self
            .inner
            .transactions
            .lock()
            .map_err(|_| StateError::poisoned("transaction"))?;
        if transactions.contains_key(&id) {
            return Err(StateError::conflict(
                "transaction.insert",
                format!("transaction already active: {id}"),
            ));
        }
        transactions.insert(id, handle);
        Ok(())
    }

    pub fn require_transaction_owner(&self, id: &str, owner_uid: Option<u32>) -> StateResult<()> {
        let transactions = self
            .inner
            .transactions
            .lock()
            .map_err(|_| StateError::poisoned("transaction"))?;
        let handle = transactions.get(id).ok_or_else(|| {
            StateError::not_found(
                "transaction.require_owner",
                format!("transaction is not active: {id}"),
            )
        })?;
        if let Some(uid) = owner_uid {
            if handle.owner_uid != uid {
                return Err(StateError::unauthorized(
                    "transaction.require_owner",
                    format!("transaction is not owned by uid {uid}"),
                ));
            }
        }
        Ok(())
    }

    pub fn take_transaction_for_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> StateResult<Option<TransactionHandle>> {
        let mut transactions = self
            .inner
            .transactions
            .lock()
            .map_err(|_| StateError::poisoned("transaction"))?;
        let Some(handle) = transactions.get(id) else {
            return Ok(None);
        };
        if let Some(uid) = owner_uid {
            if handle.owner_uid != uid {
                return Err(StateError::unauthorized(
                    "transaction.take",
                    format!("transaction is not owned by uid {uid}"),
                ));
            }
        }
        Ok(transactions.remove(id))
    }

    pub fn list_transactions_for_owner(
        &self,
        owner_uid: Option<u32>,
    ) -> StateResult<Vec<TransactionSummary>> {
        Ok(self
            .inner
            .transactions
            .lock()
            .map_err(|_| StateError::poisoned("transaction"))?
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
            .collect())
    }

    #[cfg(test)]
    pub(super) fn poison_context_for_test(&self) {
        let state = self.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = state.inner.context.lock().unwrap();
            panic!("poison context lock");
        });
    }
}

fn recover_transactions() -> StateResult<BTreeMap<String, TransactionHandle>> {
    let mut recovered = BTreeMap::new();
    let sessions = strict_session_list()?;
    for meta in sessions {
        if meta.status == SessionStatus::Running
            && meta.creator_runtime.as_deref() == Some("clawd-transaction-pending")
        {
            let lease = session::try_acquire(&meta.id).map_err(|source| {
                StateError::with_source(
                    "transaction.recover",
                    format!(
                        "clawd transaction recovery: acquire incomplete session {}",
                        meta.id
                    ),
                    source,
                )
            })?;
            drop(lease);
            session::end(&meta.id, SessionStatus::Failed).map_err(|source| {
                StateError::with_source(
                    "transaction.recover",
                    format!(
                        "clawd transaction recovery: fail incomplete session {}",
                        meta.id
                    ),
                    source,
                )
            })?;
            continue;
        }
        if meta.status != SessionStatus::Running
            || meta.creator_runtime.as_deref() != Some("clawd-transaction")
        {
            continue;
        }
        let Some(owner_uid) = meta.owner_uid else {
            return Err(StateError::corrupt(
                "transaction.recover",
                format!(
                    "clawd transaction recovery: session {} has no owner uid",
                    meta.id
                ),
            ));
        };
        let lease = session::try_acquire(&meta.id).map_err(|source| {
            StateError::with_source(
                "transaction.recover",
                format!("clawd transaction recovery: acquire session {}", meta.id),
                source,
            )
        })?;
        let started_at = DateTime::parse_from_rfc3339(&meta.created_at)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|source| {
                StateError::with_source(
                    "transaction.recover",
                    format!(
                        "clawd transaction recovery: session {} has invalid created_at",
                        meta.id
                    ),
                    source,
                )
            })?;
        let id = meta.id.as_str().to_string();
        recovered.insert(
            id,
            TransactionHandle {
                session_id: meta.id,
                purpose: meta.purpose,
                started_at,
                owner_uid,
                lease,
            },
        );
    }
    Ok(recovered)
}

fn strict_session_list() -> StateResult<Vec<session::SessionMeta>> {
    let root = session::sessions_root();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(StateError::io(
                "transaction.recover",
                format!("clawd transaction recovery: read {}", root.display()),
                source,
            ))
        }
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            StateError::io(
                "transaction.recover",
                format!("clawd transaction recovery: enumerate {}", root.display()),
                source,
            )
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(id) = SessionId::from_str(&name) else {
            continue;
        };
        let file_type = entry.file_type().map_err(|source| {
            StateError::io(
                "transaction.recover",
                format!(
                    "clawd transaction recovery: inspect {}",
                    entry.path().display()
                ),
                source,
            )
        })?;
        if !file_type.is_dir() {
            return Err(StateError::corrupt(
                "transaction.recover",
                format!(
                    "clawd transaction recovery: canonical session path is not a directory: {}",
                    entry.path().display()
                ),
            ));
        }
        sessions.push(session::get_meta(&id).map_err(|source| {
            StateError::with_source(
                "transaction.recover",
                format!("clawd transaction recovery: read session {id} metadata"),
                source,
            )
        })?);
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/state.rs"
    ));
}
