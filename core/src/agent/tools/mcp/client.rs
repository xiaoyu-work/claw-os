//! Outbound MCP client — drive a remote MCP server (e.g. a database
//! adapter, a filesystem provider) over a [`Transport`].
//!
//! Async pattern: `send_request().await` writes the request frame
//! and then awaits the matching response by id. We multiplex on a
//! single transport via a background reader task that demuxes
//! responses to per-request oneshot channels.
//!
//! ## Timeouts
//!
//! Each `request()` has a per-call timeout (default
//! [`DEFAULT_REQUEST_TIMEOUT`]). If the server is silent past the
//! deadline, the pending entry is removed and the call returns
//! [`ClientError::Timeout`]. Without this the agent loop would
//! deadlock waiting on a hung remote tool — the model can't
//! cancel its own tool call mid-flight.
//!
//! ## Shutdown
//!
//! `Drop` signals a `oneshot::Sender` that the reader task selects
//! against; the reader exits cleanly the next tick, releases its
//! transport `Arc`, and the peer sees EOF. Earlier versions called
//! `JoinHandle::abort()` via a `try_lock` race — which silently
//! failed when the abort happened during a borrowed lock and left
//! the reader holding the transport forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::oneshot;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::protocol::{
    CallToolParams, CallToolResult, ClientCapabilities, Implementation, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, RequestId, PROTOCOL_VERSION,
};
use super::transport::{Transport, TransportError};

/// Default per-request timeout. MCP tools are often LLM-backed and
/// can take tens of seconds; 60s is a reasonable upper bound. Caller
/// code that needs longer waits should use `request_with_timeout`.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("server error: code={code} message={message}")]
    Server {
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("connection closed before response arrived")]
    ConnectionClosed,
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("protocol violation: {0}")]
    Protocol(String),
}

impl ClientError {
    fn from_server(err: JsonRpcError) -> Self {
        ClientError::Server {
            code: err.code,
            message: err.message,
            data: err.data,
        }
    }
}

type Pending = HashMap<RequestId, oneshot::Sender<JsonRpcResponse>>;

pub struct McpClient {
    transport: Arc<dyn Transport>,
    next_id: AtomicI64,
    pending: Arc<Mutex<Pending>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Set on `Drop` to signal the reader to exit cleanly. The reader
    /// `select!`s between this and the transport. Held inside a
    /// `Mutex<Option<…>>` because the sender is consumed by `.send()`.
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Per-request timeout. Override via [`McpClient::with_request_timeout`].
    request_timeout: Duration,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Signal the reader. Best-effort: if the reader already
        // exited (transport EOF), `send` errors and we just fall
        // through. Using a cooperative shutdown rather than
        // `JoinHandle::abort()` avoids the historical race where
        // `try_lock` could fail under load and leave the reader
        // holding the transport forever.
        if let Ok(mut guard) = self.shutdown_tx.try_lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        // Belt-and-braces: abort the JoinHandle if cooperative
        // shutdown couldn't deliver. The reader uses `recv()` which
        // may be parked indefinitely on a transport that never
        // EOFs (e.g. a stuck child process).
        if let Ok(mut guard) = self.reader.try_lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
    }
}

impl McpClient {
    /// Construct a client around a transport. `start()` spawns the
    /// background reader; do that before issuing any requests.
    pub fn new(transport: impl Transport) -> Arc<Self> {
        Arc::new(Self {
            transport: Arc::new(transport),
            next_id: AtomicI64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            reader: Mutex::new(None),
            shutdown_tx: Mutex::new(None),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    /// Spawn the background reader. Idempotent.
    pub async fn start(self: &Arc<Self>) {
        let mut guard = self.reader.lock().await;
        if guard.is_some() {
            return;
        }
        let transport = self.transport.clone();
        let pending = self.pending.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        *self.shutdown_tx.lock().await = Some(shutdown_tx);

        *guard = Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Cooperative shutdown — Drop hits this.
                    _ = &mut shutdown_rx => break,
                    next = transport.recv() => match next {
                        Ok(Some(frame)) => {
                            if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&frame) {
                                let id = resp.id.clone();
                                let mut p = pending.lock().await;
                                if let Some(tx) = p.remove(&id) {
                                    let _ = tx.send(resp);
                                }
                                // Unmatched responses (and notifications) are
                                // silently dropped at this layer.
                            }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    },
                }
            }
            // Reader exiting: cancel every outstanding request so
            // their `rx.await` resolves to ConnectionClosed instead
            // of hanging forever.
            let mut p = pending.lock().await;
            p.drain().for_each(|(_, tx)| drop(tx));
        }));
    }

    fn next_request_id(&self) -> RequestId {
        self.next_id.fetch_add(1, Ordering::Relaxed).into()
    }

    /// Send a JSON-RPC request and await the response. Returns the
    /// raw `result` value on success. Times out after
    /// [`Self::request_timeout`] (default [`DEFAULT_REQUEST_TIMEOUT`])
    /// — on timeout, removes the pending entry so the eventual late
    /// response from the server is silently dropped instead of
    /// queueing forever.
    pub async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ClientError> {
        let id = self.next_request_id();
        let env = JsonRpcRequest::new(id.clone(), method, params);
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().await;
            p.insert(id.clone(), tx);
        }
        let body = serde_json::to_string(&env).map_err(|e| ClientError::Encode(e.to_string()))?;
        if let Err(send_err) = self.transport.send(body).await {
            // Drop the pending entry so we don't leak the oneshot.
            let mut p = self.pending.lock().await;
            p.remove(&id);
            return Err(ClientError::Transport(send_err));
        }
        let resp = match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => return Err(ClientError::ConnectionClosed),
            Err(_) => {
                // Timeout fired. Reap the pending entry so a late
                // response doesn't accumulate forever.
                let mut p = self.pending.lock().await;
                p.remove(&id);
                return Err(ClientError::Timeout(self.request_timeout));
            }
        };
        if let Some(err) = resp.error {
            return Err(ClientError::from_server(err));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Fire-and-forget notification.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ClientError> {
        let env = JsonRpcNotification::new(method, params);
        let body = serde_json::to_string(&env).map_err(|e| ClientError::Encode(e.to_string()))?;
        self.transport.send(body).await?;
        Ok(())
    }

    // --- typed helpers ---------------------------------------

    pub async fn initialize(
        self: &Arc<Self>,
        client_info: Implementation,
        capabilities: ClientCapabilities,
    ) -> Result<InitializeResult, ClientError> {
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities,
            client_info,
        };
        let v = self
            .request(
                "initialize",
                Some(
                    serde_json::to_value(&params)
                        .map_err(|e| ClientError::Encode(e.to_string()))?,
                ),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub async fn list_tools(self: &Arc<Self>) -> Result<ListToolsResult, ClientError> {
        let v = self.request("tools/list", None).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    pub async fn call_tool(
        self: &Arc<Self>,
        name: impl Into<String>,
        arguments: Option<Value>,
    ) -> Result<CallToolResult, ClientError> {
        let params = CallToolParams {
            name: name.into(),
            arguments,
        };
        let v = self
            .request(
                "tools/call",
                Some(
                    serde_json::to_value(&params)
                        .map_err(|e| ClientError::Encode(e.to_string()))?,
                ),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/client.rs"
    ));
}
