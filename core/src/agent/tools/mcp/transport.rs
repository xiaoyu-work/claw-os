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

/// Streamable-HTTP / SSE transport for connecting to a **remote** MCP
/// server (the MCP 2025 "Streamable HTTP" transport). This is the gap
/// that kept ClawOS from using hosted MCP servers — stdio only reaches
/// local subprocesses.
///
/// `send` POSTs a JSON-RPC frame to the endpoint; the server answers
/// either with a single `application/json` body or a
/// `text/event-stream` carrying one-or-more messages. Every response
/// frame is queued and surfaced through `recv`, so the client/server
/// modules keep speaking the same framed send/recv contract regardless
/// of transport. The server-assigned `Mcp-Session-Id` is captured from
/// the response headers and echoed on every subsequent request, as the
/// spec requires.
pub struct HttpTransport {
    client: reqwest::Client,
    url: reqwest::Url,
    /// Optional bearer token for authenticated servers (OAuth /
    /// static-token hubs). Sent as `Authorization: Bearer <token>`.
    bearer: Option<String>,
    /// Session id assigned by the server on the first response.
    session_id: Mutex<Option<String>>,
    tx: mpsc::UnboundedSender<Frame>,
    rx: Mutex<mpsc::UnboundedReceiver<Frame>>,
}

impl HttpTransport {
    /// Build a transport pointed at `url`. `bearer` is an optional
    /// pre-shared / OAuth access token.
    pub fn new(url: reqwest::Url, bearer: Option<String>) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| TransportError::Io(e.to_string()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        Ok(Self {
            client,
            url,
            bearer,
            session_id: Mutex::new(None),
            tx,
            rx: Mutex::new(rx),
        })
    }

    /// Parse an SSE body into individual JSON-RPC frames. Collects
    /// `data:` lines per event (a blank line ends an event); multi-line
    /// `data` is joined with `\n`. Non-`data` fields (`event:`, `id:`,
    /// comments) are ignored.
    fn push_sse_frames(body: &str, tx: &mpsc::UnboundedSender<Frame>) {
        let mut data = String::new();
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            } else if line.trim().is_empty() {
                if !data.trim().is_empty() {
                    let _ = tx.send(std::mem::take(&mut data));
                } else {
                    data.clear();
                }
            }
        }
        if !data.trim().is_empty() {
            let _ = tx.send(data);
        }
    }
}

/// Drain an HTTP response body to a `String`, refusing to buffer more
/// than 16 MiB so a hostile server can't OOM the agent.
async fn read_capped_http_body(resp: reqwest::Response) -> Result<String, TransportError> {
    use futures_util::StreamExt;
    const CAP: usize = 16 * 1024 * 1024;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| TransportError::Io(e.to_string()))?;
        if buf.len().saturating_add(chunk.len()) > CAP {
            return Err(TransportError::Decode(
                "http response exceeded 16 MiB".into(),
            ));
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|e| TransportError::Decode(format!("non-utf8 body: {e}")))
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, frame: Frame) -> Result<(), TransportError> {
        let mut req = self
            .client
            .post(self.url.clone())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(frame);
        if let Some(b) = &self.bearer {
            req = req.header("authorization", format!("Bearer {b}"));
        }
        if let Some(s) = self.session_id.lock().await.as_ref() {
            req = req.header("mcp-session-id", s.clone());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;

        // Capture the server-assigned session id (first response wins;
        // re-set if the server rotates it).
        if let Some(v) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(v.to_string());
        }

        let status = resp.status();
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = read_capped_http_body(resp).await?;

        if !status.is_success() {
            let preview: String = body.chars().take(256).collect();
            return Err(TransportError::Io(format!("http {status}: {preview}")));
        }

        if ctype.contains("text/event-stream") {
            Self::push_sse_frames(&body, &self.tx);
        } else if !body.trim().is_empty() {
            // application/json single response (or unspecified). A 202
            // with empty body — a server ack of a notification — queues
            // nothing, which is correct.
            let _ = self.tx.send(body);
        }
        Ok(())
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut rx = self.rx.lock().await;
        Ok(rx.recv().await)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/transport.rs"
    ));
}
