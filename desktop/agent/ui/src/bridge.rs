//! Discovery and request shapes for `cos-agent-bridge`.
//!
//! The bridge is a sibling process under `desktop/agent/bridge/`.
//! It listens on `127.0.0.1:<port>` and writes the bound port plus a
//! bearer token to a private discovery file under `$XDG_RUNTIME_DIR`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Maximum age of the endpoint file before we assume the bridge is dead.
/// Today we don't actually check mtime — kept here for the future
/// "bridge appears down" UI banner.
#[allow(dead_code)]
pub const ENDPOINT_FILE_STALE_AFTER: Duration = Duration::from_secs(86_400);

/// Wire format for `POST /api/chat`. Matches the React shape.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<String>,
}

/// One SSE event re-decoded out of the bridge wire format.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    TaskStarted {
        task_id: String,
        session_id: Option<String>,
    },
    /// Incremental text from the agent's stderr token stream.
    Delta(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        delta: String,
    },
    ToolUse(ToolCallView),
    ToolStart {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult(ToolResultView),
    Warning(String),
    TurnDone(serde_json::Value),
    /// Terminal envelope (answer, turns, usage, …). Sent before EOS.
    Done(serde_json::Value),
    /// Bridge-side or subprocess error. Stream still terminates on EOS.
    Error(String),
}

/// Payload variants the bridge's `delta` / `done` / `error` events carry.
#[derive(Debug, Deserialize)]
pub struct DeltaPayload {
    #[serde(default)]
    pub text: String,
}

/// Sidebar entry shape returned by `GET /api/sessions`. Mirrors the
/// memory-DB session row (id is a stable clawd session_id, not a job
/// id) so a click can resume the conversation via
/// `task.submit { session_id }`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub last_ts_ms: Option<i64>,
    #[serde(default)]
    pub message_count: i64,
}

/// One pre-parsed message row from `GET /api/sessions/:id/history`.
/// Tool calls and tool results are exposed alongside the plain text so
/// the UI can render proper tool cards instead of showing raw
/// `[tool_use:NAME] {…}` markers.
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallView>,
    #[serde(default)]
    pub tool_results: Vec<ToolResultView>,
    #[serde(default)]
    pub ts_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallView {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: serde_json::Value,
    #[serde(default)]
    pub partial_json: String,
    #[serde(default)]
    pub in_progress: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolResultView {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryEnvelope {
    #[serde(default)]
    messages: Vec<HistoryMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeEndpoint {
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSummary {
    pub id: String,
    pub provider: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelsResponse {
    pub ready: bool,
    pub provider: String,
    pub model: String,
    pub label: String,
    #[serde(default)]
    pub models: Vec<ModelSummary>,
}

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
    client
        .get(bridge_url(endpoint, "/api/health"))
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// `GET /api/sessions` — list persisted conversations newest-first.
pub async fn fetch_sessions(endpoint: BridgeEndpoint) -> Result<Vec<SessionSummary>> {
    let url = bridge_url(&endpoint, "/api/sessions");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building sessions client")?
        .get(&url)
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("bridge {url} responded {}", response.status());
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
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("bridge {url} responded {}", response.status());
    }
    let envelope = response
        .json::<HistoryEnvelope>()
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
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success() {
        anyhow::bail!("bridge {url} responded {}", response.status());
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
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("bridge {url} responded {}", response.status());
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
        .bearer_auth(&endpoint.token)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("bridge {url} responded {status}: {body}");
    }
    Ok(())
}
