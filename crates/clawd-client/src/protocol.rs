use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ClientError, Error, RemoteError};

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAGIC: [u8; 4] = *b"CBK1";
pub const KIND_REQUEST: u8 = 0x01;
pub const KIND_RESPONSE: u8 = 0x02;
pub const HEADER_BYTES: usize = 10;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_REQUEST_ID_BYTES: usize = 64;

/// Routes used by unprivileged desktop consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Command {
    #[serde(rename = "task.submit")]
    TaskSubmit,
    #[serde(rename = "task.stream")]
    TaskStream,
    #[serde(rename = "task.cancel")]
    TaskCancel,
    #[serde(rename = "memory.sessions")]
    MemorySessions,
    #[serde(rename = "memory.history")]
    MemoryHistory,
    #[serde(rename = "permission.pending")]
    PermissionPending,
    #[serde(rename = "notification.subscribe")]
    NotificationSubscribe,
    #[serde(rename = "notification.delivery.claim")]
    NotificationDeliveryClaim,
    #[serde(rename = "notification.delivery.complete")]
    NotificationDeliveryComplete,
}

impl Command {
    pub const ALL: [Self; 9] = [
        Self::TaskSubmit,
        Self::TaskStream,
        Self::TaskCancel,
        Self::MemorySessions,
        Self::MemoryHistory,
        Self::PermissionPending,
        Self::NotificationSubscribe,
        Self::NotificationDeliveryClaim,
        Self::NotificationDeliveryComplete,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskSubmit => "task.submit",
            Self::TaskStream => "task.stream",
            Self::TaskCancel => "task.cancel",
            Self::MemorySessions => "memory.sessions",
            Self::MemoryHistory => "memory.history",
            Self::PermissionPending => "permission.pending",
            Self::NotificationSubscribe => "notification.subscribe",
            Self::NotificationDeliveryClaim => "notification.delivery.claim",
            Self::NotificationDeliveryComplete => "notification.delivery.complete",
        }
    }
}

impl std::fmt::Display for Command {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.is_empty() {
            return Err("request id must not be empty");
        }
        if raw.len() > MAX_REQUEST_ID_BYTES {
            return Err("request id exceeds its maximum length");
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("request id contains characters outside [A-Za-z0-9._-]");
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    pub id: RequestId,
    pub command: Command,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(command: Command, params: Value) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: RequestId::generate(),
            command,
            params,
        }
    }

    pub fn with_id(command: Command, params: Value, id: RequestId) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            command,
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub v: u32,
    pub id: RequestId,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RemoteError>,
}

impl Response {
    pub(crate) fn validate(self, request_id: &RequestId) -> Result<Self, ClientError> {
        if self.v != PROTOCOL_VERSION {
            return Err(ClientError::UnsupportedVersion {
                actual: self.v,
                expected: PROTOCOL_VERSION,
            });
        }
        if self.id != *request_id {
            return Err(ClientError::MismatchedRequestId {
                expected: request_id.as_str().to_string(),
                actual: self.id.as_str().to_string(),
            });
        }
        match (self.ok, self.result.is_some(), self.error.is_some()) {
            (true, true, false) | (false, false, true) => Ok(self),
            (true, _, _) => Err(ClientError::InvalidResponse(
                "successful response must contain only result",
            )),
            (false, _, _) => Err(ClientError::InvalidResponse(
                "failed response must contain only error",
            )),
        }
    }

    pub fn into_result(self) -> Result<Value, Error> {
        if self.ok {
            self.result.ok_or_else(|| {
                ClientError::InvalidResponse("successful response has no result").into()
            })
        } else {
            Err(self
                .error
                .ok_or(ClientError::InvalidResponse("failed response has no error"))?
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/protocol.rs"
    ));
}
