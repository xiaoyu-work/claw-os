//! Discovery and request shapes for `cos-agent-bridge`.
//!
//! The bridge is a sibling process under `desktop/agent/bridge/`.
//! It listens on `127.0.0.1:<port>` and writes the bound port plus a
//! bearer token to a private discovery file under `$XDG_RUNTIME_DIR`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
pub use cos_agent_protocol::{
    BridgeEndpoint, ChatRequest, DonePayload, ErrorEnvelope, HistoryMessage, ModelsResponse,
    SessionSummary, StreamEvent, ToolCallView, ToolResultView,
};
use cos_agent_protocol::{CURRENT_PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER};

/// Maximum age of the endpoint file before we assume the bridge is dead.
/// Today we don't actually check mtime — kept here for the future
/// "bridge appears down" UI banner.
#[allow(dead_code)]
pub const ENDPOINT_FILE_STALE_AFTER: Duration = Duration::from_secs(86_400);

/// Read and validate the private endpoint file the bridge published at boot.
pub fn read_bridge_endpoint() -> Result<BridgeEndpoint> {
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
    let endpoint: BridgeEndpoint = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("reading bridge endpoint {}", path.display()))?,
    )
    .context("decoding bridge endpoint")?;
    if !endpoint.has_valid_version_range() || !endpoint.protocol_version.is_supported() {
        anyhow::bail!(
            "bridge protocol range {}..={} is incompatible with UI version {}",
            endpoint.min_protocol_version.0,
            endpoint.protocol_version.0,
            CURRENT_PROTOCOL_VERSION
        );
    }
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
    Ok(endpoint)
}

/// Resolve a path relative to the bridge into a full URL.
pub fn bridge_url(endpoint: &BridgeEndpoint, path: &str) -> String {
    format!("http://127.0.0.1:{}{path}", endpoint.port)
}

pub async fn ensure_bridge_endpoint() -> Result<BridgeEndpoint> {
    if let Ok(endpoint) = read_bridge_endpoint()
        && bridge_is_healthy(&endpoint).await
    {
        return Ok(endpoint);
    }

    let mut systemctl = tokio::process::Command::new("systemctl");
    systemctl
        .args(["--user", "start", "cos-agent-bridge.service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), systemctl.status()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(endpoint) = read_bridge_endpoint()
            && bridge_is_healthy(&endpoint).await
        {
            return Ok(endpoint);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("cos-agent-bridge did not become ready");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn bridge_is_healthy(endpoint: &BridgeEndpoint) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let response = client
        .get(bridge_url(endpoint, "/api/health"))
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await;
    response.is_ok_and(|response| {
        response.status().is_success() && validate_response_protocol(&response).is_ok()
    })
}

/// `GET /api/sessions` — list persisted conversations newest-first.
pub async fn fetch_sessions(endpoint: BridgeEndpoint) -> Result<Vec<SessionSummary>> {
    let url = bridge_url(&endpoint, "/api/sessions");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building sessions client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response)?;
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
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building history client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response)?;
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
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building session lookup client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response)?;
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
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building models client")?
        .get(&url)
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    validate_response_protocol(&response)?;
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
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("building cancellation client")?
        .post(&url)
        .header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    validate_response_protocol(&response)?;
    if !response.status().is_success() {
        return Err(response_error(response, &url).await);
    }
    Ok(())
}

pub fn versioned_request(
    request: reqwest::RequestBuilder,
    endpoint: &BridgeEndpoint,
) -> reqwest::RequestBuilder {
    request.header(PROTOCOL_VERSION_HEADER, endpoint.protocol_version.0)
}

pub fn validate_response_protocol(response: &reqwest::Response) -> Result<()> {
    let version = response
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        .context("bridge response omitted a valid protocol version")?;
    if version != CURRENT_PROTOCOL_VERSION {
        anyhow::bail!(
            "bridge response protocol version {version} is incompatible with UI version {CURRENT_PROTOCOL_VERSION}"
        );
    }
    Ok(())
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
