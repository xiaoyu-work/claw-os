use super::*;

#[test]
fn nearby_interruption_preserves_completed_results_and_never_replays_work() {
    let mut view = super::super::tests::fixture();
    let session = view.session.as_mut().unwrap();
    for phase in [TransferPhase::Sending, TransferPhase::Delivered] {
        session.transfers.push(NearbyTransfer {
            id: uuid::Uuid::new_v4().to_string(),
            incoming: false,
            peer: "Unverified receiver".into(),
            files: vec![SharedFile {
                name: "a.txt".into(),
                bytes: Some(3),
            }],
            phase,
            bytes: 0,
            total_bytes: None,
            received_paths: Vec::new(),
            message: None,
        });
    }
    end(session, SessionPhase::Interrupted, "Backend interrupted");
    assert_eq!(session.transfers[0].phase, TransferPhase::Interrupted);
    assert_eq!(session.transfers[1].phase, TransferPhase::Delivered);
    assert!(session.peers.is_empty());
    assert!(!session.discovering);
}

#[test]
fn nearby_discovery_rejects_non_link_local_or_foreign_interface_peers() {
    let store = Store {
        root: std::path::PathBuf::from("/unused"),
        owner: 1000,
        home: std::path::PathBuf::from("/home/user"),
    };
    let mut view = super::super::tests::fixture();
    view.session.as_mut().unwrap().discovering = true;
    let mut targets = HashMap::new();
    let mut pending = HashMap::new();
    for address in [
        "[::1]:8770",
        "[2001:db8::1%7]:8770",
        "[fe80::2%8]:8770",
        "[fe80::2%7]:0",
    ] {
        assert!(
            apply_event(
                &store,
                &mut view,
                WorkerEvent::Peer {
                    address: address.into(),
                    name: "Phone".into(),
                    flags: 0
                },
                &mut targets,
                &mut pending,
                7
            )
            .is_err(),
            "{address}"
        );
    }
    apply_event(
        &store,
        &mut view,
        WorkerEvent::Peer {
            address: "[fe80::2%7]:8770".into(),
            name: "Phone".into(),
            flags: 0,
        },
        &mut targets,
        &mut pending,
        7,
    )
    .unwrap();
    assert_eq!(view.session.as_ref().unwrap().peers.len(), 1);
    assert_eq!(targets.len(), 1);
}

#[test]
fn nearby_receive_checks_serialized_capacity_before_accepting_any_bytes() {
    let store = Store {
        root: std::path::PathBuf::from("/unused"),
        owner: 1000,
        home: std::path::PathBuf::from("/home/user"),
    };
    let mut view = super::super::tests::fixture();
    let id = uuid::Uuid::new_v4().to_string();
    view.session.as_mut().unwrap().offers.push(NearbyOffer {
        id: id.clone(),
        sender: "Phone".into(),
        files: vec![SharedFile {
            name: "a.txt".into(),
            bytes: None,
        }],
        expires_at: "2026-11-01T12:00:00Z".into(),
        phase: TransferPhase::Offered,
    });
    ensure_receive_capacity(&store, &view, &id).unwrap();
    let oversized = Store {
        home: std::path::PathBuf::from(format!("/{}", "x".repeat(4096))),
        ..store
    };
    assert!(ensure_receive_capacity(&oversized, &view, &id).is_err());
    assert!(view.session.as_ref().unwrap().transfers.is_empty());
}
