//! In-tree cryptographic primitives — SHA-256 and HMAC-SHA-256.
//!
//! `cos` deliberately avoids pulling external crypto crates for these
//! basics: SHA-256 is a stable spec (FIPS 180-4) and HMAC-SHA-256 is a
//! 30-line construction over it. Keeping them in-tree lets us:
//!
//! * verify engine package downloads (`engine_pkg::download`) without
//!   adding `sha2` + transitive deps,
//! * implement AWS Signature V4 signing (`agent::llm::sigv4`) for
//!   Bedrock without pulling `aws-sigv4` (~5MB of transitive surface),
//! * stay reproducible — no surprise CVE / breaking-change churn from
//!   a low-level dep we touch from the kernel hot path.
//!
//! These implementations are correctness-first, not perf-first. They
//! are appropriate for occasional signing / verification (a handful
//! of signatures per request, an archive hash per install). Don't put
//! them in tight loops.

#![allow(dead_code)]

/// Streaming SHA-256 hasher (FIPS 180-4).
///
/// Output is the 64-character lowercase-hex digest via
/// [`Sha256Stream::finalize_hex`], or 32 raw bytes via
/// [`Sha256Stream::finalize_bytes`]. State is consumed on finalize
/// so accidental double-finalize is a compile error.
pub struct Sha256Stream {
    state: [u32; 8],
    buffer: Vec<u8>,
    total_bits: u64,
}

impl Default for Sha256Stream {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256Stream {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: Vec::with_capacity(64),
            total_bits: 0,
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);
        self.buffer.extend_from_slice(data);
        while self.buffer.len() >= 64 {
            let block: [u8; 64] = self.buffer[..64].try_into().unwrap();
            self.compress(&block);
            self.buffer.drain(..64);
        }
    }

    pub fn finalize_bytes(mut self) -> [u8; 32] {
        let bits = self.total_bits;
        self.buffer.push(0x80);
        while self.buffer.len() % 64 != 56 {
            self.buffer.push(0);
        }
        self.buffer.extend_from_slice(&bits.to_be_bytes());
        let mut i = 0;
        while i < self.buffer.len() {
            let block: [u8; 64] = self.buffer[i..i + 64].try_into().unwrap();
            self.compress(&block);
            i += 64;
        }
        let mut out = [0u8; 32];
        for (idx, w) in self.state.iter().enumerate() {
            out[idx * 4..(idx + 1) * 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    pub fn finalize_hex(self) -> String {
        let bytes = self.finalize_bytes();
        let mut out = String::with_capacity(64);
        for b in bytes {
            out.push_str(&format!("{:02x}", b));
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

/// Convenience: SHA-256 a single byte slice and return lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256Stream::new();
    h.update(data);
    h.finalize_hex()
}

/// Convenience: SHA-256 a single byte slice and return raw 32 bytes.
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256Stream::new();
    h.update(data);
    h.finalize_bytes()
}

/// HMAC-SHA-256 (RFC 2104) — keyed MAC over arbitrary-length data.
///
/// Returns 32 raw bytes. Use [`hmac_sha256_hex`] for the lowercase
/// hex form (e.g. AWS SigV4 signature output).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    // Step 1: derive a block-sized key.
    let mut k0 = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        // RFC 2104: H(K) when K is longer than the block size.
        let h = sha256_bytes(key);
        k0[..32].copy_from_slice(&h);
    } else {
        k0[..key.len()].copy_from_slice(key);
    }
    // Step 2: ipad/opad.
    let mut ipad = [0u8; BLOCK_SIZE];
    let mut opad = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] = k0[i] ^ 0x36;
        opad[i] = k0[i] ^ 0x5c;
    }
    // Step 3: inner = H((K0 XOR ipad) || data).
    let mut inner = Sha256Stream::new();
    inner.update(&ipad);
    inner.update(data);
    let inner_hash = inner.finalize_bytes();
    // Step 4: outer = H((K0 XOR opad) || inner).
    let mut outer = Sha256Stream::new();
    outer.update(&opad);
    outer.update(&inner_hash);
    outer.finalize_bytes()
}

/// HMAC-SHA-256 → lowercase hex.
pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    let bytes = hmac_sha256(key, data);
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
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
}
