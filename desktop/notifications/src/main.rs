mod app;
mod config;
mod glass;
mod mcp;
mod subscriptions;

use config::APP_ID;
use tracing::{info, metadata::LevelFilter};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::config::VERSION;

fn main() -> anyhow::Result<()> {
    // MCP server mode — kernel agent spawns us with this env var
    // set when bringing up the App's session. We can't initialise
    // libcosmic in this mode (we'd open a window we don't want).
    if std::env::var("COS_MCP_SERVER").as_deref() == Ok("1") {
        return mcp::run();
    }

    color_backtrace::install();
    let trace = tracing_subscriber::registry();

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();
    #[cfg(feature = "systemd")]
    if let Ok(journald) = tracing_journald::layer() {
        trace
            .with(journald)
            .with(fmt::layer())
            .with(env_filter)
            .try_init()?;
    } else {
        trace.with(fmt::layer()).with(env_filter).try_init()?;
        tracing::warn!("Failed to connect to journald")
    }

    #[cfg(not(feature = "systemd"))]
    trace.with(fmt::layer()).with(env_filter).try_init()?;

    info!("cosmic-notifications ({})", APP_ID);
    info!("Version: {} ({})", VERSION, config::profile());

    app::run()?;
    Ok(())
}
