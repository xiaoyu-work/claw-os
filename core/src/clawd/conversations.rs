//! @agent-file
//! Responsibility: expose owner-scoped Agent conversation CRUD and frontend identity projection.
//! Key dependencies: root-owned SessionMeta/state, the task session baseline, and owner memory reads.
//! Constraints: presentation UUIDs never authorize work; mutation accepts only canonical session IDs.

mod bindings;
mod dto;
mod jobs;
mod owner_memory;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent::service::Store;
use crate::agentd::spawn::ROOT_OWNER_REFUSAL;
use crate::caps::Role;
use crate::session::{self, SessionId, SessionOrigin};

use self::dto::{
    Conversation, ConversationId, ConversationLookup, ConversationMetadata, ConversationResponse,
    CreateRequest, ForkRequest, GetRequest, ListRequest, ListResponse, PresentationId,
    UpdateRequest,
};
use self::owner_memory::OwnerMemoryView;
use super::client_identity::ClientIdentity;

const PRESENTATION_NAMESPACE: &str = "conversation";
const DEFAULT_TITLE: &str = "New conversation";
const DEFAULT_HISTORY_LIMIT: usize = 1_000;
const MAX_HISTORY_LIMIT: usize = 10_000;
const DEFAULT_LIST_LIMIT: usize = 100;
const MAX_LIST_LIMIT: usize = 1_000;
const MAX_TITLE_CHARS: usize = 128;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Presentation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    presentation_id: Option<PresentationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inherited_tasks: Vec<bindings::TaskMembership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

pub(super) fn create(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: CreateRequest = serde_json::from_value(params)
        .map_err(|err| format!("invalid conversation create: {err}"))?;
    let owner_uid = owner_uid(client)?;
    let title = request.title.map(validate_title).transpose()?;
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let (_, value) = super::tasks::create_agent_session_with(
        "Agent conversation".to_string(),
        owner_uid,
        &owner_home,
        |sid| {
            let meta = session::get_meta(sid).map_err(|err| err.to_string())?;
            let presentation = Presentation {
                presentation_id: Some(derive_presentation_id(sid)),
                title: title.clone(),
                ..Presentation::default()
            };
            let value = conversation_response(
                &meta,
                presentation.clone(),
                OwnerMemoryView {
                    metadata: Default::default(),
                    history: Default::default(),
                },
                owner_uid,
            )?;
            write_presentation(sid, &presentation)?;
            Ok(value)
        },
    )?;
    Ok(value)
}

pub(super) fn get(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: GetRequest =
        serde_json::from_value(params).map_err(|err| format!("invalid conversation get: {err}"))?;
    let owner_uid = owner_uid(client)?;
    let lookup = ConversationLookup::parse(&request.id)?;
    let id = lookup_session(&lookup, owner_uid)?;
    let limit = bounded_limit(
        request.limit,
        DEFAULT_HISTORY_LIMIT,
        MAX_HISTORY_LIMIT,
        "history",
    )?;
    get_for_id(&id, owner_uid, limit)
}

pub(super) fn list(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: ListRequest = serde_json::from_value(params)
        .map_err(|err| format!("invalid conversation list: {err}"))?;
    let owner_uid = owner_uid(client)?;
    let archived = request.archived.unwrap_or(false);
    let limit = bounded_limit(request.limit, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, "list")?;

    let mut sessions = session::list().map_err(|err| err.to_string())?;
    sessions.retain(|meta| is_conversation(meta, owner_uid));
    let session_ids = sessions
        .iter()
        .map(|meta| meta.id.to_string())
        .collect::<Vec<_>>();
    let summaries = owner_memory::read_summaries(owner_uid, session_ids)?;
    if summaries.len() != sessions.len() {
        return Err("owner memory summary count changed".to_string());
    }

    let mut conversations = sessions
        .into_iter()
        .zip(summaries)
        .map(|(meta, summary)| {
            let presentation = read_presentation(&meta.id)?;
            presentation_metadata(&meta, presentation, summary.metadata)
        })
        .collect::<Result<Vec<_>, String>>()?;
    conversations.retain(|value| !value.deleted && value.archived == archived);
    conversations.sort_by(|left, right| {
        timestamp(&right.updated_at)
            .cmp(&timestamp(&left.updated_at))
            .then_with(|| right.id.cmp(&left.id))
    });
    let total = conversations.len();
    let conversation_count =
        u64::try_from(total).map_err(|_| "conversation count is invalid".to_string())?;
    conversations.truncate(limit);
    response(ListResponse {
        conversations,
        conversation_count,
        conversations_truncated: total > limit,
    })
}

pub(super) fn update(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: UpdateRequest = serde_json::from_value(params)
        .map_err(|err| format!("invalid conversation update: {err}"))?;
    if request.title.is_none() && request.archived.is_none() && request.deleted.is_none() {
        return Err("conversation update requires a field to change".to_string());
    }
    let title = request.title.map(validate_title).transpose()?;
    let owner_uid = owner_uid(client)?;
    let id = ConversationId::parse(&request.id)?;
    let meta = owned_meta(&id, owner_uid)?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let _guard = store
        .lock_idle_session(meta.id.as_str())
        .map_err(|err| err.to_string())?;
    let mut presentation = read_presentation(&meta.id)?;
    if presentation.presentation_id.is_none() {
        presentation.presentation_id = Some(derive_presentation_id(&meta.id));
    }
    if let Some(title) = title {
        presentation.title = Some(title);
    }
    if let Some(archived) = request.archived {
        presentation.archived = archived;
    }
    if let Some(deleted) = request.deleted {
        presentation.deleted = deleted;
    }
    presentation.updated_at = Some(Utc::now().to_rfc3339());
    let view = owner_memory::read_view(owner_uid, meta.id.to_string(), DEFAULT_HISTORY_LIMIT)?;
    let value = conversation_response(&meta, presentation.clone(), view, owner_uid)?;
    write_presentation(&meta.id, &presentation)?;
    Ok(value)
}

pub(super) fn fork(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let request: ForkRequest = serde_json::from_value(params)
        .map_err(|err| format!("invalid conversation fork: {err}"))?;
    let owner_uid = owner_uid(client)?;
    let id = ConversationId::parse(&request.id)?;
    owned_meta(&id, owner_uid)?;
    let store = Store::open_default().map_err(|err| err.to_string())?;
    let _guard = store
        .lock_idle_session(id.session_id().as_str())
        .map_err(|err| err.to_string())?;
    let parent = owned_meta(&id, owner_uid)?;
    let parent_presentation = read_presentation(&parent.id)?;
    if parent_presentation.deleted {
        return Err("restore the soft-deleted conversation before forking it".to_string());
    }
    let (memory, snapshot) =
        owner_memory::read_snapshot(owner_uid, parent.id.to_string(), request.before_user_turn)?;
    let inherited = bindings::verify_retained(
        &parent,
        &parent_presentation,
        &snapshot.bindings,
        snapshot.visible_count(),
        owner_uid,
    )
    .map_err(|error| format!("cannot fork task history: {error}"))?;
    let title = presentation_metadata(&parent, parent_presentation, memory)?.title;
    let owner_home = super::system_caps::verified_owner_home(owner_uid)?;
    let parent_id = parent.id.to_string();
    let (_, value) = super::tasks::create_agent_session_with(
        "Agent conversation".to_string(),
        owner_uid,
        &owner_home,
        |sid| {
            let presentation = Presentation {
                presentation_id: Some(derive_presentation_id(sid)),
                title: Some(title),
                parent_id: Some(parent_id),
                inherited_tasks: inherited.memberships,
                ..Presentation::default()
            };
            write_presentation(sid, &presentation)?;
            let view = if snapshot.is_empty() {
                OwnerMemoryView {
                    metadata: Default::default(),
                    history: Default::default(),
                }
            } else {
                owner_memory::install_snapshot(
                    owner_uid,
                    sid.to_string(),
                    snapshot,
                    DEFAULT_HISTORY_LIMIT,
                )?
            };
            let meta = session::get_meta(sid).map_err(|err| err.to_string())?;
            conversation_response(&meta, presentation, view, owner_uid)
        },
    )?;
    Ok(value)
}

fn get_for_id(id: &ConversationId, owner_uid: u32, limit: usize) -> Result<Value, String> {
    let meta = owned_meta(id, owner_uid)?;
    let presentation = read_presentation(&meta.id)?;
    let view = owner_memory::read_view(owner_uid, meta.id.to_string(), limit)?;
    conversation_response(&meta, presentation, view, owner_uid)
}

fn conversation_response(
    meta: &session::SessionMeta,
    presentation: Presentation,
    view: OwnerMemoryView,
    owner_uid: u32,
) -> Result<Value, String> {
    let mut history = view.history;
    let execution = match bindings::verify(
        meta,
        &presentation,
        &history.bindings,
        history.message_count,
        owner_uid,
    ) {
        Ok(verified) => {
            bindings::annotate(&mut history);
            verified.jobs
        }
        Err(error) => {
            let mut execution = jobs::load(&meta.id, owner_uid)?;
            execution.mark_unverified(error);
            execution
        }
    };
    let metadata = presentation_metadata(meta, presentation, view.metadata)?;
    response(ConversationResponse {
        conversation: Conversation {
            metadata,
            history,
            execution,
        },
    })
}

fn lookup_session(lookup: &ConversationLookup, owner_uid: u32) -> Result<ConversationId, String> {
    match lookup {
        ConversationLookup::Canonical(id) => {
            owned_meta(id, owner_uid)?;
            Ok(id.clone())
        }
        ConversationLookup::Presentation(presentation_id) => {
            let mut matches = Vec::new();
            for meta in session::list().map_err(|err| err.to_string())? {
                if !is_conversation(&meta, owner_uid) {
                    continue;
                }
                let presentation = read_presentation(&meta.id)?;
                let candidate = presentation
                    .presentation_id
                    .unwrap_or_else(|| derive_presentation_id(&meta.id));
                if &candidate == presentation_id {
                    matches.push(ConversationId::parse(meta.id.as_str())?);
                }
            }
            match matches.len() {
                0 => Err(format!(
                    "conversation not found: {}",
                    presentation_id.as_str()
                )),
                1 => Ok(matches.remove(0)),
                _ => Err(format!(
                    "conversation presentation id is ambiguous: {}",
                    presentation_id.as_str()
                )),
            }
        }
    }
}

fn owned_meta(id: &ConversationId, owner_uid: u32) -> Result<session::SessionMeta, String> {
    let meta = session::get_meta(id.session_id())
        .map_err(|_| format!("conversation not found: {}", id.session_id()))?;
    if is_conversation(&meta, owner_uid) {
        Ok(meta)
    } else {
        Err(format!("conversation not found: {}", id.session_id()))
    }
}

fn is_conversation(meta: &session::SessionMeta, owner_uid: u32) -> bool {
    meta.owner_uid == Some(owner_uid)
        && meta.creator_runtime.as_deref() == Some("clawd")
        && meta.role == Some(Role::Observer)
        && meta.origin == Some(SessionOrigin::SystemAgentTask)
}

fn presentation_metadata(
    meta: &session::SessionMeta,
    presentation: Presentation,
    memory: crate::agent::memory::conversations::ConversationMetadata,
) -> Result<ConversationMetadata, String> {
    let created = timestamp(&meta.created_at)?;
    let mut updated = created;
    if let Some(value) = presentation.updated_at.as_deref() {
        updated = updated.max(timestamp(value)?);
    }
    if let Some(value) = memory.updated_at_ms {
        let memory_updated = Utc
            .timestamp_millis_opt(value)
            .single()
            .ok_or_else(|| format!("invalid conversation timestamp: {value}"))?;
        updated = updated.max(memory_updated);
    }
    let parent_id = presentation
        .parent_id
        .as_deref()
        .map(ConversationId::parse)
        .transpose()?
        .map(|id| id.session_id().to_string());
    let fallback_title = if meta.purpose == "Agent conversation" || meta.purpose.trim().is_empty() {
        DEFAULT_TITLE.to_string()
    } else {
        validate_title(meta.purpose.clone())?
    };
    let memory_title = memory.title.map(validate_title).transpose()?;
    Ok(ConversationMetadata {
        id: meta.id.to_string(),
        presentation_id: presentation
            .presentation_id
            .unwrap_or_else(|| derive_presentation_id(&meta.id)),
        title: presentation
            .title
            .or(memory_title)
            .unwrap_or(fallback_title),
        created_at: created.to_rfc3339(),
        updated_at: updated.to_rfc3339(),
        archived: presentation.archived,
        deleted: presentation.deleted,
        parent_id,
    })
}

fn derive_presentation_id(id: &SessionId) -> PresentationId {
    let mut hasher = Sha256::new();
    hasher.update(b"claw-tui/thread/v1\0");
    hasher.update(id.as_str().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    PresentationId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

fn read_presentation(id: &SessionId) -> Result<Presentation, String> {
    let value = session::read_state(id, PRESENTATION_NAMESPACE).map_err(|err| err.to_string())?;
    if value.is_null() {
        Ok(Presentation::default())
    } else {
        let mut presentation: Presentation = serde_json::from_value(value)
            .map_err(|err| format!("invalid conversation presentation state for {id}: {err}"))?;
        presentation.title = presentation.title.map(validate_title).transpose()?;
        Ok(presentation)
    }
}

fn write_presentation(id: &SessionId, value: &Presentation) -> Result<(), String> {
    session::write_state(
        id,
        PRESENTATION_NAMESPACE,
        serde_json::to_value(value).map_err(|err| err.to_string())?,
    )
    .map_err(|err| err.to_string())
}

fn validate_title(value: String) -> Result<String, String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("conversation title cannot be empty".to_string());
    }
    if value.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "conversation title exceeds {MAX_TITLE_CHARS} characters"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err("conversation title cannot contain control characters".to_string());
    }
    Ok(value)
}

fn bounded_limit(
    value: Option<u32>,
    default: usize,
    maximum: usize,
    name: &str,
) -> Result<usize, String> {
    let value = value.map(|item| item as usize).unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(format!(
            "conversation {name} limit must be within 1..={maximum}"
        ));
    }
    Ok(value)
}

fn owner_uid(client: &ClientIdentity) -> Result<u32, String> {
    let uid = client
        .uid
        .ok_or_else(|| "conversation command requires peer uid".to_string())?;
    if uid == 0 {
        return Err(ROOT_OWNER_REFUSAL.to_string());
    }
    Ok(uid)
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| format!("invalid conversation timestamp: {err}"))
}

fn response<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    include!("../../test/unit/clawd/conversations.rs");
}
