use super::*;

#[test]
fn resamples_above_and_below_target_rate() {
    let high = vec![0.5; 48_000];
    let low = vec![0.5; 8_000];
    assert_eq!(resample_to_16k(&high, 48_000).unwrap().len(), 16_000);
    assert_eq!(resample_to_16k(&low, 8_000).unwrap().len(), 16_000);
}

#[test]
fn rejects_empty_and_limits_output() {
    assert!(resample_to_16k(&[], 48_000).is_err());
    let oversized = vec![0i16; MAX_OUTPUT_SAMPLES + 1];
    assert!(encode_wav(&oversized).is_err());
}

#[test]
fn writes_pcm_wav() {
    let wav = encode_wav(&[0, i16::MAX, i16::MIN]).unwrap();
    let reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
    assert_eq!(reader.spec().sample_rate, TARGET_RATE);
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.duration(), 3);
}
