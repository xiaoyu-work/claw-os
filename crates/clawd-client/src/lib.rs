//! Typed, bounded client for the Claw OS `clawd` broker.
//!
//! The crate is deliberately unprivileged: it discovers one socket, opens one
//! connection for one request, and validates the correlated v1 response. It
//! owns no desktop state and imports no broker business logic.

mod client;
mod discovery;
mod error;
mod protocol;

pub use client::{Client, ClientConfig};
pub use discovery::{
    discover_socket, COMPAT_SOCKET_ENV, DEFAULT_SOCKET_PATH, RUNTIME_ENV, SOCKET_ENV,
};
pub use error::{ClientError, Error, ErrorCode, RemoteError};
pub use protocol::{
    Command, Request, RequestId, Response, HEADER_BYTES, KIND_REQUEST, KIND_RESPONSE, MAGIC,
    MAX_REQUEST_BYTES, MAX_REQUEST_ID_BYTES, MAX_RESPONSE_BYTES, PROTOCOL_VERSION,
};
