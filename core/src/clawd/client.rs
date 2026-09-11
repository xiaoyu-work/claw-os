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
use std::sync::{Arc, OnceLock};

use tokio::net::UnixStream;

use super::protocol::{encode_request, Request, Response};
use super::transport::frame;
use super::wire::{Fault, MAX_RESPONSE_BYTES, PROTOCOL_VERSION};

pub(crate) trait BrokerGateway: Send + Sync {
    fn request(&self, request: Request, invocation_id: Option<String>) -> Result<Response, String>;
    fn prepare_app(
        &self,
        invocation: crate::operations::invocation::AppInvocation,
    ) -> Result<crate::operations::invocation::PreparedInvocation, String>;
    fn finish_app(&self, invocation_id: &str) -> Result<(), String>;
    fn check_app(&self, session_id: &str, package_digest: &str) -> Result<(), String>;
}

static GATEWAY: OnceLock<Arc<dyn BrokerGateway>> = OnceLock::new();

pub(crate) struct HostedInvocation {
    gateway: Arc<dyn BrokerGateway>,
    pub(crate) prepared: crate::operations::invocation::PreparedInvocation,
}

impl Drop for HostedInvocation {
    fn drop(&mut self) {
        if let Err(error) = self.gateway.finish_app(&self.prepared.id) {
            tracing::error!(error = %error, "could not retire hosted App invocation");
        }
    }
}

pub(crate) fn prepare_app(
    invocation: crate::operations::invocation::AppInvocation,
) -> Result<Option<HostedInvocation>, String> {
    let Some(gateway) = GATEWAY.get().cloned() else {
        return Ok(None);
    };
    let prepared = gateway.prepare_app(invocation)?;
    Ok(Some(HostedInvocation { gateway, prepared }))
}

pub(crate) fn install_gateway(gateway: Arc<dyn BrokerGateway>) -> Result<(), String> {
    GATEWAY
        .set(gateway)
        .map_err(|_| "a broker gateway is already installed in this process".to_string())
}

pub(crate) fn has_gateway() -> bool {
    GATEWAY.get().is_some()
}

pub(crate) fn check_hosted_app(
    session_id: &str,
    package_digest: &str,
) -> Option<Result<(), String>> {
    GATEWAY
        .get()
        .map(|gateway| gateway.check_app(session_id, package_digest))
}

pub(crate) fn request_app_registration(
    request: Request,
    invocation_id: &str,
) -> Result<Response, String> {
    let gateway = GATEWAY
        .get()
        .ok_or_else(|| "controlled App registration has no task gateway".to_string())?;
    let response = gateway.request(request.clone(), Some(invocation_id.to_string()))?;
    check(&request, response)
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
) -> Result<Response, String> {
    use std::os::unix::net::UnixStream as StdUnixStream;

    if let Some(gateway) = GATEWAY.get() {
        return check(&request, gateway.request(request.clone(), None)?);
    }
    let mut stream = StdUnixStream::connect(socket_path.as_ref())
        .map_err(|err| format!("failed to connect to clawd socket: {err}"))?;
    let body = encode_request(&request).map_err(|err| err.to_string())?;
    frame::write_request_blocking(&mut stream, &body)
        .map_err(|err| format!("failed to write clawd request: {err}"))?;
    let response =
        frame::read_response_blocking(&mut stream, MAX_RESPONSE_BYTES).map_err(transport_error)?;
    check(&request, decode(&response)?)
}

#[cfg(not(unix))]
pub fn request_blocking(
    _socket_path: impl AsRef<Path>,
    _request: Request,
) -> Result<Response, String> {
    Err("blocking clawd requests require Unix".to_string())
}

pub async fn request(socket_path: impl AsRef<Path>, request: Request) -> Result<Response, String> {
    if let Some(gateway) = GATEWAY.get().cloned() {
        let forwarded = request.clone();
        let response = tokio::task::spawn_blocking(move || gateway.request(forwarded, None))
            .await
            .map_err(|error| format!("broker gateway failed: {error}"))??;
        return check(&request, response);
    }
    let mut stream = UnixStream::connect(socket_path.as_ref())
        .await
        .map_err(|err| format!("failed to connect to clawd socket: {err}"))?;
    let body = encode_request(&request).map_err(|err| err.to_string())?;
    frame::write_request_async(&mut stream, &body)
        .await
        .map_err(|err| format!("failed to write clawd request: {err}"))?;
    let response = frame::read_response_async(&mut stream, MAX_RESPONSE_BYTES)
        .await
        .map_err(transport_error)?;
    check(&request, decode(&response)?)
}
