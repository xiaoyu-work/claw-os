use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const PAIR_CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Message(String),
    Oversized,
    InvalidUtf8,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
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

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn send(&self, frame: String) -> Result<(), TransportError>;

    async fn recv(&self) -> Result<Option<Frame>, TransportError>;
}

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
    outgoing: mpsc::Sender<String>,
    incoming: Arc<Mutex<mpsc::Receiver<String>>>,
}

#[async_trait]
impl Transport for InMemoryTransport {
    async fn send(&self, frame: String) -> Result<(), TransportError> {
        self.outgoing
            .send(frame)
            .await
            .map_err(|_| TransportError::Closed)
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut receiver = self.incoming.lock().await;
        Ok(receiver.recv().await.map(Frame::Message))
    }
}

/// Newline-delimited JSON-RPC over an async reader and writer.
pub struct StdioTransport {
    reader: Mutex<BufReader<Box<dyn AsyncRead + Send + Unpin>>>,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
}

impl StdioTransport {
    pub fn from_pair(
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Self {
        Self {
            reader: Mutex::new(BufReader::new(reader)),
            writer: Mutex::new(writer),
        }
    }

    pub fn stdio() -> Self {
        Self::from_pair(Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, frame: String) -> Result<(), TransportError> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(frame.as_bytes())
            .await
            .map_err(|error| TransportError::Io(error.to_string()))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|error| TransportError::Io(error.to_string()))?;
        writer
            .flush()
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    async fn recv(&self) -> Result<Option<Frame>, TransportError> {
        let mut reader = self.reader.lock().await;
        loop {
            let mut bytes = Vec::new();
            let mut oversized = false;
            loop {
                enum Decision {
                    Eof,
                    Newline(usize),
                    Continue(usize),
                }
                let decision = {
                    let chunk = reader
                        .fill_buf()
                        .await
                        .map_err(|error| TransportError::Io(error.to_string()))?;
                    if chunk.is_empty() {
                        Decision::Eof
                    } else if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
                        if !oversized {
                            if bytes.len() + position > MAX_FRAME_BYTES {
                                bytes.clear();
                                oversized = true;
                            } else {
                                bytes.extend_from_slice(&chunk[..position]);
                            }
                        }
                        Decision::Newline(position + 1)
                    } else {
                        if !oversized {
                            if bytes.len() + chunk.len() > MAX_FRAME_BYTES {
                                bytes.clear();
                                oversized = true;
                            } else {
                                bytes.extend_from_slice(chunk);
                            }
                        }
                        Decision::Continue(chunk.len())
                    }
                };
                match decision {
                    Decision::Eof => {
                        if oversized {
                            return Ok(Some(Frame::Oversized));
                        }
                        if bytes.is_empty() {
                            return Ok(None);
                        }
                        break;
                    }
                    Decision::Newline(consumed) => {
                        reader.consume(consumed);
                        if oversized {
                            return Ok(Some(Frame::Oversized));
                        }
                        break;
                    }
                    Decision::Continue(consumed) => reader.consume(consumed),
                }
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            let frame = match String::from_utf8(bytes) {
                Ok(frame) => frame,
                Err(_) => return Ok(Some(Frame::InvalidUtf8)),
            };
            if frame.trim().is_empty() {
                continue;
            }
            return Ok(Some(Frame::Message(frame)));
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
