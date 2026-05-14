//! Shared application state.
//!
//! The bridge holds two things:
//! - the configured HTTP port (env override or random)
//! - the path to the `cos` binary that we subprocess for chat turns
//!
//! Everything else (sessions, models, credentials) is owned by
//! `cos agent` itself and reached through subprocess calls. The
//! bridge no longer serves a static SPA — the React frontend was
//! retired in favour of the native libcosmic UI (`cos-agent-ui`),
//! which calls only the `/api/*` JSON+SSE endpoints.

use std::path::PathBuf;

use anyhow::Context;

#[derive(Clone, Debug)]
pub struct AppState {
    pub port: u16,
    pub cos_bin: PathBuf,
}

impl AppState {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = std::env::var("COS_AGENT_BRIDGE_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let cos_bin = std::env::var("COS_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/cos"));
        Ok(Self { port, cos_bin })
    }
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
