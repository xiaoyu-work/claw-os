use super::*;

#[test]
fn typed_protocol_event_builds_an_sse_frame() {
    let _event = protocol_event(StreamEvent::Delta(DeltaPayload::new("hello")));
}
