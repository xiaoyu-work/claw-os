//! `cos-agent-overlay` — quick-summon layer-shell chat overlay.
//!
//! Phase 8 scaffold: discovers the bridge port and prints a tiny
//! status line. The real iced/libcosmic layer-shell window with
//! a slim composer + last-three-messages view lands in the
//! `overlay-quick-chat` todo.
//!
//! Lifecycle:
//! - Activated by the `Super+A` global keybind (wired in
//!   `global-keybinds` todo).
//! - Single-instance: a UDS in `$XDG_RUNTIME_DIR/cos-agent-overlay.sock`
//!   lets the second invocation say "show" / "hide" to the first.
//! - Talks to `cos-agent-bridge` over `/api/chat` SSE.

use std::path::PathBuf;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("cos_agent_overlay=info")),
        )
        .init();

    let port = read_bridge_port().context("locating cos-agent-bridge port")?;
    let url = format!("http://127.0.0.1:{port}/api/health");
    tracing::info!(%url, "bridge discovered; layer-shell window will land in `overlay-quick-chat` todo");
    Ok(())
}

fn read_bridge_port() -> anyhow::Result<u16> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("cos-agent-bridge.port");
    let s = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(s.trim().parse()?)
}
