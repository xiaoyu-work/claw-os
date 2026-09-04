//! The in-repo broker client.
//!
//! One connection carries exactly one request and one response, then
//! closes. That is the whole concurrency model: nothing is pipelined,
//! so no response can be attributed to the wrong request, and a frame
//! the daemon has already answered cannot be followed by a second one
//! on the same authenticated connection.
//!
//! The response is still checked against the request: same protocol
//! version, same correlation id. A daemon that answered something else
//! is a bug or an impostor, and either way the caller refuses the
//! answer instead of acting on it.

use std::path::Path;

use tokio::net::UnixStream;

use super::protocol::{encode_request, Request, Response};
use super::transport::frame;
use super::wire::{Fault, MAX_RESPONSE_BYTES, PROTOCOL_VERSION};

/// Whether a failed exchange can prove that the daemon never received it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchState {
    NotDispatched,
    PossiblyDispatched,
}

/// A broker transport failure with enough phase information for mutation callers.
#[derive(Debug)]
pub struct ClientError {
    state: DispatchState,
    message: String,
}

impl ClientError {
    fn before_dispatch(message: impl Into<String>) -> Self {
        Self {
            state: DispatchState::NotDispatched,
            message: message.into(),
        }
    }

    fn after_dispatch(message: impl Into<String>) -> Self {
        Self {
            state: DispatchState::PossiblyDispatched,
            message: message.into(),
        }
    }

    pub fn may_have_dispatched(&self) -> bool {
        self.state == DispatchState::PossiblyDispatched
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl From<ClientError> for String {
    fn from(error: ClientError) -> Self {
        error.message
    }
}

fn check(request: &Request, response: Response) -> Result<Response, String> {
    if response.v != PROTOCOL_VERSION {
        return Err(format!(
            "clawd answered broker protocol v{}; this client speaks v{PROTOCOL_VERSION}",
            response.v
        ));
    }
    if response.id != request.id {
        return Err("clawd response did not correlate with the request".to_string());
    }
    Ok(response)
}

fn decode(body: &[u8]) -> Result<Response, String> {
    serde_json::from_slice::<Response>(body)
        .map_err(|_| "clawd response is not a valid broker envelope".to_string())
}

fn transport_error(fault: Fault) -> String {
    format!("clawd transport refused the exchange: {}", fault.message())
}

#[cfg(unix)]
pub fn request_blocking(
    socket_path: impl AsRef<Path>,
    request: Request,
) -> Result<Response, ClientError> {
    use std::os::unix::net::UnixStream as StdUnixStream;

    let mut stream = StdUnixStream::connect(socket_path.as_ref()).map_err(|err| {
        ClientError::before_dispatch(format!("failed to connect to clawd socket: {err}"))
    })?;
    let body =
        encode_request(&request).map_err(|err| ClientError::before_dispatch(err.to_string()))?;
    frame::write_request_blocking(&mut stream, &body).map_err(|err| {
        ClientError::after_dispatch(format!("failed to write clawd request: {err}"))
    })?;
    let response = frame::read_response_blocking(&mut stream, MAX_RESPONSE_BYTES)
        .map_err(transport_error)
        .map_err(ClientError::after_dispatch)?;
    let response = decode(&response).map_err(ClientError::after_dispatch)?;
    check(&request, response).map_err(ClientError::after_dispatch)
}

#[cfg(not(unix))]
pub fn request_blocking(
    _socket_path: impl AsRef<Path>,
    _request: Request,
) -> Result<Response, ClientError> {
    Err(ClientError::before_dispatch(
        "blocking clawd requests require Unix",
    ))
}

pub async fn request(
    socket_path: impl AsRef<Path>,
    request: Request,
) -> Result<Response, ClientError> {
    let mut stream = UnixStream::connect(socket_path.as_ref())
        .await
        .map_err(|err| {
            ClientError::before_dispatch(format!("failed to connect to clawd socket: {err}"))
        })?;
    let body =
        encode_request(&request).map_err(|err| ClientError::before_dispatch(err.to_string()))?;
    frame::write_request_async(&mut stream, &body)
        .await
        .map_err(|err| {
            ClientError::after_dispatch(format!("failed to write clawd request: {err}"))
        })?;
    let response = frame::read_response_async(&mut stream, MAX_RESPONSE_BYTES)
        .await
        .map_err(transport_error)
        .map_err(ClientError::after_dispatch)?;
    let response = decode(&response).map_err(ClientError::after_dispatch)?;
    check(&request, response).map_err(ClientError::after_dispatch)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/client.rs"
    ));
}
