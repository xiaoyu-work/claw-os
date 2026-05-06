//! Transport abstraction for MCP — newline-delimited JSON frames.
//!
//! MCP is officially "JSON-RPC over a transport". Stdio and HTTP+SSE
//! are common; we model the common shape (send a serialized message,
//! receive a serialized message) so the client and server modules
//! don't care whether the bytes flow over a pipe, socket, or
//! in-memory channel.
//!
//! [`InMemoryTransport`] is a paired-channel helper used in tests.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed")]
    Closed,
    #[error("io: {0}")]
    Io(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
}

/// One framed JSON message — already serialized (we keep the
/// transport JSON-agnostic so multi-frame chunked encodings stay
/// possible later).
pub type Frame = String;

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Send one fully-serialized JSON-RPC envelope. Implementations
    /// must handle framing (e.g. append `\n` for stdio).
    async fn send(&self, frame: Frame) -> Result<(), TransportError>;

    /// Receive the next framed message. Returns `None` after the
    /// remote half-closes; further calls should keep returning
    /// `None`.
    async fn recv(&self) -> Result<Option<Frame>, TransportError>;
}

/// In-memory transport — useful for client+server pair tests
/// without spawning subprocesses. Returned as
/// `(client_side, server_side)`; messages sent on one are received
/// on the other.
pub fn in_memory_pair() -> (InMemoryTransport, InMemoryTransport) {
    let (a_tx, a_rx) = mpsc::unbounded_channel();
    let (b_tx, b_rx) = mpsc::unbounded_channel();
    let client = InMemoryTransport {
        outgoing: a_tx,
        incoming: Arc::new(Mutex::new(b_rx)),
    };
    let server = InMemoryTransport {
        outgoing: b_tx,
        incoming: Arc::new(Mutex::new(a_rx)),
    };
    (client, server)
}

pub struct InMemoryTransport {
    outgoing: mpsc::UnboundedSender<Frame>,
    incoming: Arc<Mutex<mpsc::UnboundedReceiver<Frame>>>,
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        self.outgoing
            .send(frame)
            .map_err(|_| TransportError::Closed)
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut rx = self.incoming.lock().await;
        Ok(rx.recv().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pair_round_trips() {
        let (a, b) = in_memory_pair();
        a.send("{\"hi\":1}".into()).await.unwrap();
        let recv = b.recv().await.unwrap();
        assert_eq!(recv.as_deref(), Some("{\"hi\":1}"));
    }

    #[tokio::test]
    async fn closing_one_end_signals_other() {
        let (a, b) = in_memory_pair();
        drop(a);
        let recv = b.recv().await.unwrap();
        assert!(recv.is_none(), "peer drop must surface as Ok(None)");
    }

    #[tokio::test]
    async fn send_after_close_errors() {
        let (a, b) = in_memory_pair();
        drop(b);
        let err = a.send("{}".into()).await.unwrap_err();
        assert!(matches!(err, TransportError::Closed));
    }
}
