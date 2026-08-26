use super::*;

#[test]
fn encode_then_decode_round_trips_mono() {
    let samples: Vec<i16> = (0..16).map(|i| i * 100 - 800).collect();
    let bytes = encode_pcm16(&samples, 1, 16_000).unwrap();
    let info = decode_pcm16(&bytes).unwrap();
    assert_eq!(info.channels, 1);
    assert_eq!(info.sample_rate_hz, 16_000);
    assert_eq!(info.bits_per_sample, 16);
    assert_eq!(info.samples, samples);
}

#[test]
fn encode_then_decode_round_trips_stereo() {
    let samples: Vec<i16> = vec![100, -100, 200, -200, 300, -300];
    let bytes = encode_pcm16(&samples, 2, 44_100).unwrap();
    let info = decode_pcm16(&bytes).unwrap();
    assert_eq!(info.channels, 2);
    assert_eq!(info.sample_rate_hz, 44_100);
    assert_eq!(info.samples, samples);
}

#[test]
fn empty_header_is_44_bytes_and_parses() {
    let h = empty_header(1, 16_000);
    assert_eq!(h.len(), 44);
    let info = decode_pcm16(&h).unwrap();
    assert_eq!(info.samples.len(), 0);
    assert_eq!(info.sample_rate_hz, 16_000);
}

#[test]
fn rejects_zero_channels() {
    let err = encode_pcm16(&[0i16], 0, 16_000).unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn rejects_zero_sample_rate() {
    let err = encode_pcm16(&[0i16], 1, 0).unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn rejects_unaligned_stereo() {
    let err = encode_pcm16(&[1, 2, 3], 2, 44_100).unwrap_err();
    assert!(matches!(err, MediaError::InvalidRequest(_)));
}

#[test]
fn decode_rejects_short_buffer() {
    let err = decode_pcm16(&[0u8; 10]).unwrap_err();
    assert!(matches!(err, MediaError::Parse(_)));
}

#[test]
fn decode_rejects_missing_riff() {
    let mut bytes = empty_header(1, 16_000);
    bytes[0] = b'X';
    let err = decode_pcm16(&bytes).unwrap_err();
    assert!(matches!(err, MediaError::Parse(_)));
}

#[test]
fn decode_skips_unknown_chunk_before_data() {
    // Build manually: RIFF + WAVE + fmt + JUNK + data.
    let samples = vec![100i16, 200, 300, 400];
    let mut chunk: Vec<u8> = Vec::new();
    chunk.extend_from_slice(b"WAVE");
    chunk.extend_from_slice(b"fmt ");
    chunk.extend_from_slice(&16u32.to_le_bytes());
    chunk.extend_from_slice(&1u16.to_le_bytes()); // PCM
    chunk.extend_from_slice(&1u16.to_le_bytes()); // channels
    chunk.extend_from_slice(&8000u32.to_le_bytes()); // rate
    chunk.extend_from_slice(&16000u32.to_le_bytes()); // byte rate
    chunk.extend_from_slice(&2u16.to_le_bytes()); // block align
    chunk.extend_from_slice(&16u16.to_le_bytes()); // bits
                                                   // Unknown JUNK chunk to skip
    chunk.extend_from_slice(b"JUNK");
    chunk.extend_from_slice(&4u32.to_le_bytes());
    chunk.extend_from_slice(&[0xFFu8; 4]);
    // data
    chunk.extend_from_slice(b"data");
    let data_bytes = (samples.len() * 2) as u32;
    chunk.extend_from_slice(&data_bytes.to_le_bytes());
    for s in &samples {
        chunk.extend_from_slice(&s.to_le_bytes());
    }

    let mut out = Vec::with_capacity(8 + chunk.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(&chunk);

    let info = decode_pcm16(&out).unwrap();
    assert_eq!(info.samples, samples);
}

#[test]
fn decode_rejects_non_pcm_format_tag() {
    let mut bytes = empty_header(1, 16_000);
    // fmt chunk format tag is at offset 20, 21 (little-endian u16).
    bytes[20] = 3; // IEEE float
    bytes[21] = 0;
    let err = decode_pcm16(&bytes).unwrap_err();
    match err {
        MediaError::Parse(s) => assert!(s.contains("format tag"), "got: {s}"),
        other => panic!("expected Parse, got {other:?}"),
    }
}

#[test]
fn decode_rejects_non_16bit_pcm() {
    // hand-build: PCM (tag=1) but bits_per_sample=8.
    let mut chunk: Vec<u8> = Vec::new();
    chunk.extend_from_slice(b"WAVE");
    chunk.extend_from_slice(b"fmt ");
    chunk.extend_from_slice(&16u32.to_le_bytes());
    chunk.extend_from_slice(&1u16.to_le_bytes()); // PCM
    chunk.extend_from_slice(&1u16.to_le_bytes());
    chunk.extend_from_slice(&8000u32.to_le_bytes());
    chunk.extend_from_slice(&8000u32.to_le_bytes());
    chunk.extend_from_slice(&1u16.to_le_bytes());
    chunk.extend_from_slice(&8u16.to_le_bytes()); // 8-bit
    chunk.extend_from_slice(b"data");
    chunk.extend_from_slice(&0u32.to_le_bytes());

    let mut out = Vec::with_capacity(8 + chunk.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(&chunk);

    let err = decode_pcm16(&out).unwrap_err();
    match err {
        MediaError::Parse(s) => assert!(s.contains("16-bit"), "got: {s}"),
        other => panic!("expected Parse, got {other:?}"),
    }
}
