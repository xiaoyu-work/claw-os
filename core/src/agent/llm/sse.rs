//! Server-Sent Events (SSE) byte-stream parser.
//!
//! Pure (no IO, no tokio) parser that takes raw bytes — possibly
//! split across arbitrary network-chunk boundaries — and yields
//! complete SSE events as `(event_name, data)` pairs. Designed for
//! the Anthropic Messages SSE format (`event: message_start\n
//! data: {...}\n\n`) but generic enough for any RFC 8895 / WHATWG
//! event-stream stream.
//!
//! Why hand-roll instead of pulling `eventsource-stream`?
//! - One small, well-defined protocol — full impl < 200 LoC.
//! - Avoid the transitive dep surface (`eventsource-stream` pulls
//!   `pin-project-lite`, `nom`, etc).
//! - Unit-testable as a pure function from `&[u8]` chunks → events,
//!   no async runtime in the test path.
//!
//! ## Wire format (the relevant subset)
//!
//! Events are separated by a blank line (`\n\n` or `\r\n\r\n`).
//! Each event is a sequence of `field: value\n` lines, where field
//! is one of `event`, `data`, `id`, `retry`, or anything else
//! (which is ignored). A line starting with `:` is a comment.
//!
//! Multiple `data:` lines within one event concatenate with
//! `\n` between (the **trailing** `\n` from each line is part of
//! the next data fragment). Per the spec we strip a leading single
//! space from each value.
//!
//! ## What this parser does NOT track
//!
//! - `id:` (Last-Event-ID) reconnection — we don't reconnect
//! - `retry:` — we don't reconnect
//! - BOM stripping — Anthropic's stream is UTF-8 without BOM;
//!   if a future provider sends BOM the first event's `event`
//!   field will be ignored cleanly (unknown field, dropped)

use std::collections::VecDeque;

/// Hard caps to keep a hostile / runaway upstream from OOMing us via
/// a malformed SSE stream. The body cap at the `read_body_capped`
/// layer protects non-streaming readers; these protect the streaming
/// parser's own internal state.
///
/// * `MAX_LINE_BUFFER_BYTES` — one event-stream line never legitimately
///   exceeds ~1 MiB. An upstream sending one giant unterminated line
///   would otherwise have us buffer forever.
/// * `MAX_PENDING_LINES` — Anthropic events use a handful of `data:`
///   lines per message; 10k is a generous ceiling.
/// * `MAX_READY_BYTES` — events queued but not yet popped sum to at
///   most 64 MiB before we treat the stream as runaway.
pub const MAX_LINE_BUFFER_BYTES: usize = 1024 * 1024;
pub const MAX_PENDING_LINES: usize = 10_000;
pub const MAX_READY_BYTES: usize = 64 * 1024 * 1024;

/// Error returned by [`SseParser::feed`] / [`SseParser::finish`] when
/// the parser would otherwise need to allocate beyond the configured
/// caps. Callers should treat it as a fatal stream-level error,
/// terminate the response, and bubble up as
/// [`crate::agent::llm::LlmError::UpstreamMalformed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseOverflow {
    /// Which buffer overflowed.
    pub kind: SseOverflowKind,
    /// Limit that was exceeded.
    pub cap: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseOverflowKind {
    LineBuffer,
    PendingLines,
    ReadyBytes,
}

impl std::fmt::Display for SseOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            SseOverflowKind::LineBuffer => "incoming line buffer",
            SseOverflowKind::PendingLines => "pending lines per event",
            SseOverflowKind::ReadyBytes => "ready event queue bytes",
        };
        write!(f, "SSE parser {what} exceeded cap {}", self.cap)
    }
}

impl std::error::Error for SseOverflow {}

/// One complete SSE event, as decoded by [`SseParser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Value of the `event:` field, or `"message"` if the event
    /// did not specify one (per spec default).
    pub event: String,
    /// Concatenated `data:` lines, joined with `\n`. Empty string if
    /// no `data:` field was present (rare but allowed by spec).
    pub data: String,
}

/// Stateful, push-style SSE parser. Feed bytes via [`feed`], drain
/// completed events via [`pop_event`]. Boundaries between
/// network chunks may fall anywhere — including in the middle of
/// a UTF-8 multi-byte sequence — so we accumulate raw bytes and
/// only `String`-decode complete lines.
///
/// All accumulating buffers are bounded; see [`MAX_LINE_BUFFER_BYTES`],
/// [`MAX_PENDING_LINES`], [`MAX_READY_BYTES`]. When any cap is hit
/// `feed` / `finish` return [`SseOverflow`] and the parser must be
/// discarded — further feeds will keep returning the same error.
///
/// [`feed`]: SseParser::feed
/// [`pop_event`]: SseParser::pop_event
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes seen but not yet consumed into a complete line.
    buffer: Vec<u8>,
    /// Lines collected so far for the in-progress event.
    pending_lines: Vec<String>,
    /// Completed events ready to be popped.
    ready: VecDeque<SseEvent>,
    /// Running total of bytes currently in `ready` (sum of event + data lens).
    ready_bytes: usize,
    /// Sticky overflow flag — once set, the parser refuses all further
    /// work and surfaces the original cause.
    errored: Option<SseOverflow>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes from the wire. Splits into lines on `\n`,
    /// emitting an event each time a blank line (i.e., `\n` after
    /// a previous `\n`) terminates the current accumulating event.
    /// Handles `\r\n` line endings too.
    ///
    /// Returns [`SseOverflow`] when any internal cap would be
    /// exceeded; the parser is then poisoned (subsequent calls
    /// return the same error) and must be discarded.
    pub fn feed(&mut self, chunk: &[u8]) -> std::result::Result<(), SseOverflow> {
        if let Some(e) = &self.errored {
            return Err(e.clone());
        }
        // Bound the in-progress line buffer BEFORE appending so we
        // don't briefly allocate above the cap.
        if self.buffer.len().saturating_add(chunk.len()) > MAX_LINE_BUFFER_BYTES {
            return self.poison(SseOverflowKind::LineBuffer, MAX_LINE_BUFFER_BYTES);
        }
        self.buffer.extend_from_slice(chunk);
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            // Slice [..pos] is one line, possibly with a trailing
            // \r. After we drain it we strip [..=pos] (line + \n).
            let line_bytes = &self.buffer[..pos];
            // Strip trailing \r if present.
            let line_bytes = if line_bytes.last() == Some(&b'\r') {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                line_bytes
            };
            // Decode lossily — SSE values are spec'd to be UTF-8
            // but we don't want to fail the stream on malformed
            // bytes (a chunk boundary mid-multibyte was already
            // handled above by buffering, so what's left here is
            // a complete line).
            let line = String::from_utf8_lossy(line_bytes).into_owned();
            self.consume_line(line)?;
            // Consume the \n we found.
            self.buffer.drain(..=pos);
        }
        Ok(())
    }

    /// Mark end-of-stream. Flushes any pending event that wasn't
    /// terminated by a blank line. The wire spec says streams end
    /// at a blank line; this is a forgiving final-flush helper for
    /// servers that close without one.
    pub fn finish(&mut self) -> std::result::Result<(), SseOverflow> {
        if let Some(e) = &self.errored {
            return Err(e.clone());
        }
        // If the buffer has trailing data (no newline), treat it as
        // a final unterminated line.
        if !self.buffer.is_empty() {
            let line_bytes = std::mem::take(&mut self.buffer);
            let line_bytes = if line_bytes.last() == Some(&b'\r') {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                &line_bytes[..]
            };
            let line = String::from_utf8_lossy(line_bytes).into_owned();
            self.consume_line(line)?;
        }
        if !self.pending_lines.is_empty() {
            self.dispatch_event()?;
        }
        Ok(())
    }

    /// Pop the next complete event, if one is ready.
    pub fn pop_event(&mut self) -> Option<SseEvent> {
        let ev = self.ready.pop_front();
        if let Some(e) = &ev {
            let cost = event_cost(e);
            self.ready_bytes = self.ready_bytes.saturating_sub(cost);
        }
        ev
    }

    /// Drain all complete events, in order.
    pub fn drain_events(&mut self) -> Vec<SseEvent> {
        self.ready_bytes = 0;
        self.ready.drain(..).collect()
    }

    fn consume_line(&mut self, line: String) -> std::result::Result<(), SseOverflow> {
        if line.is_empty() {
            // Blank line: dispatch event (if any pending fields).
            if !self.pending_lines.is_empty() {
                self.dispatch_event()?;
            }
            return Ok(());
        }
        // Comment line — starts with ':'.
        if line.starts_with(':') {
            return Ok(());
        }
        if self.pending_lines.len() >= MAX_PENDING_LINES {
            return self.poison(SseOverflowKind::PendingLines, MAX_PENDING_LINES);
        }
        self.pending_lines.push(line);
        Ok(())
    }

    fn dispatch_event(&mut self) -> std::result::Result<(), SseOverflow> {
        let mut event_name: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();
        for raw in self.pending_lines.drain(..) {
            let (field, value) = parse_field_line(&raw);
            // Per spec: strip a leading single space from value.
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => event_name = Some(value.to_string()),
                "data" => data_lines.push(value.to_string()),
                // id / retry / unknown — ignored
                _ => {}
            }
        }
        // Per spec: if no data lines, do not dispatch.
        if data_lines.is_empty() {
            return Ok(());
        }
        let event = SseEvent {
            event: event_name.unwrap_or_else(|| "message".to_string()),
            data: data_lines.join("\n"),
        };
        let cost = event_cost(&event);
        if self.ready_bytes.saturating_add(cost) > MAX_READY_BYTES {
            return self.poison(SseOverflowKind::ReadyBytes, MAX_READY_BYTES);
        }
        self.ready_bytes += cost;
        self.ready.push_back(event);
        Ok(())
    }

    fn poison(
        &mut self,
        kind: SseOverflowKind,
        cap: usize,
    ) -> std::result::Result<(), SseOverflow> {
        let err = SseOverflow { kind, cap };
        // Clear large buffers so we don't keep holding the memory.
        self.buffer.clear();
        self.pending_lines.clear();
        self.ready.clear();
        self.ready_bytes = 0;
        self.errored = Some(err.clone());
        Err(err)
    }
}

fn event_cost(e: &SseEvent) -> usize {
    e.event.len().saturating_add(e.data.len())
}

/// Split an SSE line into `(field, value)`. Per spec:
/// - `field: value` → `(field, value)`
/// - `field` (no colon) → `(field, "")`
/// - `field:` (colon, no value) → `(field, "")`
fn parse_field_line(line: &str) -> (&str, &str) {
    match line.find(':') {
        Some(idx) => (&line[..idx], &line[idx + 1..]),
        None => (line, ""),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/llm/sse.rs"
    ));
}
