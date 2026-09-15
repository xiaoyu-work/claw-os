//! @agent-file
//! Responsibility: read conversation memory under the authenticated owner's filesystem identity.
//! Key dependencies: FsIdentityGuard, owner-partitioned paths, and read-only MemoryDb access.
//! Constraints: root owners are refused and missing databases are never created by read routes.

use std::path::PathBuf;
use std::sync::mpsc;

use crate::agent::memory::conversations::{
    ConversationHistoryPage, ConversationMetadata, ConversationSnapshot,
};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agentd::spawn::ROOT_OWNER_REFUSAL;
use crate::clawd::client_identity::FsIdentityGuard;

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
        let Some(db) = open_read_only_if_present(path)? else {
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
        let Some(db) = open_read_only_if_present(path)? else {
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
        let Some(db) = open_read_only_if_present(path)? else {
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
        let db = open_read_only_if_present(path)?
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
        db.revert_conversation_checked(
            &session_id,
            user_turns,
            reverted_at_ms,
            &expected_revision,
        )
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

fn open_read_only_if_present(path: PathBuf) -> Result<Option<MemoryDb>, String> {
    if !path.exists() {
        return Ok(None);
    }
    MemoryDb::open_read_only(&path)
        .map(Some)
        .map_err(|err| err.to_string())
}

fn run_as_owner<T, F>(owner_uid: u32, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(PathBuf) -> Result<T, String> + Send + 'static,
{
    if owner_uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    let db_path = crate::paths::clawd_user_memory_db_path(owner_uid);
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("clawd-conversation-owner-memory".to_string())
        .spawn(move || {
            let result = FsIdentityGuard::enter(owner_uid).and_then(|_identity| operation(db_path));
            let _ = tx.send(result);
        })
        .map_err(|err| format!("start owner memory reader: {err}"))?;
    rx.recv()
        .map_err(|_| "owner memory reader stopped without a result".to_string())?
}
