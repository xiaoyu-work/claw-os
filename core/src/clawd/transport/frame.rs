//! Length-prefixed framing for the broker socket.
//!
//! A frame is a fixed 10-byte header followed by exactly the number of
//! payload bytes the header declares. Nothing is allocated for a body
//! until its length has been read and checked against the ceiling for
//! that direction, and nothing is scanned for a terminator, so a peer
//! cannot make the daemon buffer without bound by simply never sending
//! a newline.
//!
//! On the daemon side every read goes through `recvmsg(2)` so the
//! kernel's per-message credentials arrive with the bytes and any
//! descriptor the peer attached is closed and refused. See
//! [`super::peer`].

use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};

use tokio::io::{AsyncWriteExt, Interest};
use tokio::net::UnixStream;

use super::super::wire::{
    looks_like_legacy_request, Fault, HEADER_BYTES, KIND_REQUEST, KIND_RESPONSE, MAGIC,
};
use super::peer::{self, Credentials};

/// Largest chunk one `recvmsg` is asked for while filling a body.
const READ_CHUNK: usize = 64 * 1024;

/// Build one frame.
pub fn encode_frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_BYTES + body.len());
    frame.extend_from_slice(&MAGIC);
    frame.push(kind);
    frame.push(0);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

/// Validate a header and return the declared body length.
pub fn parse_header(
    header: &[u8; HEADER_BYTES],
    expected_kind: u8,
    max: usize,
) -> Result<usize, Fault> {
    if header[..4] != MAGIC {
        return Err(Fault::UnsupportedFrame);
    }
    if header[4] != expected_kind || header[5] != 0 {
        return Err(Fault::UnsupportedFrame);
    }
    let len = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as usize;
    if len > max {
        return Err(Fault::FrameTooLarge);
    }
    Ok(len)
}

/// One authenticated request frame.
pub struct RequestFrame {
    pub body: Vec<u8>,
    pub credentials: Credentials,
}

/// What the reader saw when a frame could not be produced.
pub enum ReadOutcome {
    /// The peer connected and closed without sending anything.
    Closed,
    /// A complete, credential-bearing frame.
    Frame(RequestFrame),
    /// The peer opened with the pre-v1 newline protocol.
    Legacy,
}

/// A connected peer, read through `recvmsg` so credentials travel with
/// the bytes.
pub struct PeerStream {
    stream: UnixStream,
    fd: RawFd,
    credentials: Option<Credentials>,
}

impl PeerStream {
    /// Wrap an accepted connection.
    ///
    /// `SO_PASSCRED` is already set on the listener and inherited here;
    /// setting it again is idempotent and keeps the guarantee local to
    /// this type even if the listener is ever constructed elsewhere.
    pub fn new(stream: UnixStream) -> std::io::Result<Self> {
        let fd = stream.as_raw_fd();
        peer::enable_credential_passing(fd)?;
        Ok(Self {
            stream,
            fd,
            credentials: None,
        })
    }

    /// Credentials the kernel attached to the frame just read.
    pub fn credentials(&self) -> Option<Credentials> {
        self.credentials
    }

    /// Read exactly one request frame.
    ///
    /// Every segment must carry the same credentials, must carry no
    /// descriptors, and must complete the frame the header declared.
    pub async fn read_request(&mut self, max: usize) -> Result<ReadOutcome, Fault> {
        let mut header = [0u8; HEADER_BYTES];
        match self.fill(&mut header).await? {
            Filled::Empty => return Ok(ReadOutcome::Closed),
            Filled::Partial(seen) => {
                if looks_like_legacy_request(&header[..seen]) {
                    return Ok(ReadOutcome::Legacy);
                }
                return Err(Fault::TruncatedFrame);
            }
            Filled::Complete => {}
        }
        if header[..4] != MAGIC && looks_like_legacy_request(&header) {
            return Ok(ReadOutcome::Legacy);
        }
        let len = parse_header(&header, KIND_REQUEST, max)?;
        let mut body = vec![0u8; len];
        if len > 0 {
            match self.fill(&mut body).await? {
                Filled::Complete => {}
                Filled::Empty | Filled::Partial(_) => return Err(Fault::TruncatedFrame),
            }
        }
        let credentials = self.credentials.ok_or(Fault::MissingCredentials)?;
        Ok(ReadOutcome::Frame(RequestFrame { body, credentials }))
    }

    /// Whether the peer already queued another frame.
    ///
    /// Best effort by construction — a peer can always write more after
    /// the check — but the connection is closed the moment its one
    /// response is written, so a frame that arrives later is never
    /// dispatched either way.
    pub fn has_pending_input(&self) -> bool {
        peer::pending_bytes(self.fd).is_ok_and(|bytes| bytes > 0)
    }

    pub async fn write_frame(&mut self, kind: u8, body: &[u8]) -> std::io::Result<()> {
        let frame = encode_frame(kind, body);
        self.stream.write_all(&frame).await?;
        self.stream.flush().await
    }

    pub async fn write_response(&mut self, body: &[u8]) -> std::io::Result<()> {
        self.write_frame(KIND_RESPONSE, body).await
    }

    /// Answer a peer speaking the pre-v1 newline protocol.
    ///
    /// Written without reading, parsing or authorizing anything the
    /// peer sent; it exists only so an out-of-date client prints why it
    /// was refused.
    pub async fn write_raw(&mut self, body: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(body).await?;
        self.stream.flush().await
    }

    /// Discard whatever the peer left queued, up to a fixed ceiling.
    ///
    /// Closing a Unix stream socket whose receive queue is not empty
    /// makes the kernel hand the peer `ECONNRESET`
    /// (`unix_release_sock`), which would throw away the refusal that
    /// was just written. Draining first turns that into a clean
    /// end-of-file. It is bounded in both bytes and iterations, and
    /// every descriptor found on the way is still closed, so this
    /// cannot be turned into a way to keep the daemon reading.
    pub async fn drain_pending(&mut self) {
        const MAX_DRAIN_BYTES: usize = 64 * 1024;
        let mut scratch = [0u8; 4096];
        let mut drained = 0usize;
        while drained < MAX_DRAIN_BYTES {
            if !self.has_pending_input() {
                return;
            }
            match self.recv(&mut scratch).await {
                Ok(segment) if segment.bytes > 0 => drained += segment.bytes,
                _ => return,
            }
        }
    }

    async fn fill(&mut self, buf: &mut [u8]) -> Result<Filled, Fault> {
        let mut filled = 0usize;
        while filled < buf.len() {
            let want = (buf.len() - filled).min(READ_CHUNK);
            let segment = self
                .recv(&mut buf[filled..filled + want])
                .await
                .map_err(|_| Fault::TruncatedFrame)?;
            if segment.descriptors > 0 {
                return Err(Fault::DescriptorPassing);
            }
            if segment.control_truncated {
                return Err(Fault::DescriptorPassing);
            }
            if segment.bytes == 0 {
                return Ok(if filled == 0 {
                    Filled::Empty
                } else {
                    Filled::Partial(filled)
                });
            }
            match (self.credentials, segment.credentials) {
                (_, None) => return Err(Fault::MissingCredentials),
                (None, Some(seen)) => self.credentials = Some(seen),
                (Some(known), Some(seen)) if known != seen => {
                    return Err(Fault::CredentialsChanged)
                }
                (Some(_), Some(_)) => {}
            }
            filled += segment.bytes;
        }
        Ok(Filled::Complete)
    }

    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<peer::Segment> {
        loop {
            self.stream.readable().await?;
            let attempt = self
                .stream
                .try_io(Interest::READABLE, || peer::recv_segment(self.fd, buf));
            match attempt {
                Ok(segment) => return Ok(segment),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

enum Filled {
    Empty,
    Partial(usize),
    Complete,
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Read one response frame from a blocking client socket.
pub fn read_response_blocking(
    stream: &mut std::os::unix::net::UnixStream,
    max: usize,
) -> Result<Vec<u8>, Fault> {
    let mut header = [0u8; HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .map_err(|_| Fault::TruncatedFrame)?;
    let len = parse_header(&header, KIND_RESPONSE, max)?;
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .map_err(|_| Fault::TruncatedFrame)?;
    Ok(body)
}

/// Write one request frame to a blocking client socket.
pub fn write_request_blocking(
    stream: &mut std::os::unix::net::UnixStream,
    body: &[u8],
) -> std::io::Result<()> {
    let frame = encode_frame(KIND_REQUEST, body);
    stream.write_all(&frame)?;
    stream.flush()
}

/// Read one response frame from an async client socket.
pub async fn read_response_async(stream: &mut UnixStream, max: usize) -> Result<Vec<u8>, Fault> {
    use tokio::io::AsyncReadExt;

    let mut header = [0u8; HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| Fault::TruncatedFrame)?;
    let len = parse_header(&header, KIND_RESPONSE, max)?;
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|_| Fault::TruncatedFrame)?;
    Ok(body)
}

/// Write one request frame to an async client socket.
pub async fn write_request_async(stream: &mut UnixStream, body: &[u8]) -> std::io::Result<()> {
    let frame = encode_frame(KIND_REQUEST, body);
    stream.write_all(&frame).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/transport/frame.rs"
    ));
}
