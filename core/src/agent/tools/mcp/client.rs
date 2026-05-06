//! Outbound MCP client — drive a remote MCP server (e.g. a database
//! adapter, a filesystem provider) over a [`Transport`].
//!
//! Async pattern: `send_request().await` writes the request frame
//! and then awaits the matching response by id. We multiplex on a
//! single transport via a background reader task that demuxes
//! responses to per-request oneshot channels.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::protocol::{
    CallToolParams, CallToolResult, ClientCapabilities, Implementation, InitializeParams,
    InitializeResult, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    ListToolsResult, PROTOCOL_VERSION, RequestId,
};
use super::transport::{Transport, TransportError};

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
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // Abort the background reader so the transport halves can
        // close. Without this the reader holds an Arc to the
        // transport forever, deadlocking any peer waiting on EOF.
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
        *guard = Some(tokio::spawn(async move {
            loop {
                match transport.recv().await {
                    Ok(Some(frame)) => {
                        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&frame) {
                            let id = resp.id.clone();
                            let mut p = pending.lock().await;
                            if let Some(tx) = p.remove(&id) {
                                let _ = tx.send(resp);
                            }
                            // Unmatched responses are silently dropped;
                            // a Notification arriving on this channel
                            // is also ignored at this layer.
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            // Reader exiting: cancel every outstanding request.
            let mut p = pending.lock().await;
            p.drain().for_each(|(_, tx)| drop(tx));
        }));
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::Num(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Send a JSON-RPC request and await the response. Returns the
    /// raw `result` value on success.
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
        let body =
            serde_json::to_string(&env).map_err(|e| ClientError::Encode(e.to_string()))?;
        if let Err(send_err) = self.transport.send(body).await {
            // Drop the pending entry so we don't leak the oneshot.
            let mut p = self.pending.lock().await;
            p.remove(&id);
            return Err(ClientError::Transport(send_err));
        }
        let resp = rx.await.map_err(|_| ClientError::ConnectionClosed)?;
        if let Some(err) = resp.error {
            return Err(ClientError::from_server(err));
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Fire-and-forget notification.
    pub async fn notify(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), ClientError> {
        let env = JsonRpcNotification::new(method, params);
        let body =
            serde_json::to_string(&env).map_err(|e| ClientError::Encode(e.to_string()))?;
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
    use super::super::transport::in_memory_pair;
    use super::*;

    #[tokio::test]
    async fn request_round_trip_via_in_memory_pair() {
        let (client_t, server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        client.start().await;

        // Spawn a tiny "server" that echoes every request back as
        // result = params.
        let server = tokio::spawn(async move {
            while let Ok(Some(frame)) = server_t.recv().await {
                let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
                let resp =
                    JsonRpcResponse::ok(req.id, req.params.unwrap_or(Value::Null));
                server_t.send(serde_json::to_string(&resp).unwrap()).await.unwrap();
            }
        });

        let result = client
            .request("ping", Some(serde_json::json!({"hello": "world"})))
            .await
            .unwrap();
        assert_eq!(result["hello"], "world");

        drop(client);
        let _ = server.await;
    }

    #[tokio::test]
    async fn server_error_response_surfaces_as_client_error() {
        let (client_t, server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        client.start().await;

        let server = tokio::spawn(async move {
            let frame = server_t.recv().await.unwrap().unwrap();
            let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
            let resp = JsonRpcResponse::err(
                req.id,
                JsonRpcError::new(
                    super::super::protocol::ERR_METHOD_NOT_FOUND,
                    "no method",
                ),
            );
            server_t.send(serde_json::to_string(&resp).unwrap()).await.unwrap();
        });

        let err = client.request("missing", None).await.unwrap_err();
        match err {
            ClientError::Server { code, .. } => assert_eq!(code, super::super::protocol::ERR_METHOD_NOT_FOUND),
            other => panic!("expected Server error, got {other:?}"),
        }
        let _ = server.await;
    }

    #[tokio::test]
    async fn closed_transport_yields_connection_closed() {
        let (client_t, server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        client.start().await;
        // Drop server side immediately.
        drop(server_t);
        // Give reader a tick to notice EOF.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let err = client.request("x", None).await.unwrap_err();
        // Either surfaces fine: send fails (Transport(Closed)) once
        // the peer's receiver is dropped, or the reader noticed EOF
        // first and we get ConnectionClosed when the oneshot is
        // dropped.
        assert!(
            matches!(err, ClientError::ConnectionClosed | ClientError::Transport(_)),
            "expected ConnectionClosed or Transport, got {err:?}"
        );
    }
}
