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
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append bytes from the wire. Splits into lines on `\n`,
    /// emitting an event each time a blank line (i.e., `\n` after
    /// a previous `\n`) terminates the current accumulating event.
    /// Handles `\r\n` line endings too.
    pub fn feed(&mut self, chunk: &[u8]) {
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
            self.consume_line(line);
            // Consume the \n we found.
            self.buffer.drain(..=pos);
        }
    }

    /// Mark end-of-stream. Flushes any pending event that wasn't
    /// terminated by a blank line. The wire spec says streams end
    /// at a blank line; this is a forgiving final-flush helper for
    /// servers that close without one.
    pub fn finish(&mut self) {
        // If the buffer has trailing data (no newline), treat it as
        // a final unterminated line.
        if !self.buffer.is_empty() {
            let line_bytes: Vec<u8> = self.buffer.drain(..).collect();
            let line_bytes = if line_bytes.last() == Some(&b'\r') {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                &line_bytes[..]
            };
            let line = String::from_utf8_lossy(line_bytes).into_owned();
            self.consume_line(line);
        }
        if !self.pending_lines.is_empty() {
            self.dispatch_event();
        }
    }

    /// Pop the next complete event, if one is ready.
    pub fn pop_event(&mut self) -> Option<SseEvent> {
        self.ready.pop_front()
    }

    /// Drain all complete events, in order.
    pub fn drain_events(&mut self) -> Vec<SseEvent> {
        self.ready.drain(..).collect()
    }

    fn consume_line(&mut self, line: String) {
        if line.is_empty() {
            // Blank line: dispatch event (if any pending fields).
            if !self.pending_lines.is_empty() {
                self.dispatch_event();
            }
            return;
        }
        // Comment line — starts with ':'.
        if line.starts_with(':') {
            return;
        }
        self.pending_lines.push(line);
    }

    fn dispatch_event(&mut self) {
        let mut event_name: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();
        for raw in self.pending_lines.drain(..) {
            let (field, value) = parse_field_line(&raw);
            // Per spec: strip a leading single space from value.
            let value = if value.starts_with(' ') {
                &value[1..]
            } else {
                value
            };
            match field {
                "event" => event_name = Some(value.to_string()),
                "data" => data_lines.push(value.to_string()),
                // id / retry / unknown — ignored
                _ => {}
            }
        }
        // Per spec: if no data lines, do not dispatch.
        if data_lines.is_empty() {
            return;
        }
        self.ready.push_back(SseEvent {
            event: event_name.unwrap_or_else(|| "message".to_string()),
            data: data_lines.join("\n"),
        });
    }
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
    use super::*;

    fn one_chunk(input: &[u8]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        p.feed(input);
        p.drain_events()
    }

    #[test]
    fn empty_input_yields_no_events() {
        let p = SseParser::new();
        assert!(p.ready.is_empty());
    }

    #[test]
    fn simple_event_decodes() {
        let events = one_chunk(b"event: ping\ndata: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn missing_event_field_defaults_to_message() {
        let events = one_chunk(b"data: hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "message");
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn multiple_data_lines_join_with_newline() {
        let events = one_chunk(b"data: line1\ndata: line2\ndata: line3\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2\nline3");
    }

    #[test]
    fn comment_lines_are_ignored() {
        let events = one_chunk(b": this is a comment\ndata: hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn id_and_retry_fields_are_ignored() {
        let events = one_chunk(b"id: 42\nretry: 5000\nevent: foo\ndata: bar\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "foo");
        assert_eq!(events[0].data, "bar");
    }

    #[test]
    fn event_without_data_is_dropped() {
        let events = one_chunk(b"event: orphan\n\n");
        assert!(events.is_empty(), "events without data should be dropped");
    }

    #[test]
    fn crlf_line_endings_handled() {
        let events = one_chunk(b"event: ping\r\ndata: hello\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn multiple_events_in_one_chunk() {
        let input = b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\ndata: 3\n\n";
        let events = one_chunk(input);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events.iter().map(|e| e.event.clone()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            events.iter().map(|e| e.data.clone()).collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
    }

    #[test]
    fn split_across_chunk_boundaries_byte_by_byte() {
        // Feed one byte at a time — every conceivable boundary.
        let input = b"event: msg\ndata: hello\n\nevent: end\ndata: bye\n\n";
        let mut p = SseParser::new();
        for b in input {
            p.feed(&[*b]);
        }
        let events = p.drain_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "msg");
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[1].event, "end");
        assert_eq!(events[1].data, "bye");
    }

    #[test]
    fn split_in_middle_of_field_name() {
        let mut p = SseParser::new();
        p.feed(b"eve");
        p.feed(b"nt: ping\nda");
        p.feed(b"ta: hi\n\n");
        let events = p.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn split_in_middle_of_blank_separator() {
        let mut p = SseParser::new();
        p.feed(b"event: a\ndata: 1\n");
        // First event terminator arrives split across 2 feeds.
        p.feed(b"\n");
        p.feed(b"event: b\ndata: 2\n\n");
        let events = p.drain_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "1");
        assert_eq!(events[1].data, "2");
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        p.feed(b"event: end\ndata: bye\n");
        // No blank line before EOF.
        p.finish();
        let events = p.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "bye");
    }

    #[test]
    fn finish_flushes_unterminated_partial_line() {
        let mut p = SseParser::new();
        p.feed(b"event: end\ndata: bye");
        p.finish();
        let events = p.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "bye");
    }

    #[test]
    fn json_payload_is_passed_through_intact() {
        let json = r#"{"type":"content_block_delta","delta":{"text":"hi"}}"#;
        let mut input = Vec::new();
        input.extend_from_slice(b"event: content_block_delta\ndata: ");
        input.extend_from_slice(json.as_bytes());
        input.extend_from_slice(b"\n\n");
        let events = one_chunk(&input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "content_block_delta");
        assert_eq!(events[0].data, json);
    }

    #[test]
    fn pop_event_drains_in_fifo_order() {
        let mut p = SseParser::new();
        p.feed(b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n");
        let first = p.pop_event().unwrap();
        let second = p.pop_event().unwrap();
        assert_eq!(first.event, "a");
        assert_eq!(second.event, "b");
        assert!(p.pop_event().is_none());
    }

    #[test]
    fn field_without_colon_treated_as_field_with_empty_value() {
        // Per spec: `field` (no colon) is field=line, value="".
        // event=hello → set event_name to "" — but data is empty,
        // so the event is dropped per spec.
        let events = one_chunk(b"event\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn unknown_field_is_silently_dropped() {
        let events = one_chunk(b"future_field: xyz\nevent: ping\ndata: hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
    }

    #[test]
    fn leading_single_space_stripped_from_value() {
        // Spec: strip a leading single space (not multiple).
        let events = one_chunk(b"data:  with-leading-space\n\n");
        assert_eq!(events.len(), 1);
        // First space stripped, second preserved.
        assert_eq!(events[0].data, " with-leading-space");
    }

    #[test]
    fn no_leading_space_works() {
        let events = one_chunk(b"data:no-space\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "no-space");
    }

    #[test]
    fn malformed_utf8_does_not_panic() {
        // Lone 0xFF in middle of stream: lossy decoding replaces it
        // with U+FFFD but we keep going.
        let mut input = Vec::new();
        input.extend_from_slice(b"event: ping\ndata: ");
        input.push(0xFFu8);
        input.extend_from_slice(b"hello\n\n");
        let events = one_chunk(&input);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
        assert!(events[0].data.contains("hello"));
    }

    #[test]
    fn parse_field_line_handles_no_colon() {
        let (f, v) = parse_field_line("event");
        assert_eq!(f, "event");
        assert_eq!(v, "");
    }

    #[test]
    fn parse_field_line_handles_colon_only() {
        let (f, v) = parse_field_line("event:");
        assert_eq!(f, "event");
        assert_eq!(v, "");
    }

    #[test]
    fn parse_field_line_first_colon_is_separator() {
        // A second colon belongs to the value.
        let (f, v) = parse_field_line("data: hello: world");
        assert_eq!(f, "data");
        assert_eq!(v, " hello: world");
    }
}
