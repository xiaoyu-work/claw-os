//! AWS EventStream binary frame parser.
//!
//! Used by [`crate::agent::llm::providers::bedrock`] to decode the
//! response body of `InvokeModelWithResponseStream`. Each HTTP body
//! is a sequence of independent binary frames. One frame =
//!
//! ```text
//! +--- Prelude (12 bytes) -------------------------+
//! |  total_byte_length    (u32 BE)   — 4 bytes     |
//! |  headers_byte_length  (u32 BE)   — 4 bytes     |
//! |  prelude_crc32        (u32 BE)   — 4 bytes     | ← CRC32 of first 8 bytes
//! +--- Headers (headers_byte_length bytes) --------+
//! |  for each header:                              |
//! |    name_len    (u8)                            |
//! |    name        (utf-8, name_len bytes)         |
//! |    value_type  (u8)         — 7 = string       |
//! |    value_len   (u16 BE, 2 bytes)               |
//! |    value       (utf-8, value_len bytes)        |
//! +--- Payload (variable) -------------------------+
//! |  length = total_byte_length - headers_byte_length - 16  |
//! +--- Trailer (4 bytes) --------------------------+
//! |  message_crc32  (u32 BE)                       | ← CRC32 of all preceding bytes
//! +------------------------------------------------+
//! ```
//!
//! API mirrors [`crate::agent::llm::sse::SseParser`]:
//! - [`EventStreamParser::feed`] accumulates bytes
//! - [`EventStreamParser::pop_frame`] yields one decoded frame at a
//!   time
//! - [`EventStreamParser::finish`] reports unterminated input as an
//!   error (truncation, **not** clean completion)
//!
//! Failure modes are deliberately strict — any prelude or message
//! CRC mismatch terminates the stream. Per the AWS event stream
//! spec, length fields are themselves untrusted on a CRC failure,
//! so resync would be unsound.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub headers: HashMap<String, String>,
    pub payload: Vec<u8>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum FrameError {
    #[error(
        "invalid prelude crc32 (expected {expected:#010x}, got {actual:#010x}); stream corrupt"
    )]
    PreludeCrc { expected: u32, actual: u32 },
    #[error(
        "invalid message crc32 (expected {expected:#010x}, got {actual:#010x}); stream corrupt"
    )]
    MessageCrc { expected: u32, actual: u32 },
    #[error("structurally invalid frame: total_len={total_len}, headers_len={headers_len} (need total_len >= 16 and headers_len <= total_len - 16)")]
    BadStructure { total_len: u32, headers_len: u32 },
    #[error("truncated input: {0} byte(s) of partial frame remained at EOF")]
    Truncated(usize),
    #[error("malformed header: {0}")]
    BadHeader(String),
    #[error("unsupported header value type {value_type:#04x} for name {name:?} (only string=7 is implemented)")]
    UnsupportedHeaderType { name: String, value_type: u8 },
}

const PRELUDE_LEN: usize = 12;
const TRAILER_LEN: usize = 4;
/// Frames must be at least 16 bytes (12 prelude + 4 trailer).
const MIN_FRAME_LEN: u32 = 16;
/// AWS spec caps each frame at 16 MiB.
const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
const STRING_HEADER_TYPE: u8 = 7;

#[derive(Debug, Default)]
pub struct EventStreamParser {
    buf: Vec<u8>,
    /// Sticky terminal error — once set, further `feed`/`pop_frame`
    /// calls always return it. Mirrors how a desynced byte stream
    /// is unrecoverable per the spec.
    fatal: Option<FrameError>,
}

impl EventStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.fatal.is_some() {
            return;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Try to extract one frame.
    ///
    /// Returns:
    /// - `Some(Ok(frame))`: a complete, validated frame
    /// - `Some(Err(...))`: a fatal protocol error (CRC mismatch /
    ///   structurally invalid frame). The parser is poisoned and
    ///   future calls keep returning the same error.
    /// - `None`: not enough bytes yet for the next frame.
    pub fn pop_frame(&mut self) -> Option<Result<Frame, FrameError>> {
        if let Some(e) = &self.fatal {
            return Some(Err(e.clone()));
        }
        if self.buf.len() < PRELUDE_LEN {
            return None;
        }

        let total_len = read_u32_be(&self.buf[0..4]);
        let headers_len = read_u32_be(&self.buf[4..8]);
        let prelude_crc = read_u32_be(&self.buf[8..12]);

        if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&total_len)
            || headers_len > total_len.saturating_sub(MIN_FRAME_LEN)
        {
            let e = FrameError::BadStructure {
                total_len,
                headers_len,
            };
            self.fatal = Some(e.clone());
            return Some(Err(e));
        }

        let actual_prelude_crc = crc32(&self.buf[0..8]);
        if actual_prelude_crc != prelude_crc {
            let e = FrameError::PreludeCrc {
                expected: prelude_crc,
                actual: actual_prelude_crc,
            };
            self.fatal = Some(e.clone());
            return Some(Err(e));
        }

        let total = total_len as usize;
        if self.buf.len() < total {
            return None;
        }

        let frame_bytes = self.buf[..total].to_vec();
        // Remove the consumed bytes regardless of payload result —
        // sticky-fatal errors prevent further pops anyway.
        self.buf.drain(0..total);

        // Validate the message CRC over (frame minus trailer).
        let message_crc_offset = total - TRAILER_LEN;
        let actual_msg_crc = crc32(&frame_bytes[..message_crc_offset]);
        let expected_msg_crc = read_u32_be(&frame_bytes[message_crc_offset..total]);
        if actual_msg_crc != expected_msg_crc {
            let e = FrameError::MessageCrc {
                expected: expected_msg_crc,
                actual: actual_msg_crc,
            };
            self.fatal = Some(e.clone());
            return Some(Err(e));
        }

        // Parse headers.
        let headers_start = PRELUDE_LEN;
        let headers_end = headers_start + headers_len as usize;
        let headers = match parse_headers(&frame_bytes[headers_start..headers_end]) {
            Ok(h) => h,
            Err(e) => {
                self.fatal = Some(e.clone());
                return Some(Err(e));
            }
        };

        let payload = frame_bytes[headers_end..message_crc_offset].to_vec();
        Some(Ok(Frame { headers, payload }))
    }

    /// Drain any remaining buffered bytes. If the buffer is empty,
    /// returns `Ok(())`. Otherwise the upstream truncated mid-frame
    /// — return the partial-byte count so the caller can surface a
    /// transport-level error.
    pub fn finish(self) -> Result<(), FrameError> {
        if let Some(e) = self.fatal {
            return Err(e);
        }
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(FrameError::Truncated(self.buf.len()))
        }
    }

    #[cfg(test)]
    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }
}

fn parse_headers(mut buf: &[u8]) -> Result<HashMap<String, String>, FrameError> {
    let mut out = HashMap::new();
    while !buf.is_empty() {
        if buf.is_empty() {
            return Err(FrameError::BadHeader("missing name length byte".into()));
        }
        let name_len = buf[0] as usize;
        buf = &buf[1..];
        if buf.len() < name_len {
            return Err(FrameError::BadHeader(format!(
                "name length {name_len} exceeds remaining {}",
                buf.len()
            )));
        }
        let name = std::str::from_utf8(&buf[..name_len])
            .map_err(|e| FrameError::BadHeader(format!("name not utf-8: {e}")))?
            .to_string();
        buf = &buf[name_len..];

        if buf.is_empty() {
            return Err(FrameError::BadHeader(format!(
                "missing type byte for header {name:?}"
            )));
        }
        let value_type = buf[0];
        buf = &buf[1..];

        if value_type != STRING_HEADER_TYPE {
            return Err(FrameError::UnsupportedHeaderType { name, value_type });
        }

        if buf.len() < 2 {
            return Err(FrameError::BadHeader(format!(
                "missing value length bytes for header {name:?}"
            )));
        }
        let value_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        buf = &buf[2..];

        if buf.len() < value_len {
            return Err(FrameError::BadHeader(format!(
                "value length {value_len} exceeds remaining {} for header {name:?}",
                buf.len()
            )));
        }
        let value = std::str::from_utf8(&buf[..value_len])
            .map_err(|e| FrameError::BadHeader(format!("value not utf-8: {e}")))?
            .to_string();
        buf = &buf[value_len..];

        out.insert(name, value);
    }
    Ok(out)
}

fn read_u32_be(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(bytes);
    h.finalize()
}

// --------------------------------------------------------------------
// Helpers shared with tests + with the streaming wrapper that wants
// to assemble synthetic frames (e.g. when a server-sent test case is
// represented as `headers + payload` instead of raw bytes).

/// Encode a string-typed header into the AWS EventStream byte
/// layout. Public to the crate so the bedrock streaming tests can
/// build canonical frames without re-implementing the math.
pub fn encode_string_header(name: &str, value: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + name.len() + 1 + 2 + value.len());
    v.push(name.len() as u8);
    v.extend_from_slice(name.as_bytes());
    v.push(STRING_HEADER_TYPE);
    v.extend_from_slice(&(value.len() as u16).to_be_bytes());
    v.extend_from_slice(value.as_bytes());
    v
}

/// Build a fully-framed message: 12-byte prelude (with CRC) +
/// headers + payload + 4-byte trailer (with CRC). Public so the
/// streaming wrapper's tests can construct test inputs.
pub fn encode_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut headers_bytes = Vec::new();
    for (n, v) in headers {
        headers_bytes.extend_from_slice(&encode_string_header(n, v));
    }
    let total_len = (PRELUDE_LEN + headers_bytes.len() + payload.len() + TRAILER_LEN) as u32;
    let headers_len = headers_bytes.len() as u32;

    let mut prelude = Vec::with_capacity(PRELUDE_LEN);
    prelude.extend_from_slice(&total_len.to_be_bytes());
    prelude.extend_from_slice(&headers_len.to_be_bytes());
    let prelude_crc = crc32(&prelude);
    prelude.extend_from_slice(&prelude_crc.to_be_bytes());

    let mut frame = Vec::with_capacity(total_len as usize);
    frame.extend_from_slice(&prelude);
    frame.extend_from_slice(&headers_bytes);
    frame.extend_from_slice(payload);
    let msg_crc = crc32(&frame);
    frame.extend_from_slice(&msg_crc.to_be_bytes());
    frame
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/aws_eventstream.rs"
    ));
}
