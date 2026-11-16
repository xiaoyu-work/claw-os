use super::*;

#[test]
fn audio_accepts_only_bounded_output_pcm_and_never_bus_device_or_capture_options() {
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    assert_eq!(decode_pcm(&encode(&[0, 1, 2, 3])).unwrap(), [0, 1, 2, 3]);
    for bytes in [vec![], vec![0; 3], vec![0; PCM_BYTES + 4]] {
        assert!(decode_pcm(&encode(&bytes)).is_err());
    }
    assert!(decode_pcm("not PCM").is_err());
    let valid = json!({"action":"open","session":"window","stream_id":"a".repeat(32)});
    assert!(serde_json::from_value::<Request>(valid.clone()).is_ok());
    for field in [
        "owner_uid",
        "program",
        "bus",
        "device",
        "format",
        "rate",
        "channels",
        "record",
        "target",
    ] {
        let mut invalid = valid.clone();
        invalid[field] = json!("caller-selected");
        assert!(
            serde_json::from_value::<Request>(invalid).is_err(),
            "{field}"
        );
    }
}
