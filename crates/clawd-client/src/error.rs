use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{variable} is set but empty")]
    EmptySocketConfiguration { variable: &'static str },
    #[error("clawd Unix socket transport is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("clawd request could not be encoded")]
    Encode(#[source] serde_json::Error),
    #[error("clawd request is {actual} bytes; maximum is {maximum}")]
    RequestTooLarge { actual: usize, maximum: usize },
    #[error("connecting to clawd at {path} timed out")]
    ConnectTimeout { path: String },
    #[error("failed to connect to clawd at {path}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("writing clawd request timed out")]
    WriteTimeout,
    #[error("failed to write clawd request")]
    Write(#[source] std::io::Error),
    #[error("reading clawd response timed out")]
    ReadTimeout,
    #[error("clawd response ended before its declared frame was complete")]
    TruncatedResponse,
    #[error("failed to read clawd response")]
    Read(#[source] std::io::Error),
    #[error("clawd response used unsupported frame magic, kind, or flags")]
    UnsupportedFrame,
    #[error("clawd response declares {actual} bytes; maximum is {maximum}")]
    ResponseTooLarge { actual: usize, maximum: usize },
    #[error("clawd response is not a valid broker envelope")]
    MalformedResponse(#[source] serde_json::Error),
    #[error("clawd responded with protocol v{actual}; this client speaks v{expected}")]
    UnsupportedVersion { actual: u32, expected: u32 },
    #[error("clawd response id {actual} does not match request id {expected}")]
    MismatchedRequestId { expected: String, actual: String },
    #[error("clawd response envelope is inconsistent: {0}")]
    InvalidResponse(&'static str),
}

/// Stable public categories returned by clawd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidJson,
    InvalidRequest,
    UnknownCommand,
    NotAuthorized,
    Unavailable,
    ProtocolError,
    ExecutionFailed,
    Other(String),
}

impl ErrorCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::InvalidRequest => "invalid_request",
            Self::UnknownCommand => "unknown_command",
            Self::NotAuthorized => "not_authorized",
            Self::Unavailable => "unavailable",
            Self::ProtocolError => "protocol_error",
            Self::ExecutionFailed => "execution_failed",
            Self::Other(code) => code,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Ok(match code.as_str() {
            "invalid_json" => Self::InvalidJson,
            "invalid_request" => Self::InvalidRequest,
            "unknown_command" => Self::UnknownCommand,
            "not_authorized" => Self::NotAuthorized,
            "unavailable" => Self::Unavailable,
            "protocol_error" => Self::ProtocolError,
            "execution_failed" => Self::ExecutionFailed,
            _ => Self::Other(code),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "clawd request failed ({}): {}",
            self.code, self.message
        )
    }
}

impl std::error::Error for RemoteError {}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Remote(#[from] RemoteError),
}
