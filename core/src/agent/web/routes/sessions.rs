//! Owner-scoped Web adapters for the canonical Agent conversation service.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SessionMetadata {
    id: String,
    presentation_id: String,
    title: String,
    created_at: String,
    updated_at: String,
    archived: bool,
    deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConversationList {
    conversations: Vec<SessionMetadata>,
    conversation_count: u64,
    conversations_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct LegacySession {
    id: String,
    #[serde(default)]
    title: Option<String>,
    last_ts_ms: i64,
    message_count: u64,
}

#[derive(Debug, Deserialize)]
struct LegacySessionList {
    sessions: Vec<LegacySession>,
}

#[derive(Debug, Deserialize)]
struct Conversation {
    #[serde(flatten)]
    metadata: SessionMetadata,
    messages: Vec<Value>,
    message_count: u64,
    messages_truncated: bool,
}

#[derive(Debug, Deserialize)]
struct ConversationResponse {
    conversation: Conversation,
}

pub async fn list(
    State(_state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let archived = query.archived.unwrap_or(false);
    let result = super::clawd::request(
        Command::AgentConversationList,
        json!({ "archived": archived, "limit": 1_000 }),
    )
    .await
    .map_err(super::clawd::RpcError::into_api_error)?;
    let legacy = if archived {
        None
    } else {
        Some(
            super::clawd::request(Command::MemorySessions, json!({ "limit": 1_000 }))
                .await
                .map_err(super::clawd::RpcError::into_api_error)?,
        )
    };
    project_list(result, legacy).map(Json)
}

pub async fn detail(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match get_conversation(&id, 1).await {
        Ok(conversation) => {
            let metadata = conversation.metadata;
            Ok(Json(json!({
                "id": metadata.id,
                "presentation_id": metadata.presentation_id,
                "title": metadata.title,
                "created_at": metadata.created_at,
                "updated_at": metadata.updated_at,
                "archived": metadata.archived,
                "deleted": metadata.deleted,
                "parent_id": metadata.parent_id,
                "message_count": conversation.message_count,
                "messages_truncated": conversation.messages_truncated,
                "manageable": true,
                "legacy": false,
            })))
        }
        Err(error) => legacy_detail(&id).await.or(Err(error)),
    }
}

pub async fn history(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match get_conversation(&id, 500).await {
        Ok(conversation) => Ok(Json(json!({
            "session_id": conversation.metadata.id,
            "n": conversation.messages.len(),
            "message_count": conversation.message_count,
            "messages_truncated": conversation.messages_truncated,
            "messages": conversation.messages,
        }))),
        Err(error) => legacy_history(&id).await.or(Err(error)),
    }
}

pub async fn update(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut params = json!({ "id": id });
    if let Some(title) = body.title {
        params["title"] = json!(title);
    }
    if let Some(archived) = body.archived {
        params["archived"] = json!(archived);
    }
    let result = super::clawd::request(Command::AgentConversationUpdate, params)
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let conversation = parse_conversation(result)?;
    Ok(Json(
        json!({ "session": project_metadata(conversation.metadata) }),
    ))
}

pub async fn fork(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(Command::AgentConversationFork, json!({ "id": id }))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let conversation = parse_conversation(result)?;
    Ok(Json(
        json!({ "session": project_metadata(conversation.metadata) }),
    ))
}

async fn get_conversation(id: &str, limit: u32) -> Result<Conversation, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(
        Command::AgentConversationGet,
        json!({ "id": id, "limit": limit }),
    )
    .await
    .map_err(super::clawd::RpcError::into_api_error)?;
    parse_conversation(result)
}

async fn legacy_detail(id: &str) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(Command::MemorySessions, json!({ "limit": 1_000 }))
        .await
        .map_err(super::clawd::RpcError::into_api_error)?;
    let list: LegacySessionList = serde_json::from_value(result)
        .map_err(|error| invalid_broker_response(format!("legacy session list: {error}")))?;
    let session = list
        .sessions
        .into_iter()
        .find(|session| session.id == id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("session not found: {id}") })),
            )
        })?;
    Ok(Json(project_legacy(session)))
}

async fn legacy_history(id: &str) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = super::clawd::request(
        Command::MemoryHistory,
        json!({ "session_id": id, "limit": 500 }),
    )
    .await
    .map_err(super::clawd::RpcError::into_api_error)?;
    let messages = result
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_broker_response("legacy history omitted messages".to_string()))?;
    if messages.is_empty() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("session not found: {id}") })),
        ));
    }
    Ok(Json(result))
}

fn project_list(result: Value, legacy: Option<Value>) -> Result<Value, (StatusCode, Json<Value>)> {
    let list: ConversationList = serde_json::from_value(result)
        .map_err(|error| invalid_broker_response(format!("conversation list: {error}")))?;
    let mut canonical_ids = std::collections::BTreeSet::new();
    let mut sessions = list
        .conversations
        .into_iter()
        .map(|metadata| {
            canonical_ids.insert(metadata.id.clone());
            project_metadata(metadata)
        })
        .collect::<Vec<_>>();
    let mut legacy_truncated = false;
    if let Some(legacy) = legacy {
        let legacy: LegacySessionList = serde_json::from_value(legacy)
            .map_err(|error| invalid_broker_response(format!("legacy session list: {error}")))?;
        legacy_truncated = legacy.sessions.len() == 1_000;
        sessions.extend(
            legacy
                .sessions
                .into_iter()
                .filter(|session| !canonical_ids.contains(&session.id))
                .map(project_legacy),
        );
    }
    let total = list
        .conversation_count
        .saturating_add((sessions.len() - canonical_ids.len()) as u64);
    Ok(json!({
        "n": sessions.len(),
        "total": total,
        "truncated": list.conversations_truncated || legacy_truncated,
        "sessions": sessions,
    }))
}

fn project_metadata(metadata: SessionMetadata) -> Value {
    json!({
        "id": metadata.id,
        "presentation_id": metadata.presentation_id,
        "title": metadata.title,
        "created_at": metadata.created_at,
        "updated_at": metadata.updated_at,
        "archived": metadata.archived,
        "deleted": metadata.deleted,
        "parent_id": metadata.parent_id,
        "manageable": true,
        "legacy": false,
    })
}

fn project_legacy(session: LegacySession) -> Value {
    json!({
        "id": session.id,
        "title": session.title,
        "updated_at": session.last_ts_ms,
        "message_count": session.message_count,
        "archived": false,
        "manageable": false,
        "legacy": true,
    })
}

fn parse_conversation(result: Value) -> Result<Conversation, (StatusCode, Json<Value>)> {
    serde_json::from_value::<ConversationResponse>(result)
        .map(|response| response.conversation)
        .map_err(|error| invalid_broker_response(format!("conversation: {error}")))
}

fn invalid_broker_response(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": format!("invalid clawd {message}") })),
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/sessions.rs"
    ));
}
