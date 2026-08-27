use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub command: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    /// Structured payload delivered only on this connection's response.
    ///
    /// Broker audit and journal records persist `code` and `message`,
    /// never this field, so it is where a handler answers the calling
    /// peer with machine-readable detail — currently the approval
    /// request ids an App launch is waiting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A broker failure, optionally carrying structured data for the peer.
///
/// Handlers that have nothing extra to say keep returning `String`;
/// `From<String>` makes those paths unchanged.
#[derive(Debug, Clone)]
pub struct BrokerError {
    pub message: String,
    pub data: Option<Value>,
}

impl BrokerError {
    pub fn with_data(message: impl Into<String>, data: Value) -> Self {
        Self {
            message: message.into(),
            data: Some(data),
        }
    }
}

impl From<String> for BrokerError {
    fn from(message: String) -> Self {
        Self {
            message,
            data: None,
        }
    }
}

impl std::fmt::Display for BrokerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Response {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn error_with_data(
        id: Option<Value>,
        code: impl Into<String>,
        error: BrokerError,
    ) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: error.message,
                data: error.data,
            }),
        }
    }
}

pub fn encode_response(response: &Response) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    Ok(line)
}

pub fn encode_request(request: &Request) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/protocol.rs"
    ));
}
