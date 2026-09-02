use super::*;

fn sid() -> SessionId {
    SessionId::generate()
}

#[test]
fn keys_are_derived_and_round_trip() {
    let session = Partition::Session(sid());
    let owner = Partition::Owner(1000);
    assert!(session.key().starts_with("session/ses_"));
    assert_eq!(owner.key(), "owner/1000");
    assert_eq!(Partition::parse(&session.key()), Some(session));
    assert_eq!(Partition::parse(&owner.key()), Some(owner));
}

#[test]
fn a_key_that_was_not_derived_here_does_not_parse() {
    // The name is never taken from a caller, but parsing is what a
    // reader does with a stored record, so traversal must not survive.
    assert!(Partition::parse("session/../../etc").is_none());
    assert!(Partition::parse("owner/root").is_none());
    assert!(Partition::parse("../escape").is_none());
    assert!(Partition::parse("session/").is_none());
}

#[test]
fn paths_stay_under_the_journal_root() {
    let root = std::path::Path::new("/var/lib/cos/journal");
    let partition = Partition::Owner(1000);
    for path in [
        partition.segment_path(root, 0),
        partition.segment_path(root, 7),
        partition.anchor_path(root),
        partition.lock_path(root),
        partition.segments_dir(root),
    ] {
        assert!(
            path.starts_with(root),
            "{} escaped the root",
            path.display()
        );
    }
}

#[test]
fn segments_sort_in_chain_order() {
    let tmp = tempfile::tempdir().unwrap();
    let partition = Partition::Owner(1000);
    std::fs::create_dir_all(partition.segments_dir(tmp.path())).unwrap();
    for index in [2u64, 0, 11] {
        std::fs::write(partition.segment_path(tmp.path(), index), b"x").unwrap();
    }
    assert_eq!(partition.segments(tmp.path()).unwrap(), vec![0, 2, 11]);
}

#[test]
fn an_unusable_segment_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let partition = Partition::Owner(1000);
    std::fs::create_dir_all(partition.segments_dir(tmp.path())).unwrap();
    std::fs::write(partition.segments_dir(tmp.path()).join("nope.jsonl"), b"x").unwrap();
    assert!(partition.segments(tmp.path()).is_err());
}

#[test]
fn chain_bytes_tell_a_new_partition_from_a_deleted_head() {
    let tmp = tempfile::tempdir().unwrap();
    let partition = Partition::Owner(1000);
    assert!(!partition.has_chain_bytes(tmp.path()).unwrap());

    std::fs::create_dir_all(partition.segments_dir(tmp.path())).unwrap();
    std::fs::write(partition.segment_path(tmp.path(), 0), b"").unwrap();
    assert!(
        !partition.has_chain_bytes(tmp.path()).unwrap(),
        "an empty segment is not committed data"
    );

    std::fs::write(partition.segment_path(tmp.path(), 0), b"{}\n").unwrap();
    assert!(partition.has_chain_bytes(tmp.path()).unwrap());
}

#[test]
fn listing_finds_both_partition_families() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Partition::Session(sid());
    let owner = Partition::Owner(4242);
    std::fs::create_dir_all(session.dir(tmp.path())).unwrap();
    std::fs::create_dir_all(owner.dir(tmp.path())).unwrap();
    std::fs::create_dir_all(tmp.path().join("owners").join("not-a-uid")).unwrap();

    let found = list(tmp.path()).expect("list");
    assert!(found.contains(&session));
    assert!(found.contains(&owner));
    assert_eq!(found.len(), 2, "an unparsable directory must be ignored");
}

#[test]
fn an_anchor_verifies_only_under_the_key_that_sealed_it() {
    let partition = Partition::Owner(1000);
    let mut anchor = Anchor::empty(&partition, 1000, "0011223344556677");
    anchor.seq = 3;
    anchor.active_bytes = 120;
    anchor.seal(b"key-a");
    anchor.verify(b"key-a").expect("verifies");
    assert!(anchor.verify(b"key-b").is_err());
}

#[test]
fn rewriting_the_head_breaks_the_anchor_mac() {
    // Truncating the chain and moving the head back is the attack this
    // MAC exists for.
    let partition = Partition::Owner(1000);
    let mut anchor = Anchor::empty(&partition, 1000, "0011223344556677");
    anchor.seq = 9;
    anchor.active_bytes = 900;
    anchor.seal(b"key");
    anchor.seq = 4;
    anchor.active_bytes = 400;
    assert!(anchor.verify(b"key").is_err());
}

#[test]
fn every_accounting_field_is_covered_by_the_anchor_mac() {
    let partition = Partition::Owner(1000);
    let base = {
        let mut anchor = Anchor::empty(&partition, 1000, "0011223344556677");
        anchor.seq = 5;
        anchor.events = 5;
        anchor.total_bytes = 500;
        anchor.active_bytes = 500;
        anchor.open_brackets = 2;
        anchor.control_events = 3;
        anchor.worker_events = 1;
        anchor.ingest_events = 1;
        anchor.closure_events = 0;
        anchor.active_index = 1;
        anchor.active_first_seq = 4;
        anchor.first_seq = 1;
        anchor.seal(b"key");
        anchor
    };
    base.verify(b"key").expect("baseline verifies");

    let mutations: Vec<Box<dyn Fn(&mut Anchor)>> = vec![
        Box::new(|a| a.open_brackets = 0),
        Box::new(|a| a.worker_events = 0),
        Box::new(|a| a.control_events = 0),
        Box::new(|a| a.ingest_events = 0),
        Box::new(|a| a.closure_events = 9),
        Box::new(|a| a.total_bytes = 0),
        Box::new(|a| a.active_bytes = 0),
        Box::new(|a| a.active_index = 0),
        Box::new(|a| a.active_first_seq = 1),
        Box::new(|a| a.first_seq = 0),
        Box::new(|a| a.first_prev_mac = "f".repeat(64)),
        Box::new(|a| a.active_prev_mac = "f".repeat(64)),
        Box::new(|a| a.events = 0),
        Box::new(|a| {
            a.pending_retention = Some(PendingRetention {
                segment_index: 0,
                retained_from_seq: 6,
                archive: ContentRef::of(super::super::event::ContentStore::SessionTurns, b"x"),
            })
        }),
    ];
    for mutate in mutations {
        let mut tampered = base.clone();
        mutate(&mut tampered);
        assert!(
            tampered.verify(b"key").is_err(),
            "an accounting field must not be rewritable without the key"
        );
    }
}
