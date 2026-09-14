//! @agent-file
//! Responsibility: read conversation memory under the authenticated owner's filesystem identity.
//! Key dependencies: FsIdentityGuard, owner-partitioned paths, and read-only MemoryDb access.
//! Constraints: root owners are refused and missing databases are never created by read routes.

use std::path::PathBuf;
use std::sync::mpsc;

use crate::agent::memory::conversations::{ConversationHistoryPage, ConversationMetadata};
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
