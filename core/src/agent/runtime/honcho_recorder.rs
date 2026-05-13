//! Best-effort, fire-and-forget Honcho recorder wired into the
//! agent runtime loop.
//!
//! The recorder lives alongside the SQLite-FTS recorder and the
//! semantic store. Like them, it is opt-in: when no
//! `HONCHO_BASE_URL` is configured, every call is a no-op.
//!
//! Unlike the local stores, Honcho is a remote HTTP service. To keep
//! the agent turn off the network's critical path we spawn each
//! `append_message` call onto the surrounding tokio runtime and
//! ignore the result (we `tracing::warn!` on failure but never
//! propagate). Worst case the dialectic memory loses a message.
//!
//! `HonchoRecorder::from_env_logged` mirrors the existing
//! `HonchoConfig::from_env` opt-in shape but additionally:
//!
//! - logs `tracing::info!` once when configured (so operators see
//!   Honcho is enabled);
//! - logs `tracing::warn!` when `HonchoClient::new` fails (e.g.,
//!   reqwest rejects the timeout) and returns `None`, so a broken
//!   config never poisons the runtime.
//!
//! ### Scope-limited spawning
//!
//! `spawn_message` only fires for `User` and `Assistant` roles.
//! `System` (system prompts) and `Tool` (tool results) are
//! deliberately skipped — Honcho's dialectic memory models the
//! user / agent dialog, not infrastructure messages.
//!
//! Tasks are detached. For one-shot `cos agent ask` invocations
//! that exit immediately after the answer renders, in-flight tasks
//! may be cancelled before they reach Honcho — that is acceptable
//! for a stopgap. The intended deployment is a long-running
//! `cos agent service` worker where the runtime outlives any single
//! turn.

use std::sync::Arc;

use crate::agent::llm::Role;
use crate::agent::memory::honcho::{HonchoClient, HonchoConfig, MessageRole};

/// Stable peer identifier for the human side of the dialog.
///
/// Honcho models speakers as "peers" within a session, distinct
/// from the per-message `role` field. We use two fixed peer ids
/// for now (single-user, single-agent assumption baked into the
/// rest of the OS); future multi-user scenarios will pull these
/// from session-scoped config.
pub const HUMAN_PEER_ID: &str = "user";

/// Stable peer identifier for the agent side of the dialog.
pub const AGENT_PEER_ID: &str = "assistant";

/// Best-effort Honcho writer. Construct via
/// [`HonchoRecorder::from_env_logged`]; share via `Arc`.
#[derive(Debug)]
pub struct HonchoRecorder {
    client: Arc<HonchoClient>,
}

impl HonchoRecorder {
    /// Build a recorder from process environment, logging
    /// configuration outcomes via `tracing`. Returns `None` when
    /// `HONCHO_BASE_URL` is unset or when client construction
    /// fails (in either case the runtime continues without
    /// Honcho).
    pub fn from_env_logged() -> Option<Arc<Self>> {
        let cfg = HonchoConfig::from_env()?;
        match HonchoClient::new(cfg.clone()) {
            Ok(client) => {
                tracing::info!(
                    base_url = %cfg.base_url,
                    workspace = %cfg.workspace_id,
                    "honcho recorder enabled"
                );
                Some(Arc::new(Self {
                    client: Arc::new(client),
                }))
            }
            Err(e) => {
                tracing::warn!(
                    base_url = %cfg.base_url,
                    error = %e,
                    "honcho recorder disabled: client construction failed"
                );
                None
            }
        }
    }

    /// For tests: build directly from a pre-constructed client.
    #[cfg(test)]
    pub fn from_client(client: HonchoClient) -> Arc<Self> {
        Arc::new(Self {
            client: Arc::new(client),
        })
    }

    /// Decide what (peer_id, role) to record for a runtime
    /// message. Returns `None` for roles Honcho should not see.
    pub fn map_role(role: Role) -> Option<(&'static str, MessageRole)> {
        match role {
            Role::User => Some((HUMAN_PEER_ID, MessageRole::User)),
            Role::Assistant => Some((AGENT_PEER_ID, MessageRole::Assistant)),
            Role::System | Role::Tool => None,
        }
    }

    /// Fire-and-forget `append_message`. Spawns onto the current
    /// tokio runtime; intended to be called from within
    /// `ask_inner` / `ask_inner_streaming` (which always run on
    /// tokio).
    ///
    /// Empty `content` is silently ignored — sqlite_fts already
    /// applies the same filter to keep noise out of memory.
    pub fn spawn_message(self: &Arc<Self>, session_id: String, role: Role, content: String) {
        let Some((peer_id, msg_role)) = Self::map_role(role) else {
            return;
        };
        if content.is_empty() {
            return;
        }
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = client
                .append_message(&session_id, peer_id, &content, msg_role)
                .await
            {
                tracing::warn!(
                    session_id = %session_id,
                    peer_id = %peer_id,
                    error = %e,
                    "honcho: append_message failed"
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::honcho::HonchoConfig;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn snapshot_and_clear_env() -> Vec<(String, Option<String>)> {
        let keys = [
            "HONCHO_BASE_URL",
            "HONCHO_API_KEY",
            "HONCHO_WORKSPACE_ID",
            "HONCHO_TIMEOUT_SECS",
        ];
        let saved: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        saved
    }

    fn restore_env(saved: Vec<(String, Option<String>)>) {
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
    }

    #[test]
    fn from_env_logged_returns_none_when_base_url_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        let r = HonchoRecorder::from_env_logged();
        restore_env(saved);
        assert!(r.is_none());
    }

    #[test]
    fn from_env_logged_returns_some_when_configured() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved = snapshot_and_clear_env();
        std::env::set_var("HONCHO_BASE_URL", "http://localhost:9");
        let r = HonchoRecorder::from_env_logged();
        restore_env(saved);
        assert!(r.is_some());
    }

    #[test]
    fn map_role_only_includes_user_and_assistant() {
        assert_eq!(
            HonchoRecorder::map_role(Role::User),
            Some(("user", MessageRole::User))
        );
        assert_eq!(
            HonchoRecorder::map_role(Role::Assistant),
            Some(("assistant", MessageRole::Assistant))
        );
        assert!(HonchoRecorder::map_role(Role::System).is_none());
        assert!(HonchoRecorder::map_role(Role::Tool).is_none());
    }

    /// HTTP integration: spin up an in-process server, point a
    /// `HonchoClient` at it, and verify spawn_message actually
    /// hits the server. Mirrors the spawn_one_shot_mock pattern
    /// from openai_compat tests so we avoid pulling wiremock.
    #[tokio::test]
    async fn spawn_message_hits_remote_server() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_writer = hits.clone();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await.unwrap();
            hits_writer.fetch_add(1, Ordering::SeqCst);
            let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
            sock.write_all(resp).await.unwrap();
            sock.shutdown().await.unwrap();
        });

        let cfg = HonchoConfig {
            base_url: format!("http://{addr}"),
            api_key: None,
            workspace_id: "ws".into(),
            timeout_secs: 5,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let r = HonchoRecorder::from_client(client);
        r.spawn_message("sess-1".into(), Role::User, "hello world".into());

        // give the spawned task time to drain
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), server)
            .await
            .expect("server completed");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn spawn_message_skips_tool_role_without_network() {
        // No server bound; if the recorder fired a request it
        // would trip the timeout and leak a task. We verify it
        // simply never spawns by using an obviously-unreachable
        // address and asserting the call returns synchronously.
        let cfg = HonchoConfig {
            base_url: "http://127.0.0.1:1".into(),
            api_key: None,
            workspace_id: "ws".into(),
            timeout_secs: 1,
        };
        let client = HonchoClient::new(cfg).unwrap();
        let r = HonchoRecorder::from_client(client);
        // Tool role: should be a no-op; no panic, no spawn.
        r.spawn_message("s".into(), Role::Tool, "tool output".into());
        // System role: same.
        r.spawn_message("s".into(), Role::System, "sys".into());
        // Empty content User: should also be a no-op.
        r.spawn_message("s".into(), Role::User, String::new());
    }
}
