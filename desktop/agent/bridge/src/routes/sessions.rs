//! Canonical conversation routes for the native Desktop Agent.

use std::collections::BTreeSet;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::DateTime;
use cos_agent_protocol::{
    ErrorCode, HistoryMessage, HistoryResponse, SessionSummary, SessionUpdateRequest,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{api_error::ApiError, state::AppState, translation};
use clawd_client::Command;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
struct ConversationList {
    conversations: Vec<Conversation>,
}

#[derive(Debug, Deserialize)]
struct ConversationResponse {
    conversation: Conversation,
}

#[derive(Debug, Deserialize)]
struct Conversation {
    id: String,
    presentation_id: String,
    title: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    deleted: bool,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    messages: Vec<HistoryMessage>,
    #[serde(default)]
    message_count: u64,
}

pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    let canonical = state
        .clawd
        .call(
            Command::AgentConversationList,
            json!({ "archived": query.archived, "limit": 1_000 }),
        )
        .await
        .map_err(|error| ApiError::service_unavailable(error.to_string()))?;
    let canonical: ConversationList = serde_json::from_value(canonical)
        .map_err(|error| ApiError::bad_gateway(format!("invalid conversation list: {error}")))?;
    let mut ids = BTreeSet::new();
    let mut sessions = canonical
        .conversations
        .into_iter()
        .filter(|conversation| !conversation.deleted)
        .map(|conversation| {
            ids.insert(conversation.id.clone());
            summary(conversation, true, false)
        })
        .collect::<Result<Vec<_>, _>>()?;

    if !query.archived {
        let legacy = state
            .clawd
            .call(Command::MemorySessions, json!({ "limit": 1_000 }))
            .await
            .map_err(|error| ApiError::service_unavailable(error.to_string()))?;
        sessions.extend(
            translation::sessions(legacy)
                .map_err(ApiError::bad_gateway)?
                .into_iter()
                .filter(|session| !ids.contains(&session.id))
                .map(|mut session| {
                    session.legacy = true;
                    session.manageable = false;
                    session
                }),
        );
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_ts_ms.unwrap_or_default()));
    Ok(Json(sessions))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionSummary>, ApiError> {
    match get_conversation(&state, &id, 1).await {
        Ok(conversation) => summary(conversation, true, false).map(Json),
        Err(primary) => {
            let value = state
                .clawd
                .call(Command::MemorySessions, json!({ "limit": 1_000 }))
                .await
                .map_err(|_| primary)?;
            translation::sessions(value)
                .map_err(ApiError::bad_gateway)?
                .into_iter()
                .find(|session| session.id == id)
                .map(|mut session| {
                    session.legacy = true;
                    session.manageable = false;
                    Json(session)
                })
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::NOT_FOUND,
                        ErrorCode::NotFound,
                        "session not found",
                    )
                })
        }
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SessionUpdateRequest>,
) -> Result<Json<SessionSummary>, ApiError> {
    if request.title.is_none() && request.archived.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "conversation update requires a field",
        ));
    }
    let mut params = json!({ "id": id });
    if let Some(title) = request.title {
        params["title"] = Value::String(title);
    }
    if let Some(archived) = request.archived {
        params["archived"] = Value::Bool(archived);
    }
    let value = state
        .clawd
        .call(Command::AgentConversationUpdate, params)
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    parse_conversation(value)
        .and_then(|conversation| summary(conversation, true, false))
        .map(Json)
}

pub async fn fork(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionSummary>, ApiError> {
    let value = state
        .clawd
        .call(Command::AgentConversationFork, json!({ "id": id }))
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    parse_conversation(value)
        .and_then(|conversation| summary(conversation, true, false))
        .map(Json)
}

pub async fn delete_one(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        ErrorCode::NotImplemented,
        "conversation deletion is not implemented",
    ))
}

pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<HistoryResponse>, ApiError> {
    match get_conversation(&state, &id, 500).await {
        Ok(conversation) => {
            let messages = conversation.messages;
            Ok(Json(HistoryResponse {
                session_id: conversation.id,
                n: messages.len(),
                messages,
            }))
        }
        Err(primary) => {
            let value = state
                .clawd
                .call(
                    Command::MemoryHistory,
                    json!({ "session_id": id, "limit": 500 }),
                )
                .await
                .map_err(|_| primary)?;
            translation::history(value)
                .map(Json)
                .map_err(ApiError::bad_gateway)
        }
    }
}

async fn get_conversation(
    state: &AppState,
    id: &str,
    limit: u32,
) -> Result<Conversation, ApiError> {
    let value = state
        .clawd
        .call(
            Command::AgentConversationGet,
            json!({ "id": id, "limit": limit }),
        )
        .await
        .map_err(|error| ApiError::bad_gateway(error.to_string()))?;
    parse_conversation(value)
}

fn parse_conversation(value: Value) -> Result<Conversation, ApiError> {
    serde_json::from_value::<ConversationResponse>(value)
        .map(|response| response.conversation)
        .map_err(|error| ApiError::bad_gateway(format!("invalid conversation: {error}")))
}

fn summary(
    conversation: Conversation,
    manageable: bool,
    legacy: bool,
) -> Result<SessionSummary, ApiError> {
    let message_count = i64::try_from(conversation.message_count)
        .map_err(|_| ApiError::bad_gateway("conversation message count is too large"))?;
    let last_ts_ms = DateTime::parse_from_rfc3339(&conversation.updated_at)
        .map_err(|error| ApiError::bad_gateway(format!("invalid conversation timestamp: {error}")))?
        .timestamp_millis();
    Ok(SessionSummary {
        id: conversation.id,
        presentation_id: Some(conversation.presentation_id),
        title: conversation.title,
        created_at: Some(conversation.created_at),
        updated_at: Some(conversation.updated_at),
        last_ts_ms: Some(last_ts_ms),
        message_count,
        archived: conversation.archived,
        manageable,
        legacy,
        parent_id: conversation.parent_id,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/routes/sessions.rs"
    ));
}
