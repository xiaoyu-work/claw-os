use super::*;

// FIPS 180-4 official test vectors (NIST), shrunken to a few
// canonical points. If any of these break, the SHA-256 impl is
// wrong and every dependent system (engine verification, SigV4
// signing) is broken.

#[test]
fn sha256_empty_string() {
    // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha256_abc_test_vector() {
    // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn sha256_long_message_spans_multiple_blocks() {
    // 56 'a' chars exercises the message length boundary where
    // the final block needs an extra round.
    let msg = "a".repeat(56);
    assert_eq!(
        sha256_hex(msg.as_bytes()),
        "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
    );
}

#[test]
fn sha256_streaming_matches_oneshot() {
    let mut h = Sha256Stream::new();
    h.update(b"the quick brown ");
    h.update(b"fox jumps over ");
    h.update(b"the lazy dog");
    let streamed = h.finalize_hex();
    let oneshot = sha256_hex(b"the quick brown fox jumps over the lazy dog");
    assert_eq!(streamed, oneshot);
}

// RFC 4231 HMAC-SHA-256 test vectors — the spec's official
// conformance set.

#[test]
fn hmac_sha256_rfc4231_case1() {
    // Key = 0x0b * 20, Data = "Hi There"
    let key = [0x0bu8; 20];
    let mac = hmac_sha256_hex(&key, b"Hi There");
    assert_eq!(
        mac,
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}

#[test]
fn hmac_sha256_rfc4231_case2() {
    // Key = "Jefe", Data = "what do ya want for nothing?"
    let mac = hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?");
    assert_eq!(
        mac,
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn hmac_sha256_with_oversize_key_hashes_first() {
    // RFC 4231 case 6: key length > block size (131 bytes here).
    // Should be hashed down to 32 bytes before use. Validates the
    // K-longer-than-block branch in our impl.
    let key = [0xaau8; 131];
    let data = b"Test Using Larger Than Block-Size Key - Hash Key First";
    let mac = hmac_sha256_hex(&key, data);
    assert_eq!(
        mac,
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

#[test]
fn hmac_sha256_empty_data() {
    // Edge case: empty payload, non-empty key.
    let mac = hmac_sha256_hex(b"key", b"");
    // Verified via openssl: `printf '' | openssl dgst -sha256 -hmac key`
    assert_eq!(
        mac,
        "5d5d139563c95b5967b9bd9a8c9b233a9dedb45072794cd232dc1b74832607d0"
    );
}
