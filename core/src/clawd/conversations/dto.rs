//! @agent-file
//! Responsibility: define the closed internal request and response shapes for conversation routes.
//! Key dependencies: canonical SessionId parsing, UUID validation, and shared history presentation.
//! Constraints: canonical identity and frontend presentation identity remain distinct types.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::agent::memory::conversations::ConversationHistoryPage;
use crate::session::SessionId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConversationId(SessionId);

impl ConversationId {
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        value
            .parse::<SessionId>()
            .map(Self)
            .map_err(|err| format!("invalid conversation id: {err}"))
    }

    pub(super) fn session_id(&self) -> &SessionId {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(super) struct PresentationId(String);

impl PresentationId {
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        uuid::Uuid::parse_str(value)
            .map(|id| Self(id.to_string()))
            .map_err(|_| "invalid conversation presentation id".to_string())
    }

    pub(super) fn from_uuid(value: uuid::Uuid) -> Self {
        Self(value.to_string())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PresentationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ConversationLookup {
    Canonical(ConversationId),
    Presentation(PresentationId),
}

impl ConversationLookup {
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        if let Ok(id) = ConversationId::parse(value) {
            return Ok(Self::Canonical(id));
        }
        PresentationId::parse(value).map(Self::Presentation)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateRequest {
    #[serde(default)]
    pub(super) title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetRequest {
    pub(super) id: String,
    #[serde(default)]
    pub(super) limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListRequest {
    #[serde(default)]
    pub(super) archived: Option<bool>,
    #[serde(default)]
    pub(super) limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateRequest {
    pub(super) id: String,
    #[serde(default)]
    pub(super) title: Option<String>,
    #[serde(default)]
    pub(super) archived: Option<bool>,
    #[serde(default)]
    pub(super) deleted: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForkRequest {
    pub(super) id: String,
    #[serde(default)]
    pub(super) before_user_turn: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ConversationMetadata {
    pub(super) id: String,
    pub(super) presentation_id: PresentationId,
    pub(super) title: String,
    pub(super) created_at: String,
    pub(super) updated_at: String,
    pub(super) archived: bool,
    pub(super) deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parent_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Conversation {
    #[serde(flatten)]
    pub(super) metadata: ConversationMetadata,
    #[serde(flatten)]
    pub(super) history: ConversationHistoryPage,
    #[serde(flatten)]
    pub(super) execution: super::jobs::ConversationJobs,
}

#[derive(Debug, Serialize)]
pub(super) struct ConversationResponse {
    pub(super) conversation: Conversation,
}

#[derive(Debug, Serialize)]
pub(super) struct ListResponse {
    pub(super) conversations: Vec<ConversationMetadata>,
    pub(super) conversation_count: u64,
    pub(super) conversations_truncated: bool,
}
