//! Shared application state.
//!
//! The bridge holds two things:
//! - the configured HTTP port (env override or random)
//! - the `clawd` Unix socket path used for chat turns
//!
//! Everything else (sessions, models, credentials) is owned by `clawd`. The
//! bridge no longer serves a static SPA — the React frontend was
//! retired in favour of the native libcosmic UI (`cos-agent-ui`),
//! which calls only the `/api/*` JSON+SSE endpoints.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct AppState {
    pub port: u16,
    pub clawd_socket: PathBuf,
}

impl AppState {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = std::env::var("COS_AGENT_BRIDGE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let clawd_socket = clawd_socket_path();
        Ok(Self { port, clawd_socket })
    }
}

fn clawd_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("CLAWD_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("COS_RUNTIME_DIR") {
        return PathBuf::from(path).join("clawd.sock");
    }
    PathBuf::from("/run/cos/clawd.sock")
}

/// Write the bound port to `$XDG_RUNTIME_DIR/cos-agent-bridge.port`
/// so the native UI (`cos-agent-ui`) can discover the dynamic port
/// without scanning.
pub fn write_port_file(port: u16) -> anyhow::Result<()> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("cos-agent-bridge.port");
    std::fs::write(&path, port.to_string())
        .with_context(|| format!("writing port file {}", path.display()))?;
    Ok(())
}
