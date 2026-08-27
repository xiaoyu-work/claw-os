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
    /// Neither this field nor `message` is ever persisted, so this is
    /// where a handler answers the calling peer with machine-readable
    /// detail — currently the approval request ids an App launch is
    /// waiting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Stable, input-free classification of this failure.
    ///
    /// Handler messages are built with `format!` from request fields,
    /// resolved paths and provider text, so audit sinks reduce
    /// `message` to a length and keyed digest. A route that wants its
    /// failure named in the audit trail sets this to a `&'static str`,
    /// which by construction cannot carry caller or secret material.
    /// Never crosses the wire.
    #[serde(default, skip)]
    pub audit_class: Option<&'static str>,
}

/// A broker failure, optionally carrying structured data for the peer
/// and a stable class for the audit trail.
///
/// Handlers that have nothing extra to say keep returning `String`;
/// `From<String>` makes those paths unchanged, and their messages stay
/// unclassified — recorded as size and digest only.
#[derive(Debug, Clone)]
pub struct BrokerError {
    pub message: String,
    pub data: Option<Value>,
    pub audit_class: Option<&'static str>,
}

impl BrokerError {
    pub fn with_data(message: impl Into<String>, data: Value) -> Self {
        Self {
            message: message.into(),
            data: Some(data),
            audit_class: None,
        }
    }

    /// Attach the stable class audit records store in place of the
    /// message text.
    pub fn classified(mut self, class: &'static str) -> Self {
        self.audit_class = Some(class);
        self
    }
}

impl From<String> for BrokerError {
    fn from(message: String) -> Self {
        Self {
            message,
            data: None,
            audit_class: None,
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
                audit_class: None,
            }),
        }
    }

    /// Fail with a message the audit trail may name by its stable
    /// class instead of storing a digest of the text.
    pub fn error_classified(
        id: Option<Value>,
        code: impl Into<String>,
        class: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
                data: None,
                audit_class: Some(class),
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
                audit_class: error.audit_class,
            }),
        }
    }

    /// Reduce this response to what a durable record may say about it.
    ///
    /// The handler message and the peer-only `data` payload are left
    /// behind here, so no sink can reach them.
    pub fn audit_facts(&self) -> crate::audit_policy::ResponseFacts {
        crate::audit_policy::ResponseFacts {
            ok: self.ok,
            error: self.error.as_ref().map(|err| {
                crate::audit_policy::error_facts(&err.code, err.audit_class, &err.message)
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
