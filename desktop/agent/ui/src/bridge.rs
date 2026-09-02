//! Discovery and request shapes for `cos-agent-bridge`.
//!
//! The bridge is a sibling process under `desktop/agent/bridge/`.
//! It listens on `127.0.0.1:<port>` and writes the bound port plus a
//! bearer token to a private discovery file under `$XDG_RUNTIME_DIR`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
pub use cos_agent_protocol::{
    BridgeEndpoint, ChatRequest, ErrorEnvelope, HistoryMessage, ModelsResponse, SessionSummary,
    StreamEvent, ToolCallView, ToolResultView,
};
use cos_agent_protocol::{PROTOCOL_VERSION_HEADER, ProtocolMetadata, ProtocolVersion};
use reqwest::header::HeaderMap;
use serde::Deserialize;

/// Maximum age of the endpoint file before we assume the bridge is dead.
/// Today we don't actually check mtime — kept here for the future
/// "bridge appears down" UI banner.
#[allow(dead_code)]
pub const ENDPOINT_FILE_STALE_AFTER: Duration = Duration::from_secs(86_400);
const SERVICE_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const BRIDGE_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const BRIDGE_SERVICE: &str = "cos-agent-bridge.service";
static UPGRADE_RESTART_ATTEMPTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
enum DiscoveryState {
    Ready(BridgeEndpoint),
    UpgradeRequired,
}

#[derive(Debug, Deserialize)]
struct LegacyBridgeEndpoint {
    port: u16,
    token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceAction {
    Start,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthStatus {
    Healthy,
    NegotiationFailed,
    Unavailable,
}

fn read_bridge_discovery() -> Result<DiscoveryState> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .map(|dir| dir.join("cos-agent-bridge"))
        .context("XDG_RUNTIME_DIR is required for bridge discovery")?;
    if !dir.is_absolute() {
        anyhow::bail!("bridge runtime directory must be absolute");
    }
    let path = dir.join("endpoint.json");
    let dir_metadata = std::fs::symlink_metadata(&dir)
        .with_context(|| format!("inspecting bridge runtime directory {}", dir.display()))?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        anyhow::bail!(
            "bridge runtime path is not a real directory: {}",
            dir.display()
        );
    }
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("inspecting bridge endpoint {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("bridge endpoint is not a regular file: {}", path.display());
    }
    if metadata.len() == 0 || metadata.len() > 4096 {
        anyhow::bail!("bridge endpoint has an invalid size");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if dir_metadata.mode() & 0o077 != 0 || metadata.mode() & 0o077 != 0 {
            anyhow::bail!(
                "bridge discovery state {} is accessible by another user",
                path.display()
            );
        }
        #[cfg(target_os = "linux")]
        {
            let current_uid = std::fs::metadata("/proc/self")
                .context("inspecting Agent UI process identity")?
                .uid();
            if dir_metadata.uid() != current_uid || metadata.uid() != current_uid {
                anyhow::bail!("bridge discovery state belongs to another user");
            }
        }
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading bridge endpoint {}", path.display()))?;
    let state = decode_bridge_discovery(&bytes).context("decoding bridge endpoint")?;
    let DiscoveryState::Ready(endpoint) = state else {
        return Ok(state);
    };
    if endpoint.port == 0
        || endpoint.token.len() < 32
        || endpoint.token.len() > 256
        || !endpoint
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("bridge endpoint contains invalid credentials");
    }
    Ok(DiscoveryState::Ready(endpoint))
}

fn decode_bridge_discovery(bytes: &[u8]) -> Result<DiscoveryState> {
    if let Ok(endpoint) = serde_json::from_slice::<BridgeEndpoint>(bytes) {
        return if endpoint.has_valid_version_range()
            && endpoint.negotiate(ProtocolMetadata::CURRENT).is_some()
        {
            Ok(DiscoveryState::Ready(endpoint))
        } else {
            Ok(DiscoveryState::UpgradeRequired)
        };
    }
    let legacy: LegacyBridgeEndpoint = serde_json::from_slice(bytes)?;
    let _ = (legacy.port, legacy.token);
    Ok(DiscoveryState::UpgradeRequired)
}

/// Resolve a path relative to the bridge into a full URL.
pub fn bridge_url(endpoint: &BridgeEndpoint, path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", endpoint.port)
}

pub async fn ensure_bridge_endpoint() -> Result<BridgeEndpoint> {
    let action = match read_bridge_discovery() {
        Ok(DiscoveryState::Ready(endpoint)) => {
            let health = bridge_health(&endpoint).await;
            if service_action(&DiscoveryState::Ready(endpoint.clone()), health).is_none() {
                return Ok(endpoint);
            }
            service_action(&DiscoveryState::Ready(endpoint), health).unwrap_or(ServiceAction::Start)
        }
        Ok(state @ DiscoveryState::UpgradeRequired) => {
            service_action(&state, HealthStatus::NegotiationFailed)
                .unwrap_or(ServiceAction::Restart)
        }
        Err(_) => ServiceAction::Start,
    };
    if action == ServiceAction::Restart && !claim_upgrade_restart(&UPGRADE_RESTART_ATTEMPTED) {
        anyhow::bail!("bridge protocol upgrade restart was already attempted");
    }
    control_bridge_service(action).await;

    // Service control runs once. Polling never invokes another restart, which
    // keeps stale discovery from causing an upgrade loop.
    let deadline = tokio::time::Instant::now() + BRIDGE_STARTUP_TIMEOUT;
    loop {
        if let Ok(DiscoveryState::Ready(endpoint)) = read_bridge_discovery()
            && bridge_health(&endpoint).await == HealthStatus::Healthy
        {
            return Ok(endpoint);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("cos-agent-bridge did not become ready");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn service_action(discovery: &DiscoveryState, health: HealthStatus) -> Option<ServiceAction> {
    match discovery {
        DiscoveryState::Ready(_) if health == HealthStatus::Healthy => None,
        DiscoveryState::Ready(_) if health == HealthStatus::Unavailable => {
            Some(ServiceAction::Start)
        }
        DiscoveryState::Ready(_) | DiscoveryState::UpgradeRequired => Some(ServiceAction::Restart),
    }
}

fn claim_upgrade_restart(attempted: &AtomicBool) -> bool {
    attempted
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

async fn control_bridge_service(action: ServiceAction) {
    let succeeded = run_systemctl(action).await;
    if action == ServiceAction::Restart && !succeeded {
        // A failed restart may mean the unit is installed but inactive, or
        // systemd did not know it yet. Preserve the previous start fallback;
        // a manually launched bridge remains untouched if no unit exists.
        let _ = run_systemctl(ServiceAction::Start).await;
    }
}

async fn run_systemctl(action: ServiceAction) -> bool {
    let verb = match action {
        ServiceAction::Start => "start",
        ServiceAction::Restart => "restart",
    };
    let mut systemctl = tokio::process::Command::new("systemctl");
    systemctl
        .args(["--user", verb, BRIDGE_SERVICE])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    tokio::time::timeout(SERVICE_CONTROL_TIMEOUT, systemctl.status())
        .await
        .is_ok_and(|status| status.is_ok_and(|status| status.success()))
}

async fn bridge_health(endpoint: &BridgeEndpoint) -> HealthStatus {
    let Ok(selected) = selected_protocol_version(endpoint) else {
        return HealthStatus::NegotiationFailed;
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return HealthStatus::Unavailable;
    };
    let Ok(response) = client
        .get(bridge_url(endpoint, "/api/health"))
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
    else {
        return HealthStatus::Unavailable;
    };
    if health_response_is_compatible(&response, selected) {
        HealthStatus::Healthy
    } else if validate_response_protocol_headers(response.headers(), selected).is_err() {
        HealthStatus::NegotiationFailed
    } else {
        HealthStatus::Unavailable
    }
}

/// `GET /api/sessions` — list persisted conversations newest-first.
pub async fn fetch_sessions(endpoint: BridgeEndpoint) -> Result<Vec<SessionSummary>> {
    let url = bridge_url(&endpoint, "/api/sessions");
    let selected = selected_protocol_version(&endpoint)?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building sessions client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response, selected)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    let sessions = response
        .json::<Vec<SessionSummary>>()
        .await
        .context("decoding /api/sessions")?;
    Ok(sessions)
}

/// `GET /api/sessions/:id/history` — full transcript for `session_id`.
pub async fn fetch_history(
    endpoint: BridgeEndpoint,
    session_id: &str,
) -> Result<Vec<HistoryMessage>> {
    let path = format!("/api/sessions/{session_id}/history");
    let url = bridge_url(&endpoint, &path);
    let selected = selected_protocol_version(&endpoint)?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building history client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response, selected)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    let envelope = response
        .json::<cos_agent_protocol::HistoryResponse>()
        .await
        .context("decoding history envelope")?;
    Ok(envelope.messages)
}

pub async fn session_exists(endpoint: BridgeEndpoint, session_id: &str) -> Result<bool> {
    let url = bridge_url(&endpoint, &format!("/api/sessions/{session_id}"));
    let selected = selected_protocol_version(&endpoint)?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building session lookup client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response, selected)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    Ok(true)
}

pub async fn fetch_models(endpoint: BridgeEndpoint) -> Result<ModelsResponse> {
    let url = bridge_url(&endpoint, "/api/models");
    let selected = selected_protocol_version(&endpoint)?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building models client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response, selected)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    response
        .json::<ModelsResponse>()
        .await
        .context("decoding /api/models")
}

pub async fn cancel_task(endpoint: BridgeEndpoint, task_id: &str) -> Result<()> {
    let url = bridge_url(&endpoint, &format!("/api/chat/{task_id}/cancel"));
    let selected = selected_protocol_version(&endpoint)?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building cancellation client")?
        .post(&url)
        .header(PROTOCOL_VERSION_HEADER, selected.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    validate_response_protocol(&response, selected)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    Ok(())
}

pub fn versioned_request(
    request: reqwest::RequestBuilder,
    endpoint: &BridgeEndpoint,
) -> Result<(reqwest::RequestBuilder, ProtocolVersion)> {
    let selected = selected_protocol_version(endpoint)?;
    Ok((
        request.header(PROTOCOL_VERSION_HEADER, selected.0),
        selected,
    ))
}

pub fn selected_protocol_version(endpoint: &BridgeEndpoint) -> Result<ProtocolVersion> {
    endpoint
        .negotiate(ProtocolMetadata::CURRENT)
        .with_context(|| {
            format!(
                "bridge protocol range {}..={} has no overlap with UI range {}..={}",
                endpoint.min_protocol_version.0,
                endpoint.protocol_version.0,
                ProtocolMetadata::CURRENT.min_protocol_version.0,
                ProtocolMetadata::CURRENT.protocol_version.0,
            )
        })
}

pub fn validate_response_protocol(
    response: &reqwest::Response,
    selected: ProtocolVersion,
) -> Result<()> {
    validate_response_protocol_headers(response.headers(), selected)
}

fn validate_response_protocol_headers(
    headers: &HeaderMap,
    selected: ProtocolVersion,
) -> Result<()> {
    let version = headers
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        .context("bridge response omitted a valid protocol version")?;
    if version != selected.0 {
        anyhow::bail!(
            "bridge echoed protocol version {version}, expected negotiated version {}",
            selected.0
        );
    }
    Ok(())
}

fn health_response_is_compatible(response: &reqwest::Response, selected: ProtocolVersion) -> bool {
    response.status().is_success()
        && validate_response_protocol_headers(response.headers(), selected).is_ok()
}

pub async fn response_error(response: reqwest::Response, url: &str) -> anyhow::Error {
    let status = response.status();
    match response.json::<ErrorEnvelope>().await {
        Ok(envelope) => {
            let detail = envelope
                .hint
                .filter(|hint| !hint.trim().is_empty())
                .map(|hint| format!("{} - {hint}", envelope.error))
                .unwrap_or(envelope.error);
            anyhow!("bridge {url} responded {status}: {detail}")
        }

        Err(error) => {
            anyhow!("bridge {url} responded {status} with an invalid error envelope: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/bridge.rs"));
}
