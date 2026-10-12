//! Owner-facing memory inspection and checked learned-memory edits.

use super::{
    app_memory, history::text_revision, notes::NotesStore, semantic::SemanticStore,
    sqlite_fts::MemoryDb,
};
use crate::agent::safety::redact::Redactor;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MAX_CONTENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Notes,
    Apps,
    Conversations,
    Semantic,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReference {
    Note { name: String },
    App { id: String },
    Message { id: String },
    Semantic { namespace: String, key: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryEntry {
    pub reference: MemoryReference,
    pub source: String,
    pub category: String,
    pub recorded_at_ms: Option<i64>,
    pub revision: String,
    pub content: String,
    pub editable: bool,
    pub deletable: bool,
    pub provenance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InspectRequest {
    pub kind: MemoryKind,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: u32,
}
fn default_limit() -> u32 {
    50
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub reference: MemoryReference,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    pub name: String,
    pub revision: String,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
    pub reference: MemoryReference,
    pub revision: String,
    pub confirm: bool,
}

fn checked(path: &Path, directory: bool) -> Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("inspect memory store: {error}")),
    };
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("memory store is not a regular owner-scoped file or directory".into());
    }
    Ok(true)
}

fn db(state: &Path, writable: bool) -> Result<Option<MemoryDb>, String> {
    let path = state.join("memory.db");
    if !checked(&path, false)? {
        return Ok(None);
    }
    (if writable {
        MemoryDb::open(path)
    } else {
        MemoryDb::open_read_only(path)
    })
    .map(Some)
    .map_err(|error| error.to_string())
}

fn entry(
    reference: MemoryReference,
    source: String,
    category: String,
    time: Option<i64>,
    text: String,
    editable: bool,
    deletable: bool,
    provenance: String,
) -> Result<MemoryEntry, String> {
    if text.len() > MAX_CONTENT_BYTES {
        return Err("memory entry exceeds the inspection byte limit".into());
    }
    let redactor = Redactor::default_set();
    let secret = redactor.contains_secrets(&text);
    Ok(MemoryEntry {
        reference,
        source,
        category,
        recorded_at_ms: time,
        revision: text_revision(&text),
        content: redactor.redact(&text),
        editable: editable && !secret,
        deletable,
        provenance,
    })
}

pub fn read_at(state: &Path, reference: MemoryReference) -> Result<MemoryEntry, String> {
    checked(state, true)?;
    match reference {
        MemoryReference::Note { name } => {
            super::notes::validate_name(&name)?;
            checked(&state.join("notes"), true)?;
            if !checked(&state.join("notes").join(&name), false)? {
                return Err("note is unavailable".into());
            }
            let metadata = std::fs::metadata(state.join("notes").join(&name))
                .map_err(|error| error.to_string())?;
            if metadata.len() > MAX_CONTENT_BYTES as u64 {
                return Err("note exceeds the inspection byte limit".into());
            }
            let text = NotesStore::at(state.join("notes"))
                .read(&name)?
                .ok_or("note is unavailable")?;
            let modified = metadata
                .modified()
                .map_err(|error| format!("read note modification time: {error}"))?;
            let time = Some(
                i64::try_from(
                    modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_millis(),
                )
                .map_err(|error| error.to_string())?,
            );
            entry(
                MemoryReference::Note { name: name.clone() },
                format!("notes/{name}"),
                "owner_note".into(),
                time,
                text,
                true,
                true,
                "Owner/Agent note; remembered data, not OS policy.".into(),
            )
        }
        MemoryReference::App { id } => {
            let id_number = id.parse::<i64>().map_err(|_| "invalid App memory id")?;
            if id_number <= 0 {
                return Err("invalid App memory id".into());
            }
            let database = db(state, false)?.ok_or("App memory store is unavailable")?;
            let row = app_memory::show(&database, id_number)
                .map_err(|error| error.to_string())?
                .ok_or("App memory row is unavailable")?;
            entry(MemoryReference::App { id }, format!("app/{}", row.source), row.kind.unwrap_or_else(|| "app_report".into()),
                Some(row.ts_ms), row.text, false, true, "App-reported memory; not a current OS observation. Corrections are separate owner notes.".into())
        }
        MemoryReference::Message { id } => {
            let id_number = id
                .parse::<i64>()
                .map_err(|_| "invalid conversation memory id")?;
            let database = db(state, false)?.ok_or("conversation memory is unavailable")?;
            let row = database
                .replayable_message(id_number)
                .map_err(|error| error.to_string())?
                .ok_or("conversation row is unavailable")?;
            if row.role != "user" && row.role != "assistant" {
                return Err("this row is not a visible conversation message".into());
            }
            let text = super::history::parse_stored_content(&row.role, &row.content).text;
            entry(
                MemoryReference::Message { id },
                format!("conversation/{}", row.session_id),
                row.role,
                Some(row.ts_ms),
                text,
                false,
                false,
                "Recorded interaction; canonical conversation and execution evidence is read-only."
                    .into(),
            )
        }
        MemoryReference::Semantic { namespace, key } => {
            if namespace.len() > 256 || key.len() > 256 {
                return Err("invalid semantic source reference".into());
            }
            let path = state.join("semantic.db");
            if !checked(&path, false)? {
                return Err("semantic memory store is unavailable".into());
            }
            let store = SemanticStore::open_read_only(path).map_err(|error| error.to_string())?;
            let row = store
                .get(&namespace, &key)
                .map_err(|error| error.to_string())?
                .ok_or("semantic memory row is unavailable")?;
            entry(MemoryReference::Semantic { namespace: namespace.clone(), key: key.clone() }, format!("{namespace}/{key}"), "derived_semantic".into(),
                Some(row.ts_ms), row.text, false, false, "Derived semantic index. Time is indexing time, not original event time; edit or forget the original source.".into())
        }
    }
}

pub fn inspect_at(state: &Path, request: InspectRequest) -> Result<serde_json::Value, String> {
    if request.limit == 0 || request.limit > 100 || request.query.chars().count() > 256 {
        return Err("invalid memory inspection limit or query".into());
    }
    checked(state, true)?;
    let mut references = Vec::new();
    match request.kind {
        MemoryKind::Notes => {
            checked(&state.join("notes"), true)?;
            for name in NotesStore::at(state.join("notes")).list()? {
                references.push(MemoryReference::Note { name });
            }
        }
        MemoryKind::Apps => {
            if let Some(database) = db(state, false)? {
                let rows = if request.query.trim().is_empty() {
                    app_memory::list(&database, None, request.limit as usize + 1)
                } else {
                    app_memory::search(&database, &request.query, None, request.limit as usize + 1)
                }
                .map_err(|error| error.to_string())?;
                references.extend(rows.into_iter().map(|row| MemoryReference::App {
                    id: row.id.to_string(),
                }));
            }
        }
        MemoryKind::Conversations => {
            if request.query.trim().is_empty() {
                return Err("enter a query to search conversation memory; the Sessions list provides chronological browsing".into());
            }
            if let Some(database) = db(state, false)? {
                let rows = database
                    .search_history(&request.query, None, request.limit as usize + 1)
                    .map_err(|error| error.to_string())?;
                references.extend(
                    rows.into_iter()
                        .filter(|hit| hit.row.role == "user" || hit.row.role == "assistant")
                        .map(|hit| MemoryReference::Message {
                            id: hit.row.id.to_string(),
                        }),
                );
            }
        }
        MemoryKind::Semantic => {
            let path = state.join("semantic.db");
            if checked(&path, false)? {
                let store =
                    SemanticStore::open_read_only(path).map_err(|error| error.to_string())?;
                references.extend(
                    store
                        .list(None, 100)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|row| MemoryReference::Semantic {
                            namespace: row.namespace,
                            key: row.key,
                        }),
                );
            }
        }
    }
    let mut entries = Vec::new();
    let mut has_more = false;
    let mut bytes = 0usize;
    for reference in references {
        let mut value = read_at(state, reference)?;
        if matches!(request.kind, MemoryKind::Notes | MemoryKind::Semantic)
            && !request.query.trim().is_empty()
            && !value
                .content
                .to_lowercase()
                .contains(&request.query.to_lowercase())
            && !value
                .source
                .to_lowercase()
                .contains(&request.query.to_lowercase())
        {
            continue;
        }
        if entries.len() == request.limit as usize {
            has_more = true;
            break;
        }
        value.content = value.content.chars().take(512).collect();
        bytes += serde_json::to_vec(&value)
            .map_err(|error| error.to_string())?
            .len();
        if bytes > 256 * 1024 {
            has_more = true;
            break;
        }
        entries.push(value);
    }
    Ok(
        serde_json::json!({"entries":entries,"has_more":has_more,"coverage":"Bounded owner-scoped view; semantic filtering scans at most 100 indexed rows."}),
    )
}

pub fn update_at(state: &Path, request: UpdateRequest) -> Result<MemoryEntry, String> {
    checked(state, true)?;
    checked(&state.join("notes"), true)?;
    super::notes::validate_name(&request.name)?;
    checked(&state.join("notes").join(&request.name), false)?;
    if request.content.len() > MAX_CONTENT_BYTES {
        return Err("note exceeds the edit byte limit".into());
    }
    if Redactor::default_set().contains_secrets(&request.content) {
        return Err("credentials belong in the credential store, not Agent memory".into());
    }
    NotesStore::at(state.join("notes")).write_checked(
        &request.name,
        &request.revision,
        &request.content,
    )?;
    read_at(state, MemoryReference::Note { name: request.name })
}

pub fn forget_at(state: &Path, request: ForgetRequest) -> Result<serde_json::Value, String> {
    if !request.confirm {
        return Err("forgetting memory requires explicit confirmation".into());
    }
    let current = read_at(state, request.reference.clone())?;
    if current.revision != request.revision {
        return Err("memory changed; refresh and review before forgetting".into());
    }
    match request.reference {
        MemoryReference::Note { name } => NotesStore::at(state.join("notes")).delete_checked(&name, &request.revision)?,
        MemoryReference::App { id } => {
            let database = db(state, true)?.ok_or("App memory store is unavailable")?;
            let semantic = if checked(&state.join("semantic.db"), false)? {
                Some(std::sync::Arc::new(SemanticStore::open(state.join("semantic.db"), None).map_err(|error| error.to_string())?))
            } else { None };
            if !app_memory::forget_row_checked(&database, semantic.as_ref(), id.parse::<i64>().map_err(|_| "invalid App memory id")?, &request.revision)
                .map_err(|error| error.to_string())? { return Err("App memory changed before deletion".into()); }
        }
        _ => return Err("conversation and derived-index evidence is read-only; change the original learned source".into()),
    }
    Ok(serde_json::json!({"forgotten":true,"conversations_preserved":true}))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/memory/inspection.rs"
    ));
}
