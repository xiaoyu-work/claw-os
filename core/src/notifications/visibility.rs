use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use super::{now_ms, validate_identifier, NotificationError};

const VIEW_LEASE_MS: i64 = 15_000;
const VIEW_RETENTION_MS: i64 = 60_000;
const MAX_OWNER_VIEWS: usize = 64;
const MAX_VIEWS: usize = 1024;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ViewReceipt {
    pub id: String,
    pub revision: u64,
    pub session_id: Option<String>,
    pub expires_at_ms: i64,
}

struct Entry {
    receipt: ViewReceipt,
    retained_until_ms: i64,
}

#[derive(Default)]
pub(super) struct ConversationViews {
    entries: HashMap<(u32, String), Entry>,
}

impl ConversationViews {
    pub(super) fn update(
        &mut self,
        owner_uid: u32,
        id: &str,
        revision: u64,
        session_id: Option<&str>,
        now: i64,
    ) -> Result<ViewReceipt, NotificationError> {
        validate_identifier("notification viewer", id, 128)?;
        if revision == 0 {
            return Err(NotificationError::Invalid(
                "notification viewer revision must be positive".into(),
            ));
        }
        self.entries
            .retain(|_, entry| entry.retained_until_ms > now);
        let key = (owner_uid, id.to_string());
        if let Some(entry) = self.entries.get(&key) {
            if revision < entry.receipt.revision {
                return Ok(entry.receipt.clone());
            }
            if revision == entry.receipt.revision
                && entry.receipt.session_id.as_deref() != session_id
            {
                return Err(NotificationError::Invalid(
                    "notification viewer revision was reused for a different view".into(),
                ));
            }
        } else if self.entries.len() >= MAX_VIEWS
            || self
                .entries
                .keys()
                .filter(|(owner, _)| *owner == owner_uid)
                .count()
                >= MAX_OWNER_VIEWS
        {
            return Err(NotificationError::Invalid(
                "too many notification viewers".into(),
            ));
        }
        let receipt = ViewReceipt {
            id: id.into(),
            revision,
            session_id: session_id.map(str::to_string),
            expires_at_ms: now + VIEW_LEASE_MS,
        };
        self.entries.insert(
            key,
            Entry {
                receipt: receipt.clone(),
                retained_until_ms: now + VIEW_RETENTION_MS,
            },
        );
        Ok(receipt)
    }

    pub(super) fn contains(&mut self, owner_uid: u32, session_id: &str, now: i64) -> bool {
        self.entries
            .retain(|_, entry| entry.retained_until_ms > now);
        self.entries.iter().any(|((owner, _), entry)| {
            *owner == owner_uid
                && entry.receipt.expires_at_ms > now
                && entry.receipt.session_id.as_deref() == Some(session_id)
        })
    }
}

fn views() -> &'static Mutex<ConversationViews> {
    static VIEWS: OnceLock<Mutex<ConversationViews>> = OnceLock::new();
    VIEWS.get_or_init(|| Mutex::new(ConversationViews::default()))
}

// Viewing is only a notification hint, never attended presence or consent.
pub(crate) fn set_conversation_view(
    owner_uid: u32,
    id: &str,
    revision: u64,
    session_id: Option<&str>,
) -> Result<ViewReceipt, NotificationError> {
    views()
        .lock()
        .map_err(|_| NotificationError::Poisoned)?
        .update(owner_uid, id, revision, session_id, now_ms())
}

pub(crate) fn conversation_is_viewed(
    owner_uid: u32,
    session_id: &str,
) -> Result<bool, NotificationError> {
    Ok(views()
        .lock()
        .map_err(|_| NotificationError::Poisoned)?
        .contains(owner_uid, session_id, now_ms()))
}
