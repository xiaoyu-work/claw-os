use super::*;

#[tokio::test]
async fn command_frames_survive_interleaving_and_refuse_malformed_assignments() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let mut reader = BufReader::new(reader);
    let mut bytes = Vec::new();
    writer.write_all(b"{\"command\":\"st").await.unwrap();
    assert!(
        timeout(Duration::from_millis(10), frame(&mut reader, &mut bytes))
            .await
            .is_err()
    );
    writer.write_all(b"op\"}\n").await.unwrap();
    assert!(matches!(
        frame(&mut reader, &mut bytes).await.unwrap(),
        Some(WorkerCommand::Stop {})
    ));
    for input in [
        b"{\"command\":\"stop\"}".to_vec(),
        b"{\"command\":\"stop\",\"root\":true}\n".to_vec(),
        vec![b'x'; MAX_FRAME_BYTES + 1],
    ] {
        let mut reader = BufReader::new(input.as_slice());
        let mut bytes = Vec::new();
        assert!(frame(&mut reader, &mut bytes).await.is_err());
        assert!(bytes.len() <= MAX_FRAME_BYTES);
    }
}

#[test]
fn only_exact_staged_files_and_link_local_peers_can_be_assigned() {
    let id = uuid::Uuid::new_v4();
    assert!(valid_send_path(&format!(
        "/data/outgoing/{id}/archive..txt"
    )));
    for path in [
        "/etc/passwd",
        "/data/outgoing/../secret",
        "/data/outgoing/not-a-transfer/file",
        "/data/outgoing",
        "/data/outgoing/file",
    ] {
        assert!(!valid_send_path(path), "{path}");
    }
    assert!(address("[::1]:8770", 7).is_err());
    assert!(address("[2001:db8::1]:8770", 7).is_err());
    assert_eq!(address("[fe80::2]:8770", 7).unwrap().scope_id(), 7);
    let config = receiver_config("Fixture".into());
    assert_eq!(config.ask_timeout, Duration::from_secs(15));
    assert_eq!(config.limits.max_upload_body, MAX_WIRE_BYTES);
    assert_eq!(config.limits.extract.max_total, MAX_TOTAL_BYTES);
}

#[tokio::test]
async fn actual_airdrop_tls_driver_declines_then_transfers_only_after_exact_acceptance() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("one TLS fixture");
    let source = tempfile::tempdir().unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let path = source.path().join("nearby-fixture.txt");
    std::fs::write(&path, b"bounded nearby fixture").unwrap();
    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
    let std::net::SocketAddr::V6(address) = listener.local_addr().unwrap() else {
        panic!("IPv6 fixture");
    };
    drop(listener);
    let identity = Identity::generate().unwrap();
    let (receiver, mut incoming) = Receiver::bind(
        address,
        &identity,
        Options::new(
            receiver_config("Claw fixture".into()),
            inbox.path().to_path_buf(),
        ),
    )
    .await
    .unwrap();
    let receiver = tokio::spawn(receiver.run());
    let (events, _progress) = mpsc::channel(64);
    for accept in [false, true] {
        let id = uuid::Uuid::new_v4().to_string();
        let files = vec![path.to_str().unwrap().to_string()];
        let events = events.clone();
        let sending = tokio::spawn(async move {
            send_files("Fixture", &id, address, RECEIVER_FLAGS, files, &events).await
        });
        let mut request_id = None;
        let mut received_paths = Vec::new();
        timeout(Duration::from_secs(15), async {
            while let Some(event) = incoming.recv().await {
                match event {
                    ReceiverEvent::Request(request) => {
                        assert!(request_id.is_none());
                        let Offer::Files(files) = &request.ask.offer else {
                            panic!("file-only fixture");
                        };
                        assert_eq!(files.len(), 1);
                        assert_eq!(files[0].name, "nearby-fixture.txt");
                        request_id = Some(request.id);
                        if accept {
                            request.accept();
                        } else {
                            request.decline();
                            break;
                        }
                    }
                    ReceiverEvent::Received { id, paths } => {
                        assert!(accept);
                        assert_eq!(Some(id), request_id);
                        received_paths = paths;
                        break;
                    }
                    ReceiverEvent::Discovered { .. } | ReceiverEvent::Withdrawn { .. } => {}
                    other => panic!("unexpected receive event: {other:?}"),
                }
            }
        })
        .await
        .expect("bounded protocol exchange");
        let outcome = timeout(Duration::from_secs(15), sending)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome, if accept { "delivered" } else { "declined" });
        if accept {
            assert_eq!(received_paths.len(), 1);
            assert_eq!(
                std::fs::read(&received_paths[0]).unwrap(),
                b"bounded nearby fixture"
            );
        } else {
            assert_eq!(std::fs::read_dir(inbox.path()).unwrap().count(), 0);
        }
    }
    receiver.abort();
    assert!(receiver.await.unwrap_err().is_cancelled());
}
