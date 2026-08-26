use super::*;

fn one_chunk(input: &[u8]) -> Vec<SseEvent> {
    let mut p = SseParser::new();
    p.feed(input).expect("test input fits in caps");
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
        p.feed(&[*b]).expect("fits in caps");
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
    p.feed(b"eve").expect("fits in caps");
    p.feed(b"nt: ping\nda").expect("fits in caps");
    p.feed(b"ta: hi\n\n").expect("fits in caps");
    let events = p.drain_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "ping");
    assert_eq!(events[0].data, "hi");
}

#[test]
fn split_in_middle_of_blank_separator() {
    let mut p = SseParser::new();
    p.feed(b"event: a\ndata: 1\n").expect("fits in caps");
    // First event terminator arrives split across 2 feeds.
    p.feed(b"\n").expect("fits in caps");
    p.feed(b"event: b\ndata: 2\n\n").expect("fits in caps");
    let events = p.drain_events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data, "1");
    assert_eq!(events[1].data, "2");
}

#[test]
fn finish_flushes_unterminated_event() {
    let mut p = SseParser::new();
    p.feed(b"event: end\ndata: bye\n").expect("fits in caps");
    // No blank line before EOF.
    p.finish().expect("fits in caps");
    let events = p.drain_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "bye");
}

#[test]
fn finish_flushes_unterminated_partial_line() {
    let mut p = SseParser::new();
    p.feed(b"event: end\ndata: bye").expect("fits in caps");
    p.finish().expect("fits in caps");
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
    p.feed(b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n").expect("fits in caps");
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

/// A hostile upstream that streams a single unterminated multi-MiB
/// line must NOT cause us to allocate without bound. We surface
/// an `SseOverflow` and the caller stops the stream.
#[test]
fn oversized_buffer_errors() {
    let mut p = SseParser::new();
    // Feed up to just under the cap — still happy.
    let chunk = vec![b'x'; MAX_LINE_BUFFER_BYTES / 2];
    p.feed(&chunk).expect("first half OK");
    // Second chunk pushes us past the cap → must error.
    let chunk2 = vec![b'y'; MAX_LINE_BUFFER_BYTES / 2 + 100];
    let err = p.feed(&chunk2).expect_err("second half must overflow");
    assert_eq!(err.kind, SseOverflowKind::LineBuffer);
    // Parser is poisoned: subsequent feeds return the same error.
    let err2 = p.feed(b"more").expect_err("poisoned parser stays errored");
    assert_eq!(err, err2);
}

/// Too many lines per event (no terminating blank line) must also
/// surface as overflow, not OOM.
#[test]
fn oversized_pending_lines_errors() {
    let mut p = SseParser::new();
    // Each "data: x\n" line is 8 bytes, so MAX_PENDING_LINES of
    // them fits in the line-buffer cap.
    let mut buf = Vec::with_capacity(8 * (MAX_PENDING_LINES + 10));
    for _ in 0..(MAX_PENDING_LINES + 10) {
        buf.extend_from_slice(b"data: x\n");
    }
    // Note no trailing blank line — they all accumulate in
    // pending_lines until the cap kicks in.
    let err = p.feed(&buf).expect_err("must overflow pending lines");
    assert_eq!(err.kind, SseOverflowKind::PendingLines);
}

/// Too many ready events queued must surface as overflow.
#[test]
fn oversized_ready_queue_errors() {
    // One event of ~512 KiB. 130 such events ≈ 65 MiB > cap.
    let mut p = SseParser::new();
    let payload = "y".repeat(512 * 1024);
    let mut total_err: Option<SseOverflow> = None;
    for _ in 0..130 {
        let mut chunk = Vec::with_capacity(payload.len() + 16);
        chunk.extend_from_slice(b"data: ");
        chunk.extend_from_slice(payload.as_bytes());
        chunk.extend_from_slice(b"\n\n");
        if let Err(e) = p.feed(&chunk) {
            total_err = Some(e);
            break;
        }
    }
    let e = total_err.expect("ready queue should have overflowed");
    assert_eq!(e.kind, SseOverflowKind::ReadyBytes);
}
