//! Claw-owned implementation of the Codex TUI's remote app-server protocol.
//!
//! Only presentation and connection state live here. Tasks, conversations,
//! capabilities and approval decisions remain owned by the Claw broker.

mod approvals;
mod backend;
mod bootstrap;
mod broker;
mod events;
mod history;
mod input;
mod models;
mod pagination;
mod protocol;
mod server;
mod threads;
mod transport;

use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::watch;

use crate::config::CosConfig;

/// Execution defaults supplied by the process that owns the private listener.
#[derive(Clone, Debug)]
pub struct Options {
    pub use_memory: bool,
    pub max_turns: Option<u32>,
    pub session_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            use_memory: true,
            max_turns: None,
            session_id: None,
        }
    }
}

/// Resolve an owner-visible Claw session before handing its UUID presentation
/// identity to the upstream TUI's `resume` command.
pub async fn initial_thread_id(session_id: &str) -> Result<String, String> {
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getuid() } == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.to_string());
    }
    let canonical = protocol::canonical_session_id(session_id);
    let alias = uuid::Uuid::parse_str(session_id).ok();
    if canonical.is_none() && alias.is_none() {
        return Err("expected a canonical Claw session id or presentation UUID".to_string());
    }
    let conversation = broker::read_initial_conversation(session_id)
        .await
        .map_err(|error| error.message)?;
    let conversation = history::conversation(conversation).map_err(|error| error.message)?;
    let id = protocol::backend_string(&conversation, "id").map_err(|error| error.message)?;
    let frontend_id = history::frontend_id(&conversation).map_err(|error| error.message)?;
    if canonical
        .as_ref()
        .is_some_and(|canonical| canonical.as_str() != id)
        || alias.is_some_and(|alias| uuid::Uuid::parse_str(frontend_id).ok() != Some(alias))
    {
        return Err("Claw returned another conversation".to_string());
    }
    Ok(frontend_id.to_string())
}

/// Serve the real upstream TUI over an already securely bound Unix listener.
///
/// This does not create a listener, launch a TUI, call a model, or adopt the
/// caller's sandbox/approval settings as authority. Disconnecting a client
/// stops its subscription, not its durable Claw task.
pub async fn serve(
    listener: UnixListener,
    config: Arc<CosConfig>,
    options: Options,
    shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let uid = unsafe { libc::geteuid() };
    if uid == 0 || unsafe { libc::getuid() } == 0 {
        return Err(crate::agentd::spawn::ROOT_OWNER_REFUSAL.to_string());
    }
    if options.max_turns == Some(0) {
        return Err("max_turns must be greater than zero".to_string());
    }
    let initial_session = options.session_id.clone();
    let initialize = async move {
        if let Some(id) = initial_session {
            initial_thread_id(&id).await?;
        }
        broker::BrokerBackend::new(config, uid).await
    };
    let Some(backend) = initialize_or_shutdown(initialize, shutdown.clone()).await? else {
        return Ok(());
    };
    let backend = Arc::new(backend);
    let service = Arc::new(server::Service::new(backend, options));
    transport::serve(listener, service, uid, shutdown).await
}

async fn initialize_or_shutdown<T>(
    initialize: impl std::future::Future<Output = Result<T, String>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<Option<T>, String> {
    let stopped = async {
        loop {
            if *shutdown.borrow() {
                break;
            }
            if shutdown.changed().await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        biased;
        _ = stopped => Ok(None),
        result = initialize => result.map(Some),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/mod.rs"
    ));
}

#[cfg(test)]
mod bound_history_tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tui_backend/bound_history.rs"
    ));
}
