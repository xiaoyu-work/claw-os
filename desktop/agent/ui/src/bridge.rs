//! Discovery and request shapes for `cos-agent-bridge`.
//!
//! The bridge is a sibling process under `desktop/agent/bridge/`.
//! It listens on `127.0.0.1:<port>` and writes the bound port to
//! `$XDG_RUNTIME_DIR/cos-agent-bridge.port` so other clients (this
//! UI, the future global hotkey overlay, the legacy React app) can
//! find it without command-line wiring.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Maximum age of the port file before we assume the bridge is dead.
/// Today we don't actually check mtime — kept here for the future
/// "bridge appears down" UI banner.
#[allow(dead_code)]
pub const PORT_FILE_STALE_AFTER: Duration = Duration::from_secs(86_400);

/// Wire format for `POST /api/chat`. Matches the React shape.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One SSE event re-decoded out of the bridge wire format.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Incremental text from the agent's stderr token stream.
    Delta(String),
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

#[derive(Debug, Deserialize)]
pub struct ErrorPayload {
    #[serde(default)]
    pub message: String,
}

/// Read the port file the bridge dropped at boot.
pub fn read_bridge_port() -> Result<u16> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("cos-agent-bridge.port");
    let s = std::fs::read_to_string(&path)
        .with_context(|| format!("reading bridge port file at {}", path.display()))?;
    let port: u16 = s.trim().parse().with_context(|| "parsing bridge port")?;
    Ok(port)
}

/// Resolve a path relative to the bridge into a full URL.
pub fn bridge_url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}
