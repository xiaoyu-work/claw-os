//! Shared application state.
//!
//! The bridge holds three things:
//! - the configured HTTP port (env override or random)
//! - the path on disk where the exported web SPA lives
//! - the path to the `cos` binary that we subprocess for chat turns
//!
//! Everything else (sessions, models, credentials) is owned by
//! `cos agent` itself and reached through subprocess calls.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct AppState {
    pub port: u16,
    pub web_root: PathBuf,
    pub cos_bin: PathBuf,
}

impl AppState {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = std::env::var("COS_AGENT_BRIDGE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let web_root = std::env::var("COS_AGENT_WEB_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/usr/share/cos-agent/web"));
        let cos_bin = std::env::var("COS_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/cos"));
        Ok(Self {
            port,
            web_root,
            cos_bin,
        })
    }
}

/// Write the bound port to `$XDG_RUNTIME_DIR/cos-agent-bridge.port`
/// so the overlay + cos-browser launcher can discover the dynamic
/// port without scanning.
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
