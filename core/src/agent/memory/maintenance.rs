//! @agent-file
//! Responsibility: reset owner-learned Agent memory without deleting conversation evidence.
//! Key dependencies: owner-partitioned MemoryDb, notes, and derived semantic storage.
//! Constraints: reject symlinked stores and preserve conversations, Jobs, bindings, summaries, and audit.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::app_memory;
use super::notes::NotesStore;
use super::semantic::SemanticStore;
use super::sqlite_fts::MemoryDb;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct LearnedMemoryReset {
    pub notes_deleted: usize,
    pub app_memories_deleted: usize,
    pub semantic_rows_deleted: usize,
    pub conversations_preserved: bool,
}

pub fn reset_at(agent_state_dir: &Path) -> Result<LearnedMemoryReset, String> {
    if !checked_kind(
        agent_state_dir,
        EntryKind::Directory,
        "Agent state directory",
    )? {
        return Ok(LearnedMemoryReset {
            notes_deleted: 0,
            app_memories_deleted: 0,
            semantic_rows_deleted: 0,
            conversations_preserved: true,
        });
    }

    let memory_path = agent_state_dir.join("memory.db");
    let semantic_path = agent_state_dir.join("semantic.db");
    let notes_path = agent_state_dir.join("notes");
    let memory_present = checked_kind(&memory_path, EntryKind::File, "memory database")?;
    let semantic_present = checked_kind(&semantic_path, EntryKind::File, "semantic database")?;
    let notes_present = checked_kind(&notes_path, EntryKind::Directory, "notes directory")?;
    let notes = NotesStore::at(&notes_path);
    let note_names = if notes_present {
        notes.list()?
    } else {
        Vec::new()
    };

    let app_memories_deleted = if memory_present {
        let db = MemoryDb::open(&memory_path).map_err(|error| error.to_string())?;
        app_memory::forget_all(&db).map_err(|error| error.to_string())?
    } else {
        0
    };
    let semantic_rows_deleted = if semantic_present {
        SemanticStore::open(&semantic_path, None)
            .map_err(|error| error.to_string())?
            .clear_all()
            .map_err(|error| error.to_string())?
    } else {
        0
    };
    let mut notes_deleted = 0;
    for name in note_names {
        notes.delete(&name)?;
        notes_deleted += 1;
    }

    Ok(LearnedMemoryReset {
        notes_deleted,
        app_memories_deleted,
        semantic_rows_deleted,
        conversations_preserved: true,
    })
}

#[derive(Clone, Copy)]
enum EntryKind {
    File,
    Directory,
}

fn checked_kind(path: &Path, kind: EntryKind, label: &str) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("inspect {label} {}: {error}", path.display())),
    };
    let file_type = metadata.file_type();
    let expected = match kind {
        EntryKind::File => file_type.is_file(),
        EntryKind::Directory => file_type.is_dir(),
    };
    if file_type.is_symlink() || !expected {
        return Err(format!(
            "{label} is not a regular {}: {}",
            match kind {
                EntryKind::File => "file",
                EntryKind::Directory => "directory",
            },
            path.display()
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/maintenance.rs"
    ));
}
