use super::*;
use crate::session::journal::harness::{probe, Harness};

fn chain(harness: &Harness) -> Chain {
    let lease = harness.lease();
    read(
        &harness.root(),
        &harness.partition(),
        harness.owner_uid(),
        lease.keyring(),
    )
    .expect("read")
}

#[test]
fn an_untouched_chain_verifies() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    let chain = chain(&harness);
    assert!(chain.health.is_verified(), "{:?}", chain.health);
    assert_eq!(chain.records.len(), 2);
    assert_eq!(chain.records[0].seq, 1);
    assert_eq!(chain.records[1].prev, chain.records[0].mac);
}

#[test]
fn an_empty_partition_verifies() {
    let harness = Harness::new();
    let chain = chain(&harness);
    assert!(chain.health.is_verified());
    assert!(chain.records.is_empty());
}

#[test]
fn a_swapped_record_body_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    let mut lines = harness.lines();
    let mut record: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    record["event"]["turn"] = serde_json::json!(99);
    lines[1] = record.to_string();
    std::fs::write(harness.active_path(), format!("{}\n", lines.join("\n"))).unwrap();

    let chain = chain(&harness);
    assert!(chain.health.is_damaged(), "{:?}", chain.health);
}

#[test]
fn a_reordered_chain_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    let lines = harness.lines();
    std::fs::write(
        harness.active_path(),
        format!("{}\n{}\n", lines[1], lines[0]),
    )
    .unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::Damaged { ref detail } => {
            assert!(detail.contains("expected seq"), "{detail}")
        }
        other => panic!("expected damage, got {other:?}"),
    }
}

#[test]
fn an_injected_record_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));

    let lines = harness.lines();
    // Re-signing is impossible without the root-only key, so the best
    // an attacker can do is move a record the daemon already signed.
    // Both records are the same width, so this keeps the committed byte
    // count intact and the chain check is what catches it.
    assert_eq!(lines[0].len(), lines[1].len());
    std::fs::write(
        harness.active_path(),
        format!("{}\n{}\n", lines[0], lines[0]),
    )
    .unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::Damaged { ref detail } => assert!(detail.contains("expected seq"), "{detail}"),
        other => panic!("expected damage, got {other:?}"),
    }
    assert_eq!(
        chain.records.len(),
        1,
        "only the verified prefix is returned"
    );
}

#[test]
fn a_chain_shorter_than_its_head_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));
    harness.append(probe(2));
    std::fs::write(harness.active_path(), "").unwrap();

    let chain = chain(&harness);
    assert!(chain.health.is_damaged(), "{:?}", chain.health);
}

#[test]
fn bytes_past_the_head_are_reported_as_an_uncommitted_tail() {
    let harness = Harness::new();
    harness.append(probe(1));
    let mut data = std::fs::read(harness.active_path()).unwrap();
    data.extend_from_slice(b"{\"v\":1,\"seq\":2\n");
    std::fs::write(harness.active_path(), data).unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::UncommittedTail { head_seq, .. } => assert_eq!(head_seq, 1),
        other => panic!("expected an uncommitted tail, got {other:?}"),
    }
    assert_eq!(
        chain.records.len(),
        1,
        "only committed records are returned"
    );
}

#[test]
fn a_rewritten_head_is_reported_rather_than_believed() {
    let harness = Harness::new();
    harness.append(probe(1));

    let mut anchor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(harness.anchor_path()).unwrap()).unwrap();
    anchor["seq"] = serde_json::json!(99);
    std::fs::write(harness.anchor_path(), anchor.to_string()).unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::Damaged { ref detail } => assert!(detail.contains("MAC"), "{detail}"),
        other => panic!("expected damage, got {other:?}"),
    }
}

#[test]
fn a_missing_head_over_committed_bytes_is_damage_not_a_fresh_partition() {
    let harness = Harness::new();
    harness.append(probe(1));
    let chain_path = harness.active_path();
    let bytes = std::fs::read(&chain_path).unwrap();
    std::fs::remove_file(harness.anchor_path()).unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::Damaged { ref detail } => {
            assert!(detail.contains("committed head is missing"), "{detail}")
        }
        other => panic!("expected damage, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&chain_path).unwrap(),
        bytes,
        "a read must never erase the evidence it could not verify"
    );
}

#[test]
fn a_record_signed_with_an_unknown_key_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));

    let mut lines = harness.lines();
    let mut record: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    record["key_id"] = serde_json::json!("ffffffffffffffff");
    lines[0] = record.to_string();
    std::fs::write(harness.active_path(), format!("{}\n", lines[0])).unwrap();

    let chain = chain(&harness);
    match chain.health {
        Health::Damaged { ref detail } => assert!(detail.contains("does not hold"), "{detail}"),
        other => panic!("expected damage, got {other:?}"),
    }
}

#[test]
fn a_gap_between_segments_is_reported() {
    let harness = Harness::new();
    harness.append(probe(1));
    super::super::rotate(
        &harness.lease(),
        &harness.partition(),
        harness.owner_uid(),
        super::super::RetentionReason::SizeRotation,
    )
    .expect("rotate");
    harness.append(probe(2));

    // Drop the archived segment without telling the head: the chain no
    // longer starts where the anchor says it does.
    std::fs::remove_file(harness.segment_path(0)).unwrap();

    let chain = chain(&harness);
    assert!(
        chain.health.is_damaged(),
        "a missing archived segment must not read as a clean chain: {:?}",
        chain.health
    );
}
