//! @agent-file
//! Responsibility: read conversation memory under the authenticated owner's filesystem identity.
//! Key dependencies: the shared owner-memory provider and read-only MemoryDb access.
//! Constraints: root owners are refused and missing databases are never created by read routes.

use crate::agent::memory::conversations::{
    ConversationHistoryPage, ConversationMetadata, ConversationSnapshot,
};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::clawd::memory::{open_owner_db, run_as_owner};

#[derive(Debug)]
pub(super) struct OwnerMemoryView {
    pub(super) metadata: ConversationMetadata,
    pub(super) history: ConversationHistoryPage,
}

#[derive(Debug)]
pub(super) struct OwnerMemorySummary {
    pub(super) metadata: ConversationMetadata,
}

pub(super) fn read_view(
    owner_uid: u32,
    session_id: String,
    limit: usize,
) -> Result<OwnerMemoryView, String> {
    run_as_owner(owner_uid, move |path| {
        let Some(db) = open_owner_db(&path, owner_uid)? else {
            return Ok(OwnerMemoryView {
                metadata: ConversationMetadata::default(),
                history: ConversationHistoryPage::default(),
            });
        };
        Ok(OwnerMemoryView {
            metadata: db
                .conversation_metadata(&session_id)
                .map_err(|err| err.to_string())?,
            history: db
                .conversation_history_page(&session_id, limit)
                .map_err(|err| err.to_string())?,
        })
    })
}

pub(super) fn read_summaries(
    owner_uid: u32,
    session_ids: Vec<String>,
) -> Result<Vec<OwnerMemorySummary>, String> {
    run_as_owner(owner_uid, move |path| {
        let Some(db) = open_owner_db(&path, owner_uid)? else {
            return Ok(session_ids
                .into_iter()
                .map(|_| OwnerMemorySummary {
                    metadata: ConversationMetadata::default(),
                })
                .collect());
        };
        session_ids
            .into_iter()
            .map(|session_id| {
                db.conversation_metadata(&session_id)
                    .map(|metadata| OwnerMemorySummary { metadata })
                    .map_err(|err| err.to_string())
            })
            .collect()
    })
}

pub(super) fn read_snapshot(
    owner_uid: u32,
    session_id: String,
    before_user_turn: Option<u32>,
) -> Result<(ConversationMetadata, ConversationSnapshot), String> {
    run_as_owner(owner_uid, move |path| {
        let Some(db) = open_owner_db(&path, owner_uid)? else {
            if let Some(requested) = before_user_turn.filter(|requested| *requested > 0) {
                return Err(format!(
                    "requested {requested} user turns, but the conversation has 0"
                ));
            }
            return Ok((
                ConversationMetadata::default(),
                ConversationSnapshot::default(),
            ));
        };
        Ok((
            db.conversation_metadata(&session_id)
                .map_err(|err| err.to_string())?,
            db.conversation_snapshot(&session_id, before_user_turn)
                .map_err(|err| err.to_string())?,
        ))
    })
}

pub(super) fn install_snapshot(
    owner_uid: u32,
    session_id: String,
    snapshot: ConversationSnapshot,
    limit: usize,
) -> Result<OwnerMemoryView, String> {
    run_as_owner(owner_uid, move |path| {
        let db = MemoryDb::open(&path).map_err(|err| err.to_string())?;
        db.install_conversation_snapshot(&session_id, &snapshot)
            .map_err(|err| err.to_string())?;
        Ok(OwnerMemoryView {
            metadata: db
                .conversation_metadata(&session_id)
                .map_err(|err| err.to_string())?,
            history: db
                .conversation_history_page(&session_id, limit)
                .map_err(|err| err.to_string())?,
        })
    })
}

pub(super) fn read_revert_plan(
    owner_uid: u32,
    session_id: String,
    user_turns: u32,
) -> Result<(ConversationSnapshot, ConversationSnapshot), String> {
    run_as_owner(owner_uid, move |path| {
        let db = open_owner_db(&path, owner_uid)?
            .ok_or_else(|| "the active conversation has no user turns".to_string())?;
        let current = db
            .conversation_snapshot(&session_id, None)
            .map_err(|err| err.to_string())?;
        let retained = db
            .conversation_revert_snapshot(&session_id, user_turns)
            .map_err(|err| err.to_string())?;
        Ok((current, retained))
    })
}

pub(super) fn apply_revert(
    owner_uid: u32,
    session_id: String,
    user_turns: u32,
    reverted_at_ms: i64,
    expected_revision: String,
    limit: usize,
) -> Result<OwnerMemoryView, String> {
    run_as_owner(owner_uid, move |path| {
        let db = MemoryDb::open(&path).map_err(|err| err.to_string())?;
        db.revert_conversation_checked(&session_id, user_turns, reverted_at_ms, &expected_revision)
            .map_err(|err| err.to_string())?;
        Ok(OwnerMemoryView {
            metadata: db
                .conversation_metadata(&session_id)
                .map_err(|err| err.to_string())?,
            history: db
                .conversation_history_page(&session_id, limit)
                .map_err(|err| err.to_string())?,
        })
    })
}
