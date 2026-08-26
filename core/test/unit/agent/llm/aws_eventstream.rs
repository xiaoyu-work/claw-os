use super::*;

impl EventStreamParser {
    fn pending_bytes(&self) -> usize {
        self.buf.len()
    }
}

fn good_chunk_frame() -> Vec<u8> {
    encode_frame(
        &[
            (":message-type", "event"),
            (":event-type", "chunk"),
            (":content-type", "application/json"),
        ],
        br#"{"bytes":"hello"}"#,
    )
}

#[test]
fn round_trip_one_frame() {
    let frame = good_chunk_frame();
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let f = p.pop_frame().unwrap().unwrap();
    assert_eq!(f.headers.get(":message-type").unwrap(), "event");
    assert_eq!(f.headers.get(":event-type").unwrap(), "chunk");
    assert_eq!(f.payload, br#"{"bytes":"hello"}"#);
    // No more frames.
    assert!(p.pop_frame().is_none());
    assert!(p.finish().is_ok());
}

#[test]
fn round_trip_three_frames_back_to_back() {
    let mut all = Vec::new();
    all.extend_from_slice(&good_chunk_frame());
    all.extend_from_slice(&good_chunk_frame());
    all.extend_from_slice(&good_chunk_frame());
    let mut p = EventStreamParser::new();
    p.feed(&all);
    for _ in 0..3 {
        let f = p.pop_frame().unwrap().unwrap();
        assert_eq!(f.headers.get(":event-type").unwrap(), "chunk");
    }
    assert!(p.pop_frame().is_none());
}

#[test]
fn handles_byte_by_byte_chunking() {
    let frame = good_chunk_frame();
    let mut p = EventStreamParser::new();
    // Feed every byte individually. Should not yield anything
    // until the whole frame is in.
    let mut yielded = Vec::new();
    for byte in &frame {
        p.feed(&[*byte]);
        while let Some(res) = p.pop_frame() {
            yielded.push(res);
        }
    }
    assert_eq!(yielded.len(), 1);
    let f = yielded.pop().unwrap().unwrap();
    assert_eq!(f.payload, br#"{"bytes":"hello"}"#);
}

#[test]
fn pop_frame_returns_none_when_buf_smaller_than_prelude() {
    let mut p = EventStreamParser::new();
    p.feed(&[0u8; 3]);
    assert!(p.pop_frame().is_none());
    assert_eq!(p.pending_bytes(), 3);
}

#[test]
fn pop_frame_waits_for_full_payload() {
    let frame = good_chunk_frame();
    let mut p = EventStreamParser::new();
    // Feed everything except the last byte.
    p.feed(&frame[..frame.len() - 1]);
    assert!(p.pop_frame().is_none());
    assert_eq!(p.pending_bytes(), frame.len() - 1);
    // Feed the last byte → frame pops out.
    p.feed(&frame[frame.len() - 1..]);
    let f = p.pop_frame().unwrap().unwrap();
    assert_eq!(f.headers.get(":event-type").unwrap(), "chunk");
    assert!(p.pop_frame().is_none());
}

#[test]
fn bad_prelude_crc_terminates_stream() {
    let mut frame = good_chunk_frame();
    // Corrupt one byte of the prelude CRC.
    frame[10] ^= 0xff;
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(err, FrameError::PreludeCrc { .. }));
    // Sticky: subsequent calls return the same error.
    let again = p.pop_frame().unwrap().unwrap_err();
    assert_eq!(err, again);
}

#[test]
fn bad_message_crc_terminates_stream() {
    let mut frame = good_chunk_frame();
    // Corrupt one byte of the trailer.
    let n = frame.len();
    frame[n - 2] ^= 0xff;
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(err, FrameError::MessageCrc { .. }));
}

#[test]
fn structurally_invalid_total_len_zero_rejected() {
    // Build a fake prelude with total_len = 0, headers_len = 0,
    // and a (now-meaningless) prelude crc that matches the
    // first 8 zero bytes. We want to assert structural check
    // fires before crc check, OR even if crc check fires first,
    // both result in a fatal error.
    let mut buf = Vec::with_capacity(12);
    buf.extend_from_slice(&[0u8; 8]);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_be_bytes());
    let mut p = EventStreamParser::new();
    p.feed(&buf);
    let err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(err, FrameError::BadStructure { .. }));
}

#[test]
fn structurally_invalid_headers_overflow_rejected() {
    // total_len = 16 (== MIN_FRAME_LEN), headers_len = 17 → overflow.
    let mut prelude_first = Vec::with_capacity(8);
    prelude_first.extend_from_slice(&16u32.to_be_bytes());
    prelude_first.extend_from_slice(&17u32.to_be_bytes());
    let crc = crc32(&prelude_first);
    let mut full = prelude_first;
    full.extend_from_slice(&crc.to_be_bytes());
    // pad out to total_len so we don't hit the "wait for more
    // bytes" branch
    full.extend(vec![0u8; 4]);
    let mut p = EventStreamParser::new();
    p.feed(&full);
    let err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(err, FrameError::BadStructure { .. }));
}

#[test]
fn finish_with_empty_buffer_is_ok() {
    let p = EventStreamParser::new();
    assert!(p.finish().is_ok());
}

#[test]
fn finish_with_partial_frame_reports_truncation() {
    let frame = good_chunk_frame();
    let mut p = EventStreamParser::new();
    // Feed only the prelude.
    p.feed(&frame[..PRELUDE_LEN]);
    assert!(p.pop_frame().is_none());
    let err = p.finish().unwrap_err();
    assert!(matches!(err, FrameError::Truncated(n) if n == PRELUDE_LEN));
}

#[test]
fn unsupported_header_value_type_is_fatal() {
    // Build a frame whose single header has value_type 9 (timestamp).
    let mut headers = Vec::new();
    let name = ":content-type";
    headers.push(name.len() as u8);
    headers.extend_from_slice(name.as_bytes());
    headers.push(9); // unsupported
                     // 8 more bytes — type 9 is timestamp (u64); we just stuff zeros
                     // because parsing should bail out.
    headers.extend_from_slice(&[0u8; 8]);

    let payload = b"";
    let total_len = (PRELUDE_LEN + headers.len() + payload.len() + TRAILER_LEN) as u32;
    let mut prelude_first = Vec::with_capacity(8);
    prelude_first.extend_from_slice(&total_len.to_be_bytes());
    prelude_first.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&prelude_first);
    let mut frame = prelude_first;
    frame.extend_from_slice(&prelude_crc.to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(payload);
    let msg_crc = crc32(&frame);
    frame.extend_from_slice(&msg_crc.to_be_bytes());

    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(err, FrameError::UnsupportedHeaderType { .. }));
}

#[test]
fn header_with_zero_length_value_round_trips() {
    let frame = encode_frame(&[(":message-type", "")], b"x");
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let f = p.pop_frame().unwrap().unwrap();
    assert_eq!(f.headers.get(":message-type").unwrap(), "");
    assert_eq!(f.payload, b"x");
}

#[test]
fn frame_with_no_headers_round_trips() {
    let frame = encode_frame(&[], b"raw payload");
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let f = p.pop_frame().unwrap().unwrap();
    assert!(f.headers.is_empty());
    assert_eq!(f.payload, b"raw payload");
}

#[test]
fn frame_with_empty_payload_round_trips() {
    let frame = encode_frame(&[(":event-type", "ping")], b"");
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let f = p.pop_frame().unwrap().unwrap();
    assert_eq!(f.headers.get(":event-type").unwrap(), "ping");
    assert!(f.payload.is_empty());
}

#[test]
fn fatal_error_persists_across_subsequent_feeds() {
    let mut frame = good_chunk_frame();
    frame[10] ^= 0xff;
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let _ = p.pop_frame().unwrap().unwrap_err();
    // Feeding more bytes is a no-op once poisoned.
    p.feed(&good_chunk_frame());
    let still_err = p.pop_frame().unwrap().unwrap_err();
    assert!(matches!(still_err, FrameError::PreludeCrc { .. }));
    // finish() also reports the fatal.
    let err = p.finish().unwrap_err();
    assert!(matches!(err, FrameError::PreludeCrc { .. }));
}

#[test]
fn encode_frame_helper_round_trips_full_table() {
    // Every header type currently used by Bedrock.
    let frame = encode_frame(
        &[
            (":message-type", "event"),
            (":event-type", "chunk"),
            (":content-type", "application/json"),
        ],
        b"{\"bytes\":\"YWJjZA==\"}",
    );
    let mut p = EventStreamParser::new();
    p.feed(&frame);
    let f = p.pop_frame().unwrap().unwrap();
    assert_eq!(f.headers.len(), 3);
    assert_eq!(f.headers.get(":message-type").unwrap(), "event");
    assert_eq!(f.headers.get(":event-type").unwrap(), "chunk");
    assert_eq!(f.headers.get(":content-type").unwrap(), "application/json");
    assert_eq!(f.payload, b"{\"bytes\":\"YWJjZA==\"}");
}
