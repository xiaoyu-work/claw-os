use super::*;
use crate::session::journal::event::{JournalEvent, Label};

fn event() -> JournalEvent {
    JournalEvent::ToolStarted {
        turn: 1,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("t-1"),
        known: true,
    }
}

fn preimage<'a>(event: &'a JournalEvent, seq: u64, prev: &'a str) -> Preimage<'a> {
    Preimage {
        schema: SCHEMA_VERSION,
        partition: "owner/1000",
        owner_uid: 1000,
        seq,
        epoch: 7,
        recorded_at_ms: 1_700_000_000_000,
        source: EventSource::Kernel,
        event,
        prev,
        key_id: "0011223344556677",
    }
}

fn record(event: JournalEvent, seq: u64, prev: &str, key: &[u8]) -> JournalRecord {
    let mac = preimage(&event, seq, prev).seal(key).unwrap();
    JournalRecord {
        v: SCHEMA_VERSION,
        seq,
        epoch: 7,
        recorded_at_ms: 1_700_000_000_000,
        partition: "owner/1000".to_string(),
        owner_uid: 1000,
        source: EventSource::Kernel,
        key_id: "0011223344556677".to_string(),
        prev: prev.to_string(),
        mac,
        event,
    }
}

#[test]
fn a_sealed_record_verifies_under_its_key() {
    let key = b"k".repeat(32);
    let record = record(event(), 1, GENESIS_MAC, &key);
    record.verify(&key).expect("verifies");
}

#[test]
fn the_encoding_is_unambiguous_across_field_boundaries() {
    // Length-prefixing is what stops one field's bytes being read as
    // the next field's: two different splits of the same characters
    // must not seal to the same MAC.
    let key = b"k".repeat(32);
    let event = event();
    let mut left = preimage(&event, 1, GENESIS_MAC);
    left.partition = "owner/10";
    let mut right = preimage(&event, 1, GENESIS_MAC);
    right.partition = "owner/1";
    assert_ne!(left.seal(&key).unwrap(), right.seal(&key).unwrap());
}

#[test]
fn a_body_change_breaks_the_mac() {
    let key = b"k".repeat(32);
    let mut record = record(event(), 1, GENESIS_MAC, &key);
    record.event = JournalEvent::ToolStarted {
        turn: 2,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("t-1"),
        known: true,
    };
    assert!(
        record.verify(&key).is_err(),
        "a tampered body must not verify"
    );
}

#[test]
fn a_sequence_change_breaks_the_mac() {
    let key = b"k".repeat(32);
    let mut record = record(event(), 1, GENESIS_MAC, &key);
    record.seq = 2;
    assert!(record.verify(&key).is_err());
}

#[test]
fn an_epoch_change_breaks_the_mac() {
    let key = b"k".repeat(32);
    let mut record = record(event(), 1, GENESIS_MAC, &key);
    record.epoch = 8;
    assert!(record.verify(&key).is_err());
}

#[test]
fn a_source_change_breaks_the_mac() {
    // Re-labelling a worker record as a kernel one must not verify,
    // or the ACL could be bypassed after the fact.
    let key = b"k".repeat(32);
    let mut record = record(event(), 1, GENESIS_MAC, &key);
    record.source = EventSource::Worker;
    assert!(record.verify(&key).is_err());
}

#[test]
fn a_different_key_does_not_verify() {
    let key = b"k".repeat(32);
    let record = record(event(), 1, GENESIS_MAC, &key);
    assert!(record.verify(&b"j".repeat(32)).is_err());
}

#[test]
fn an_unknown_schema_version_is_refused() {
    let key = b"k".repeat(32);
    let mut record = record(event(), 1, GENESIS_MAC, &key);
    record.v = SCHEMA_VERSION + 1;
    let error = record.verify(&key).expect_err("unknown schema");
    assert!(error.to_string().contains("declares schema"));
}

#[test]
fn a_decorated_line_is_refused_outright() {
    let key = b"k".repeat(32);
    let record = record(event(), 1, GENESIS_MAC, &key);
    let mut line: serde_json::Value = serde_json::from_str(&record.encode_line().unwrap()).unwrap();
    line.as_object_mut()
        .unwrap()
        .insert("smuggled".to_string(), serde_json::json!("payload"));
    let decoded = JournalRecord::decode_line(&line.to_string());
    assert!(
        decoded.is_err(),
        "an extra top-level key must not decode into a record"
    );
}

#[test]
fn an_oversized_line_is_refused_before_it_is_parsed() {
    let line = "x".repeat(MAX_RECORD_BYTES + 1);
    assert!(JournalRecord::decode_line(&line).is_err());
}
