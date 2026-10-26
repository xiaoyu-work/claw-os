use super::*;

#[test]
fn nearby_controls_are_closed_bounded_and_owner_free() {
    let id = uuid::Uuid::new_v4().to_string();
    let action = NearbyAction::Open {
        duration_secs: 300,
        receive: true,
        expected_mode: RadioMode::Exclusive,
        allow_disruption: false,
    };
    action.validate().unwrap();
    let mut value = serde_json::to_value(&action).unwrap();
    for field in [
        "owner_uid",
        "capabilities",
        "interface",
        "executable",
        "auto_accept",
    ] {
        value[field] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<NearbyAction>(value.clone()).is_err(),
            "{field}"
        );
        value.as_object_mut().unwrap().remove(field);
    }
    assert!(NearbyAction::Send {
        session_id: id.clone(),
        peer_id: id,
        paths: vec!["relative".into()],
        uploads: vec![]
    }
    .validate()
    .is_err());
    assert!(validate_files(&[SharedFile {
        name: "../escape".into(),
        bytes: Some(1)
    }])
    .is_err());
    assert!(validate_files(&[SharedFile {
        name: "large".into(),
        bytes: Some(MAX_FILE_BYTES + 1)
    }])
    .is_err());
}

fn view() -> NearbyView {
    NearbyView {
        schema: SCHEMA,
        owner_uid: 1000,
        inbox: "/home/user/Downloads/Claw Nearby".into(),
        uploads: Vec::new(),
        probe: RadioProbe {
            available: true,
            mode: Some(RadioMode::Shared),
            driver: None,
            diagnostics: Vec::new(),
        },
        session: Some(NearbySession {
            id: uuid::Uuid::new_v4().to_string(),
            phase: SessionPhase::Ready,
            mode: RadioMode::Shared,
            receive: true,
            expires_at: "2026-11-01T12:00:00.000Z".into(),
            peers: Vec::new(),
            offers: Vec::new(),
            discovering: false,
            transfers: Vec::new(),
            message: None,
        }),
    }
}

#[test]
fn nearby_limits_are_exact_and_do_not_conflate_file_and_wire_bytes() {
    let set: Vec<_> = (0..4)
        .map(|n| SharedFile {
            name: format!("file-{n}"),
            bytes: Some(MAX_FILE_BYTES),
        })
        .collect();
    validate_files(&set).unwrap();
    let mut oversized = set.clone();
    oversized.push(SharedFile {
        name: "extra".into(),
        bytes: Some(1),
    });
    assert!(validate_files(&oversized).is_err());
    assert!(validate_files(&[set[0].clone(), set[0].clone()]).is_err());
    for seconds in [30, 600] {
        NearbyAction::Open {
            duration_secs: seconds,
            receive: false,
            expected_mode: RadioMode::Shared,
            allow_disruption: false,
        }
        .validate()
        .unwrap();
    }
    for seconds in [0, 29, 601] {
        let body = serde_json::json!({"request":{"action":"open","duration_secs":seconds,
            "receive":true,"expected_mode":"shared","allow_disruption":false}});
        assert!(serde_json::from_value::<NearbyRequest>(body).is_err());
    }
    let mut status = view();
    status
        .session
        .as_mut()
        .unwrap()
        .transfers
        .push(NearbyTransfer {
            id: uuid::Uuid::new_v4().to_string(),
            incoming: false,
            peer: "Receiver".into(),
            files: set,
            phase: TransferPhase::Sending,
            bytes: MAX_WIRE_BYTES,
            total_bytes: Some(MAX_WIRE_BYTES),
            received_paths: Vec::new(),
            message: None,
        });
    status.validate(1000).unwrap();
    status.session.as_mut().unwrap().transfers[0].bytes += 1;
    assert!(status.validate(1000).is_err());
}

#[test]
fn nearby_views_check_owners_timestamps_ids_and_real_encoded_size() {
    let mut view = view();
    view.validate(1000).unwrap();
    assert!(view.validate(1001).is_err());
    view.session.as_mut().unwrap().expires_at = "not-a-timestamp".into();
    assert!(view.validate(1000).is_err());
    assert!(!file_name("\u{202e}hidden.txt"));
    assert!(
        serde_json::from_value::<NearbyStatusRequest>(serde_json::json!({"owner_uid":1001}))
            .is_err()
    );
    let mut oversized = self::view();
    for index in 0..MAX_TRANSFERS {
        oversized
            .session
            .as_mut()
            .unwrap()
            .transfers
            .push(NearbyTransfer {
                id: uuid::Uuid::new_v4().to_string(),
                incoming: true,
                peer: "Receiver".into(),
                files: vec![SharedFile {
                    name: "a".into(),
                    bytes: None,
                }],
                phase: TransferPhase::Received,
                bytes: 0,
                total_bytes: None,
                received_paths: (0..MAX_FILES)
                    .map(|n| format!("/{}-{index}-{n}", "x".repeat(2048)))
                    .collect(),
                message: None,
            });
    }
    assert!(serde_json::to_vec(&oversized).unwrap().len() > MAX_FRAME_BYTES);
    assert!(oversized.validate(1000).is_err());
}

#[test]
fn nearby_acknowledgements_cannot_switch_windows_or_upload_offsets() {
    let mut view = view();
    let other = uuid::Uuid::new_v4().to_string();
    let response = NearbyResponse {
        view: view.clone(),
        upload: None,
    };
    assert!(response
        .validate(1000, &NearbyAction::Stop { session_id: other })
        .is_err());
    let upload = UploadView {
        id: uuid::Uuid::new_v4().to_string(),
        name: "a.txt".into(),
        bytes: 3,
        written: 3,
        ready: false,
    };
    view.uploads.push(upload.clone());
    let action = NearbyAction::UploadChunk {
        upload_id: upload.id.clone(),
        offset: 0,
        data: "YWJj".into(),
    };
    let mut response = NearbyResponse {
        view,
        upload: Some(upload),
    };
    response.validate(1000, &action).unwrap();
    response.upload.as_mut().unwrap().written = 2;
    assert!(response.validate(1000, &action).is_err());
}
