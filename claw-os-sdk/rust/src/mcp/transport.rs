//! Transport abstraction for MCP — newline-delimited JSON frames.
//!
//! MCP is officially "JSON-RPC over a transport". Stdio and HTTP+SSE
//! are common; we model the common shape (send a serialized message,
//! receive a serialized message) so the client and server modules
//! don't care whether the bytes flow over a pipe, socket, or
//! in-memory channel.
//!
//! [`InMemoryTransport`] is a paired-channel helper used in tests.
//! [`StdioTransport`] reads/writes newline-delimited JSON-RPC frames
//! over a configurable async pair (`tokio::io::stdin/stdout` in the
//! production path; arbitrary `tokio::io::DuplexStream` halves in
//! tests). Used by `cos agent mcp serve` to expose the cos tool
//! catalogue to MCP clients (Claude Desktop / Cursor / Cody).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

/// Maximum bytes the transport will buffer for a single frame before
/// surfacing [`TransportError::TooLarge`] and bailing out. Picked to be
/// comfortably above any realistic MCP frame (tool args, prompts,
/// embedded JSON-RPC payloads) while keeping a single misbehaving peer
/// from exhausting the host. 16 MiB matches the SDK-side serve.py
/// cap so the two language implementations agree on what an oversize frame looks like.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Capacity of the in-memory channel paired by [`in_memory_pair`].
/// Bounded so a stalled receiver applies backpressure on the sender
/// instead of letting both sides grow without limit (the prior
/// `unbounded_channel` made every test that forgets to drain the
/// peer an OOM risk).
pub const PAIR_CHANNEL_CAPACITY: usize = 1024;

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
    /// A peer tried to send more than [`MAX_FRAME_BYTES`] of data in a
    /// single frame. The transport drops the connection rather than
    /// keep buffering.
    #[error("frame exceeded {limit} bytes")]
    TooLarge { limit: usize },
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
    let (a_tx, a_rx) = mpsc::channel(PAIR_CHANNEL_CAPACITY);
    let (b_tx, b_rx) = mpsc::channel(PAIR_CHANNEL_CAPACITY);
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
    outgoing: mpsc::Sender<Frame>,
    incoming: Arc<Mutex<mpsc::Receiver<Frame>>>,
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        self.outgoing
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut rx = self.incoming.lock().await;
        Ok(rx.recv().await)
    }
}

/// Newline-delimited JSON-RPC over an arbitrary async reader/writer
/// pair. Production: `StdioTransport::stdio()` wires the OS stdio
/// streams. Tests: pass `tokio::io::DuplexStream` halves.
///
/// Frames are written as `<json>\n`. Reads are line-buffered; lines
/// containing only whitespace are silently skipped (a courtesy for
/// peers that emit blank-line keepalives). Maximum frame size is
/// bounded only by the underlying buffer; pathological clients
/// can exhaust memory but this is the same constraint MCP itself
/// places on its transport.
pub struct StdioTransport {
    reader: Mutex<BufReader<Box<dyn AsyncRead + Send + Unpin>>>,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
}

impl StdioTransport {
    /// Build from arbitrary async halves. Used by tests with
    /// `tokio::io::duplex` and by production with `tokio::io::stdin`/
    /// `stdout`.
    pub fn from_pair(
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Self {
        Self {
            reader: Mutex::new(BufReader::new(reader)),
            writer: Mutex::new(writer),
        }
    }

    /// Production constructor: wires `tokio::io::stdin()` and
    /// `tokio::io::stdout()`. Inherits the parent process's stdio,
    /// which is exactly what an MCP client subprocess wants.
    pub fn stdio() -> Self {
        Self::from_pair(Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        let mut w = self.writer.lock().await;
        w.write_all(frame.as_bytes())
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        w.write_all(b"\n")
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        w.flush()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(())
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut r = self.reader.lock().await;
        loop {
            // Read one frame using AsyncBufRead's fill_buf/consume so
            // we get the underlying buffered chunks without paying
            // for a per-byte read syscall. After every chunk we
            // check against MAX_FRAME_BYTES so a pathological peer
            // can't drive us OOM by streaming an endless "line".
            // This replaces the prior `read_line` (no size bound) —
            // See also the matching Python SDK cap in
            // claw_os_sdk.mcp.MAX_LINE_BYTES.
            let mut buf: Vec<u8> = Vec::new();
            loop {
                // The fill_buf borrow ends inside this inner block,
                // freeing `r` for the matching `consume` call below.
                enum ChunkDecision {
                    Eof,
                    Newline(usize),
                    Continue(usize),
                    TooLarge(usize),
                }
                let decision = {
                    let chunk = r
                        .fill_buf()
                        .await
                        .map_err(|e| TransportError::Io(e.to_string()))?;
                    if chunk.is_empty() {
                        ChunkDecision::Eof
                    } else if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
                        if buf.len() + pos > MAX_FRAME_BYTES {
                            ChunkDecision::TooLarge(pos + 1)
                        } else {
                            buf.extend_from_slice(&chunk[..pos]);
                            ChunkDecision::Newline(pos + 1)
                        }
                    } else if buf.len() + chunk.len() > MAX_FRAME_BYTES {
                        ChunkDecision::TooLarge(chunk.len())
                    } else {
                        buf.extend_from_slice(chunk);
                        ChunkDecision::Continue(chunk.len())
                    }
                };
                match decision {
                    ChunkDecision::Eof => {
                        if buf.is_empty() {
                            return Ok(None);
                        }
                        break;
                    }
                    ChunkDecision::Newline(advance) => {
                        r.consume(advance);
                        break;
                    }
                    ChunkDecision::Continue(advance) => {
                        r.consume(advance);
                    }
                    ChunkDecision::TooLarge(advance) => {
                        r.consume(advance);
                        return Err(TransportError::TooLarge {
                            limit: MAX_FRAME_BYTES,
                        });
                    }
                }
            }
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            let line = match String::from_utf8(buf) {
                Ok(s) => s,
                Err(e) => return Err(TransportError::Decode(format!("invalid utf-8: {e}"))),
            };
            if line.trim().is_empty() {
                continue;
            }
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/mcp/transport.rs"
    ));
}
