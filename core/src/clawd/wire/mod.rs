//! The versioned broker envelope.
//!
//! # Frame
//!
//! Every message on `/run/cos/clawd.sock` is one length-prefixed frame:
//!
//! ```text
//! magic  4 bytes  b"CBK1"
//! kind   1 byte   0x01 request, 0x02 response
//! flags  1 byte   reserved, must be zero
//! len    4 bytes  big-endian payload length
//! body   len bytes of UTF-8 JSON
//! ```
//!
//! The header is fixed-size and is read before anything is allocated,
//! so a declared length above the peer's ceiling is refused without the
//! daemon ever reserving the memory. There is no terminator to scan
//! for: a short read is a truncation, not a partial record the daemon
//! waits on forever.
//!
//! # Envelope
//!
//! The body is a closed object — `deny_unknown_fields` — carrying the
//! protocol version, a bounded correlation id, the route name and that
//! route's typed parameters. There is no legacy shape and no fallback
//! parse: a frame whose magic does not match is refused with a named
//! error and the connection closes. `clawd` answers a peer that opens
//! with a bare JSON object (the pre-v1 newline protocol) with one
//! newline-terminated JSON error so an out-of-date `cos` prints
//! something actionable, but that path never parses the request, never
//! authorizes and never dispatches.
//!
//! The correlation id is exactly that: correlation. It selects nothing,
//! authorizes nothing, and is echoed back so a caller can prove the
//! response it read belongs to the request it sent. One request per
//! connection means responses cannot be crossed in the first place.

pub mod bounded;
pub mod requests;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::ErrorBody;
use super::routes::Command;

/// Wire protocol both ends must agree on.
pub const PROTOCOL_VERSION: u32 = 1;

/// Frame magic. Also the version marker: a future incompatible framing
/// changes these bytes, so an old daemon rejects a new client at the
/// header instead of mis-parsing its body.
pub const MAGIC: [u8; 4] = *b"CBK1";

pub const KIND_REQUEST: u8 = 0x01;
pub const KIND_RESPONSE: u8 = 0x02;

/// Fixed header: magic, kind, flags, length.
pub const HEADER_BYTES: usize = 10;

/// Largest request body the daemon will read. Comfortably above the
/// biggest real request (an agent prompt plus context) and far below
/// anything that could pressure the broker.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Largest response body the daemon will write. `task.result` and
/// `system.operations` are the wide ones; a response above this is
/// replaced by a named error rather than truncated, so a client can
/// never mistake a partial document for a complete one.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Longest correlation id a peer may choose.
pub const MAX_REQUEST_ID_BYTES: usize = 64;

/// Placeholder echoed when the daemon refuses a frame before it could
/// read a well-formed id.
pub const UNKNOWN_REQUEST_ID: &str = "unknown";

/// A bounded, opaque correlation id.
///
/// Never authority: the broker derives uid, pid and session from the
/// kernel, so knowing or guessing an id gains nothing. It exists so a
/// response can be matched to its request and so a repeated mutation
/// can be recognised.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    pub fn unknown() -> Self {
        Self(UNKNOWN_REQUEST_ID.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// A request as an in-repo client builds it.
///
/// `command` is the [`Command`] enum, so a caller inside this crate
/// cannot name a route that does not exist, and the registry's static
/// name is what crosses the wire.
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
}

/// The daemon's own view of an inbound envelope.
///
/// Deliberately *not* the client type: the route name arrives as a
/// bounded token so the daemon can tell "unknown route" apart from
/// "malformed envelope" and audit them separately, and so an unknown
/// name is refused rather than being decoded into a variant that
/// happens to sort nearby.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundRequest {
    pub v: u32,
    pub id: RequestId,
    pub command: bounded::Token<64>,
    #[serde(default)]
    pub params: Value,
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
    pub error: Option<ErrorBody>,
}

// ---------------------------------------------------------------------------
// Faults
// ---------------------------------------------------------------------------

/// Everything that can go wrong before a route runs.
///
/// Each variant maps to a stable class and a message built only from
/// `&'static str`, so the text answered to the peer and the class
/// stored in the audit trail carry no caller bytes at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The frame did not start with [`MAGIC`], or declared a kind or
    /// flag this daemon does not serve — including a pre-v1 newline
    /// request.
    UnsupportedFrame,
    /// The declared length is above the peer's ceiling. Refused before
    /// the body is read.
    FrameTooLarge,
    /// The peer closed, or stopped writing, part-way through a frame.
    TruncatedFrame,
    /// The body is not UTF-8, or not JSON.
    MalformedBody,
    /// A second frame arrived on a connection that is allowed one.
    ExtraFrame,
    /// The body parsed as JSON but is not a v1 envelope.
    InvalidEnvelope,
    /// A well-formed envelope naming another protocol version.
    UnsupportedVersion,
    /// A well-formed envelope naming a route that does not exist.
    UnknownCommand,
    /// The route's typed parameters did not decode.
    InvalidParams,
    /// The kernel attached no credentials to the message.
    MissingCredentials,
    /// The credentials changed between segments of one frame.
    CredentialsChanged,
    /// The peer attached file descriptors. They were closed.
    DescriptorPassing,
    /// The sending process could not be re-verified through `/proc`.
    PeerUnverified,
    /// The peer did not finish its request inside the read deadline.
    ReadTimeout,
    /// The peer did not drain its response inside the write deadline.
    WriteTimeout,
    /// The route produced more than [`MAX_RESPONSE_BYTES`].
    ResponseTooLarge,
    /// Global or per-uid connection ceiling.
    TooManyConnections,
    /// Global or per-uid in-flight ceiling.
    TooManyRequests,
    /// The route's own concurrency ceiling.
    RouteBusy,
    /// A mutation replayed the correlation id of a recent one.
    DuplicateRequest,
    /// The route exists but this peer's access class cannot reach it.
    NotAuthorized,
    /// The route did not finish inside its declared budget.
    RouteTimeout,
}

impl Fault {
    /// Stable classification stored in the audit trail.
    pub fn class(self) -> &'static str {
        match self {
            Fault::UnsupportedFrame => "protocol_unsupported_frame",
            Fault::FrameTooLarge => "protocol_frame_too_large",
            Fault::TruncatedFrame => "protocol_truncated_frame",
            Fault::MalformedBody => "invalid_json",
            Fault::ExtraFrame => "protocol_extra_frame",
            Fault::InvalidEnvelope => "protocol_invalid_envelope",
            Fault::UnsupportedVersion => "protocol_unsupported_version",
            Fault::UnknownCommand => "unknown_command",
            Fault::InvalidParams => "invalid_params",
            Fault::MissingCredentials => "peer_credentials_missing",
            Fault::CredentialsChanged => "peer_credentials_changed",
            Fault::DescriptorPassing => "peer_descriptor_passing",
            Fault::PeerUnverified => "peer_unverified",
            Fault::ReadTimeout => "request_read_timeout",
            Fault::WriteTimeout => "response_write_timeout",
            Fault::ResponseTooLarge => "response_too_large",
            Fault::TooManyConnections => "too_many_connections",
            Fault::TooManyRequests => "too_many_requests",
            Fault::RouteBusy => "route_busy",
            Fault::DuplicateRequest => "duplicate_request",
            Fault::NotAuthorized => "command_not_authorized",
            Fault::RouteTimeout => "route_timeout",
        }
    }

    /// Stable response category. Transport-specific detail remains in
    /// [`Self::class`] while clients can branch on this smaller set.
    pub fn code(self) -> &'static str {
        match self {
            Fault::MalformedBody => "invalid_json",
            Fault::InvalidEnvelope | Fault::InvalidParams => "invalid_request",
            Fault::UnknownCommand => "unknown_command",
            Fault::NotAuthorized => "not_authorized",
            Fault::TooManyConnections
            | Fault::TooManyRequests
            | Fault::RouteBusy
            | Fault::RouteTimeout => "unavailable",
            _ => "protocol_error",
        }
    }

    /// Text answered to the peer. Static by construction: no field
    /// name, no value, no path, no `io::Error`.
    pub fn message(self) -> &'static str {
        match self {
            Fault::UnsupportedFrame => {
                "clawd speaks broker protocol v1 framing; upgrade the client"
            }
            Fault::FrameTooLarge => "request frame exceeds the broker's maximum size",
            Fault::TruncatedFrame => "request frame ended before its declared length",
            Fault::MalformedBody => "request frame is not valid JSON",
            Fault::ExtraFrame => "clawd serves one request per connection",
            Fault::InvalidEnvelope => "request is not a valid broker envelope",
            Fault::UnsupportedVersion => "clawd speaks broker protocol v1; upgrade the client",
            Fault::UnknownCommand => "unknown clawd command",
            Fault::InvalidParams => "request parameters are not valid for this command",
            Fault::MissingCredentials => "kernel reported no credentials for this message",
            Fault::CredentialsChanged => "peer credentials changed inside one request",
            Fault::DescriptorPassing => {
                "clawd refuses file descriptors; the attached descriptors were closed"
            }
            Fault::PeerUnverified => "peer process could not be verified",
            Fault::ReadTimeout => "request was not delivered inside the read deadline",
            Fault::WriteTimeout => "response was not drained inside the write deadline",
            Fault::ResponseTooLarge => "response exceeds the broker's maximum size",
            Fault::TooManyConnections => "clawd has too many open connections",
            Fault::TooManyRequests => "clawd has too many requests in flight",
            Fault::RouteBusy => "this clawd command has too many requests in flight",
            Fault::DuplicateRequest => "this request id was already served",
            Fault::NotAuthorized => "clawd command requires root",
            Fault::RouteTimeout => "clawd command exceeded its time budget",
        }
    }

    /// Whether the daemon should still try to answer before closing.
    ///
    /// A frame whose header we could not trust is answered too — the
    /// reply is a v1 frame either way, so a confused peer learns why it
    /// was refused instead of seeing an unexplained close.
    pub fn is_reportable(self) -> bool {
        !matches!(self, Fault::TruncatedFrame | Fault::WriteTimeout)
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// One newline-terminated JSON object, sent only to a peer that opened
/// with the pre-v1 newline protocol.
///
/// This is a diagnostic, not a downgrade: it is produced without
/// looking at a single byte of the caller's request, so nothing is
/// parsed, authorized or dispatched. An old `cos` prints an actionable
/// message instead of a parse failure against binary framing.
pub fn legacy_upgrade_notice() -> Vec<u8> {
    let mut body = serde_json::json!({
        "ok": false,
        "error": {
            "code": Fault::UnsupportedFrame.code(),
            "message": Fault::UnsupportedFrame.message(),
        }
    })
    .to_string()
    .into_bytes();
    body.push(b'\n');
    body
}

/// Whether these bytes open the pre-v1 newline protocol.
pub fn looks_like_legacy_request(prefix: &[u8]) -> bool {
    prefix
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{')
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/wire/mod.rs"
    ));
}
