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
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

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

/// Newline-delimited JSON-RPC over an arbitrary async reader/writer
/// pair. Production: `StdioTransport::stdio()` wires the OS stdio
/// streams. Tests: pass `tokio::io::DuplexStream` halves.
///
/// Frames are written as `<json>\n`. Reads are line-buffered; lines
/// containing only whitespace are silently skipped (a courtesy for
/// peers that emit blank-line keepalives).
///
/// **Frame-size cap**: a single frame is capped at
/// [`MAX_FRAME_BYTES`] (16 MiB). A peer that streams data without
/// ever emitting `\n` will hit this cap; the transport returns
/// `TransportError::Decode("frame too large")` and the reader loop
/// is expected to tear down the connection. The cap protects the
/// process from a hostile / buggy MCP peer that would otherwise
/// drive us to OOM by streaming megabytes per "frame".
pub struct StdioTransport {
    reader: Mutex<BufReader<Box<dyn AsyncRead + Send + Unpin>>>,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    /// Per-frame byte cap. Defaults to [`MAX_FRAME_BYTES`]; tests
    /// override with a lower value to exercise the OOM defence
    /// without a real-size frame.
    max_frame_bytes: usize,
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
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }

    /// Production constructor: wires `tokio::io::stdin()` and
    /// `tokio::io::stdout()`. Inherits the parent process's stdio,
    /// which is exactly what an MCP client subprocess wants.
    pub fn stdio() -> Self {
        Self::from_pair(Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
    }

    /// Test-only: override the per-frame byte cap so the
    /// oversize-frame defence can be exercised without streaming
    /// the full 16 MiB.
    #[cfg(test)]
    pub(crate) fn with_max_frame_bytes(mut self, n: usize) -> Self {
        self.max_frame_bytes = n;
        self
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
        let cap = self.max_frame_bytes;
        loop {
            // Wrap the locked reader in a per-call `take()` limit so a
            // peer that streams without ever sending `\n` cannot drive
            // us to OOM. We pull at most `cap + 1` bytes — the `+1`
            // lets us detect "exactly at limit vs. went past" without
            // a second probe.
            let mut limited = (&mut *r).take(cap as u64 + 1);
            let mut buf = Vec::with_capacity(256);
            let n = limited
                .read_until(b'\n', &mut buf)
                .await
                .map_err(|e| TransportError::Io(e.to_string()))?;
            if n == 0 {
                return Ok(None);
            }
            // If we read more than `cap` bytes without seeing `\n`,
            // the peer's frame exceeded the cap. Drop the connection
            // — recovery is impossible (we don't know where the
            // truncation falls in the JSON grammar).
            if n > cap && buf.last() != Some(&b'\n') {
                return Err(TransportError::Decode(format!(
                    "frame exceeded {cap} bytes"
                )));
            }
            let line = match std::str::from_utf8(&buf) {
                Ok(s) => s,
                Err(e) => {
                    return Err(TransportError::Decode(format!("non-utf8 frame: {e}")));
                }
            };
            // Strip trailing newline(s); skip blank-line keepalives.
            let trimmed = line.trim_end_matches(['\n', '\r']).to_string();
            if trimmed.trim().is_empty() {
                continue;
            }
            return Ok(Some(trimmed));
        }
    }
}

/// Per-frame byte cap for [`StdioTransport`]. Chosen as 16 MiB — well
/// above any plausible legitimate MCP message (tool descriptors,
/// resource bodies, image attachments) and well below a process
/// memory limit that would matter operationally.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

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

    /// StdioTransport over a `tokio::io::duplex` pair. Reader sees
    /// what the writer wrote, framed by `\n`. Smoke test: round-trip
    /// one frame both directions.
    #[tokio::test]
    async fn stdio_transport_round_trips_frame() {
        // Two duplexes give us a full bidirectional pair: the
        // server-under-test reads from `srv_r` and writes to
        // `srv_w`; the test driver writes to `cli_w` and reads
        // from `cli_r`.
        let (cli_r_to_srv, srv_in) = tokio::io::duplex(4096);
        let (srv_out, cli_w_from_srv) = tokio::io::duplex(4096);
        let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));

        // Drive: write a request from client side; server reads it.
        let (mut cli_in_w, _hold) = (cli_r_to_srv, ());
        let _ = _hold;
        use tokio::io::AsyncWriteExt;
        cli_in_w.write_all(b"{\"hello\":1}\n").await.unwrap();
        cli_in_w.flush().await.unwrap();
        let frame = server.recv().await.unwrap().unwrap();
        assert_eq!(frame, "{\"hello\":1}");

        // Server writes a response; client side reads it.
        server.send("{\"ok\":true}".into()).await.unwrap();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64];
        let n = cli_w_from_srv.take(64).read(&mut buf).await.unwrap();
        let s = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(
            s.starts_with("{\"ok\":true}"),
            "frame should be JSON line, got {s:?}"
        );
        assert!(s.contains('\n'), "frame must end with newline");
    }

    /// Blank lines between frames are tolerated as keepalives.
    #[tokio::test]
    async fn stdio_transport_skips_blank_lines() {
        let (mut cli_in_w, srv_in) = tokio::io::duplex(4096);
        let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
        let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));

        use tokio::io::AsyncWriteExt;
        cli_in_w.write_all(b"\n\n   \n{\"go\":1}\n").await.unwrap();
        cli_in_w.flush().await.unwrap();

        let frame = server.recv().await.unwrap().unwrap();
        assert_eq!(frame, "{\"go\":1}");
    }

    /// EOF on the read half surfaces as `Ok(None)`, matching the
    /// transport-closed contract documented on `Transport::recv`.
    #[tokio::test]
    async fn stdio_transport_eof_returns_none() {
        let (cli_in_w, srv_in) = tokio::io::duplex(4096);
        let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
        let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out));
        drop(cli_in_w);

        let frame = server.recv().await.unwrap();
        assert!(frame.is_none(), "EOF must surface as Ok(None)");
    }

    /// A peer that streams more bytes than the configured per-frame
    /// cap without ever sending `\n` must be cut off with a `Decode`
    /// error rather than allowed to drive us to OOM. Use a small
    /// override of the cap to keep the test fast.
    #[tokio::test]
    async fn stdio_transport_rejects_oversize_frame() {
        use tokio::io::AsyncWriteExt;
        let (mut cli_in_w, srv_in) = tokio::io::duplex(8192);
        let (srv_out, _cli_out_r) = tokio::io::duplex(4096);
        let server = StdioTransport::from_pair(Box::new(srv_in), Box::new(srv_out))
            .with_max_frame_bytes(1024);

        let recv_task = tokio::spawn(async move { server.recv().await });

        // 2 KiB of bytes, no newline. Cap is 1 KiB → cap+1 = 1025
        // bytes get pulled, then `\n` not found → Decode error.
        let payload = vec![b'x'; 2048];
        let _ = cli_in_w.write_all(&payload).await;
        let _ = cli_in_w.shutdown().await;
        drop(cli_in_w);

        let result = recv_task.await.unwrap();
        match result {
            Err(TransportError::Decode(msg)) => {
                assert!(msg.contains("exceeded"), "got {msg}");
            }
            other => panic!("expected Decode(exceeded), got {other:?}"),
        }
    }
}
