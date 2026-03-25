/// OS-level credential store — encrypted secret storage with tier-based access.
///
/// Analogous to the Linux kernel keyring (`keyctl`), this provides a secure
/// store for secrets (API keys, tokens, passwords) that are accessible only
/// to sessions with sufficient privilege tier.
///
/// Credentials are encrypted with AES-256-GCM keyed on a SHA-256 hash of the
/// machine ID. Legacy credentials using XOR obfuscation are detected (by the
/// absence of a `nonce_b64` field) and still decrypted for backward
/// compatibility.
///
/// Features:
///   - **Namespace isolation**: credentials live under `<namespace>/` subdirs.
///   - **TTL / expiry**: optional `--ttl <seconds>` on store; enforced on load.
///   - **Bundles**: named groups of credentials loaded as a single JSON object.
///
/// Storage: `$COS_DATA_DIR/credentials/<namespace>/<name>.json`
///
///   - **Auto-refresh**: optional `--refresh-cmd CMD` on store; executed on
///     load if credential is expired.
///
/// Commands:
///   store  <name> <value> [--tier N] [--namespace NS] [--ttl SECS] [--refresh-cmd CMD]
///   load   <name> [--namespace NS]
///   revoke <name> [--namespace NS]
///   list   [--namespace NS]         — omit NS to see all namespaces
///   bundle <name> --keys k1,k2,k3 [--namespace NS]
///   load-bundle <name> [--namespace NS]
///   oauth-refresh <google|microsoft> [--namespace NS]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};

// ===========================================================================
// SHA-256 (pure Rust, no external crate)
// ===========================================================================

mod sha256 {
    /// SHA-256 round constants (first 32 bits of the fractional parts of the
    /// cube roots of the first 64 primes).
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

    /// Initial hash values (first 32 bits of the fractional parts of the
    /// square roots of the first 8 primes).
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    /// Compute the SHA-256 digest of `data`.
    pub(super) fn hash(data: &[u8]) -> [u8; 32] {
        // Pre-processing: pad message to a multiple of 512 bits (64 bytes).
        let bit_len = (data.len() as u64) * 8;
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        let mut h = H0;

        // Process each 512-bit (64-byte) block.
        for block in msg.chunks_exact(64) {
            let mut w = [0u32; 64];
            for t in 0..16 {
                w[t] = u32::from_be_bytes([
                    block[4 * t],
                    block[4 * t + 1],
                    block[4 * t + 2],
                    block[4 * t + 3],
                ]);
            }
            for t in 16..64 {
                let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
                let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
                w[t] = w[t - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[t - 7])
                    .wrapping_add(s1);
            }

            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

            for t in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[t])
                    .wrapping_add(w[t]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);

                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }

            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

// ===========================================================================
// AES-256-GCM (pure Rust, no external crate)
// ===========================================================================

mod aes_gcm {
    // ---- AES S-box --------------------------------------------------------
    #[rustfmt::skip]
    const SBOX: [u8; 256] = [
        0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
        0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
        0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
        0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
        0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
        0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
        0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
        0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
        0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
        0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
        0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
        0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
        0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
        0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
        0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
        0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
    ];

    // ---- AES round-constant (only byte 0 is non-zero) ---------------------
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

    // ---- AES-256 key schedule ---------------------------------------------

    /// Expanded round keys for AES-256 (15 × 16 bytes = 240 bytes).
    struct Aes256 {
        rk: [[u8; 16]; 15],
    }

    impl Aes256 {
        fn new(key: &[u8; 32]) -> Self {
            // Expand into 60 32-bit words (Nk=8, Nr=14).
            let mut w = [0u32; 60];
            for i in 0..8 {
                w[i] = u32::from_be_bytes([
                    key[4 * i],
                    key[4 * i + 1],
                    key[4 * i + 2],
                    key[4 * i + 3],
                ]);
            }
            for i in 8..60 {
                let mut tmp = w[i - 1];
                if i % 8 == 0 {
                    tmp = sub_word(rot_word(tmp)) ^ ((RCON[i / 8 - 1] as u32) << 24);
                } else if i % 8 == 4 {
                    tmp = sub_word(tmp);
                }
                w[i] = w[i - 8] ^ tmp;
            }

            let mut rk = [[0u8; 16]; 15];
            for r in 0..15 {
                for j in 0..4 {
                    let bytes = w[4 * r + j].to_be_bytes();
                    rk[r][4 * j..4 * j + 4].copy_from_slice(&bytes);
                }
            }
            Self { rk }
        }

        /// Encrypt one 16-byte block in place (AES-256, encryption direction only).
        fn encrypt_block(&self, blk: &mut [u8; 16]) {
            xor_block(blk, &self.rk[0]);
            for round in 1..14 {
                sub_bytes(blk);
                shift_rows(blk);
                mix_columns(blk);
                xor_block(blk, &self.rk[round]);
            }
            sub_bytes(blk);
            shift_rows(blk);
            xor_block(blk, &self.rk[14]);
        }
    }

    fn sub_word(w: u32) -> u32 {
        let b = w.to_be_bytes();
        u32::from_be_bytes([
            SBOX[b[0] as usize],
            SBOX[b[1] as usize],
            SBOX[b[2] as usize],
            SBOX[b[3] as usize],
        ])
    }

    fn rot_word(w: u32) -> u32 {
        w.rotate_left(8)
    }

    fn xor_block(a: &mut [u8; 16], b: &[u8; 16]) {
        for i in 0..16 {
            a[i] ^= b[i];
        }
    }

    fn sub_bytes(blk: &mut [u8; 16]) {
        for b in blk.iter_mut() {
            *b = SBOX[*b as usize];
        }
    }

    fn shift_rows(s: &mut [u8; 16]) {
        // Row 1: shift left 1
        let t = s[1];
        s[1] = s[5];
        s[5] = s[9];
        s[9] = s[13];
        s[13] = t;
        // Row 2: shift left 2
        let (t0, t1) = (s[2], s[6]);
        s[2] = s[10];
        s[6] = s[14];
        s[10] = t0;
        s[14] = t1;
        // Row 3: shift left 3 (= shift right 1)
        let t = s[15];
        s[15] = s[11];
        s[11] = s[7];
        s[7] = s[3];
        s[3] = t;
    }

    /// Multiply by 2 in GF(2^8) with irreducible polynomial x^8+x^4+x^3+x+1.
    fn xtime(x: u8) -> u8 {
        if x & 0x80 != 0 {
            (x << 1) ^ 0x1b
        } else {
            x << 1
        }
    }

    fn mix_columns(s: &mut [u8; 16]) {
        for col in 0..4 {
            let i = 4 * col;
            let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
            let t = a0 ^ a1 ^ a2 ^ a3;
            s[i] = a0 ^ xtime(a0 ^ a1) ^ t;
            s[i + 1] = a1 ^ xtime(a1 ^ a2) ^ t;
            s[i + 2] = a2 ^ xtime(a2 ^ a3) ^ t;
            s[i + 3] = a3 ^ xtime(a3 ^ a0) ^ t;
        }
    }

    // ---- GCM: GHASH in GF(2^128) -----------------------------------------

    /// Multiply two 128-bit blocks in GF(2^128) with the GCM polynomial
    /// R = 0xE1 || 0^120.
    fn ghash_mul(x: &[u8; 16], y: &[u8; 16]) -> [u8; 16] {
        let mut z = [0u8; 16];
        let mut v = *y;
        for i in 0..128 {
            if (x[i / 8] >> (7 - (i % 8))) & 1 == 1 {
                for k in 0..16 {
                    z[k] ^= v[k];
                }
            }
            let lsb = v[15] & 1;
            // Right-shift V by 1 bit
            for k in (1..16).rev() {
                v[k] = (v[k] >> 1) | (v[k - 1] << 7);
            }
            v[0] >>= 1;
            if lsb == 1 {
                v[0] ^= 0xe1; // R polynomial high byte
            }
        }
        z
    }

    /// Compute GHASH_H(aad, ciphertext).
    fn ghash(h: &[u8; 16], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut y = [0u8; 16];

        // Process AAD blocks
        for chunk in aad.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            for k in 0..16 {
                y[k] ^= block[k];
            }
            y = ghash_mul(&y, h);
        }

        // Process ciphertext blocks
        for chunk in ct.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            for k in 0..16 {
                y[k] ^= block[k];
            }
            y = ghash_mul(&y, h);
        }

        // Final block: lengths (in bits) of AAD and CT as big-endian u64.
        let aad_bits = (aad.len() as u64) * 8;
        let ct_bits = (ct.len() as u64) * 8;
        let mut len_block = [0u8; 16];
        len_block[..8].copy_from_slice(&aad_bits.to_be_bytes());
        len_block[8..].copy_from_slice(&ct_bits.to_be_bytes());
        for k in 0..16 {
            y[k] ^= len_block[k];
        }
        y = ghash_mul(&y, h);

        y
    }

    /// Increment the rightmost 32 bits of a 128-bit counter block.
    fn inc32(counter: &mut [u8; 16]) {
        let mut c = u32::from_be_bytes([counter[12], counter[13], counter[14], counter[15]]);
        c = c.wrapping_add(1);
        counter[12..16].copy_from_slice(&c.to_be_bytes());
    }

    // ---- Public API -------------------------------------------------------

    /// Encrypt with AES-256-GCM.  Returns `ciphertext || 16-byte tag`.
    pub(super) fn encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
        let aes = Aes256::new(key);

        // H = AES_K(0^128)
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);

        // J0 = nonce || 0x00000001  (96-bit IV path)
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;

        // Encrypt plaintext with GCTR starting at inc32(J0)
        let mut counter = j0;
        let mut ciphertext = Vec::with_capacity(plaintext.len());

        for chunk in plaintext.chunks(16) {
            inc32(&mut counter);
            let mut keystream = counter;
            aes.encrypt_block(&mut keystream);
            for (i, &p) in chunk.iter().enumerate() {
                ciphertext.push(p ^ keystream[i]);
            }
        }

        // Compute authentication tag
        let tag_input = ghash(&h, &[], &ciphertext);
        let mut tag_block = j0;
        aes.encrypt_block(&mut tag_block);
        let mut tag = [0u8; 16];
        for k in 0..16 {
            tag[k] = tag_input[k] ^ tag_block[k];
        }

        ciphertext.extend_from_slice(&tag);
        ciphertext
    }

    /// Decrypt with AES-256-GCM.  Input is `ciphertext || 16-byte tag`.
    /// Returns the plaintext or an error if the tag does not match.
    pub(super) fn decrypt(
        key: &[u8; 32],
        nonce: &[u8; 12],
        ct_and_tag: &[u8],
    ) -> Result<Vec<u8>, String> {
        if ct_and_tag.len() < 16 {
            return Err("ciphertext too short (missing tag)".into());
        }
        let ct_len = ct_and_tag.len() - 16;
        let ct = &ct_and_tag[..ct_len];
        let expected_tag = &ct_and_tag[ct_len..];

        let aes = Aes256::new(key);

        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);

        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;

        // Verify tag first
        let tag_input = ghash(&h, &[], ct);
        let mut tag_block = j0;
        aes.encrypt_block(&mut tag_block);
        let mut computed_tag = [0u8; 16];
        for k in 0..16 {
            computed_tag[k] = tag_input[k] ^ tag_block[k];
        }
        if computed_tag != expected_tag {
            return Err("AES-GCM authentication failed".into());
        }

        // Decrypt
        let mut counter = j0;
        let mut plaintext = Vec::with_capacity(ct_len);
        for chunk in ct.chunks(16) {
            inc32(&mut counter);
            let mut keystream = counter;
            aes.encrypt_block(&mut keystream);
            for (i, &c) in chunk.iter().enumerate() {
                plaintext.push(c ^ keystream[i]);
            }
        }

        Ok(plaintext)
    }
}

// ===========================================================================
// Base64 helpers (no external dependency)
// ===========================================================================

fn to_b64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn from_b64(s: &str) -> Result<Vec<u8>, String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::new();
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let val = |c: u8| -> u32 {
            if c == b'=' {
                0
            } else {
                CHARS.iter().position(|&x| x == c).unwrap_or(0) as u32
            }
        };
        let b0 = val(chunk[0]);
        let b1 = val(chunk[1]);
        let b2 = if chunk.len() > 2 { val(chunk[2]) } else { 0 };
        let b3 = if chunk.len() > 3 { val(chunk[3]) } else { 0 };
        let n = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        result.push(((n >> 16) & 0xFF) as u8);
        if chunk.len() > 2 && chunk[2] != b'=' {
            result.push(((n >> 8) & 0xFF) as u8);
        }
        if chunk.len() > 3 && chunk[3] != b'=' {
            result.push((n & 0xFF) as u8);
        }
    }
    Ok(result)
}

// ===========================================================================
// Key derivation and nonce generation
// ===========================================================================

/// Derive a 256-bit encryption key from the machine identity.
/// Uses SHA-256(machine-id) so the result is always exactly 32 bytes.
fn derive_key() -> [u8; 32] {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string("/etc/machine-id") {
            return sha256::hash(id.trim().as_bytes());
        }
    }
    sha256::hash(b"claw-os-credential-store-key-v1")
}

/// Generate a random 12-byte nonce.
/// Reads `/dev/urandom` on Linux; falls back to timestamp-based on other OS.
fn generate_nonce() -> [u8; 12] {
    #[cfg(target_os = "linux")]
    {
        if let Ok(bytes) = fs::read("/dev/urandom") {
            // read returns the whole file; just take first 12 bytes — but
            // that's unreliable.  Use std::io::Read instead.
        }
        use std::io::Read;
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            let mut nonce = [0u8; 12];
            if f.read_exact(&mut nonce).is_ok() {
                return nonce;
            }
        }
    }
    // Fallback: timestamp-based nonce (non-Linux or urandom unavailable).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut nonce = [0u8; 12];
    nonce[..8].copy_from_slice(&now.as_nanos().to_le_bytes()[..8]);
    // Mix in a process-level counter to avoid collisions within the same
    // nanosecond (e.g. in tests).
    static CTR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let c = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    nonce[8..12].copy_from_slice(&c.to_le_bytes());
    nonce
}

// ===========================================================================
// Legacy XOR obfuscation (backward compatibility only)
// ===========================================================================

/// Key used by the legacy XOR obfuscation scheme.
fn legacy_obfuscation_key() -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string("/etc/machine-id") {
            return id.trim().as_bytes().to_vec();
        }
    }
    b"claw-os-credential-store-key-v1".to_vec()
}

/// XOR-based deobfuscation (symmetric — same function encrypts and decrypts).
fn legacy_xor(data: &[u8]) -> Vec<u8> {
    let key = legacy_obfuscation_key();
    data.iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

// ===========================================================================
// Credential and bundle data structures
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCredential {
    name: String,
    /// Namespace this credential belongs to.
    namespace: String,
    /// Base64-encoded encrypted value (AES-256-GCM ciphertext + tag, or legacy
    /// XOR-obfuscated bytes).
    value_b64: String,
    /// Base64-encoded 12-byte nonce.  `None` indicates a legacy XOR credential.
    #[serde(default)]
    nonce_b64: Option<String>,
    /// Minimum tier required to load this credential (0 = ROOT only, 1 = OPERATE+, etc.)
    min_tier: u8,
    stored_at: String,
    stored_by: Option<String>,
    /// ISO 8601 expiry timestamp.  `None` means the credential never expires.
    #[serde(default)]
    expires_at: Option<String>,
    /// Command to execute when credential expires (auto-refresh).
    /// The command should output a new value to stdout.
    #[serde(default)]
    refresh_cmd: Option<String>,
}

/// A bundle manifest — a named group of credential keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleManifest {
    name: String,
    namespace: String,
    keys: Vec<String>,
    created_at: String,
}

// ===========================================================================
// Path helpers
// ===========================================================================

/// Root credentials directory: `$COS_DATA_DIR/credentials`.
fn credentials_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("credentials")
}

/// Namespace directory: `$COS_DATA_DIR/credentials/<namespace>`.
fn namespace_dir(namespace: &str) -> PathBuf {
    credentials_dir().join(namespace)
}

/// Bundle directory: `$COS_DATA_DIR/credentials/<namespace>/bundles`.
fn bundles_dir(namespace: &str) -> PathBuf {
    namespace_dir(namespace).join("bundles")
}

// ===========================================================================
// Argument parsing helpers
// ===========================================================================

/// Extract `--namespace <value>` from an argument list.
/// Returns `(namespace_option, remaining_args)`.
fn parse_namespace_flag(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut ns: Option<String> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--namespace" && i + 1 < args.len() {
            ns = Some(args[i + 1].clone());
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    (ns, rest)
}

// ===========================================================================
// Encryption / decryption helpers
// ===========================================================================

/// Encrypt a plaintext value with AES-256-GCM.
/// Returns `(value_b64, nonce_b64)`.
fn encrypt_value(plaintext: &[u8]) -> (String, String) {
    let key = derive_key();
    let nonce = generate_nonce();
    let ct_and_tag = aes_gcm::encrypt(&key, &nonce, plaintext);
    (to_b64(&ct_and_tag), to_b64(&nonce))
}

/// Decrypt a stored credential.  Handles both AES-256-GCM (has nonce) and
/// legacy XOR (no nonce) formats transparently.
fn decrypt_value(cred: &StoredCredential) -> Result<Vec<u8>, String> {
    let raw =
        from_b64(&cred.value_b64).map_err(|e| format!("failed to decode credential value: {e}"))?;

    match &cred.nonce_b64 {
        Some(nonce_b64) => {
            let nonce_bytes =
                from_b64(nonce_b64).map_err(|e| format!("failed to decode nonce: {e}"))?;
            if nonce_bytes.len() != 12 {
                return Err("invalid nonce length (expected 12 bytes)".into());
            }
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&nonce_bytes);
            let key = derive_key();
            aes_gcm::decrypt(&key, &nonce, &raw)
        }
        None => {
            // Legacy XOR format — no nonce field present.
            Ok(legacy_xor(&raw))
        }
    }
}

// ===========================================================================
// Expiry helpers
// ===========================================================================

/// Check whether a credential with the given `expires_at` has expired.
fn is_expired(expires_at: &Option<String>) -> bool {
    if let Some(exp) = expires_at {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(exp, "%Y-%m-%dT%H:%M:%SZ") {
            return chrono::Utc::now().naive_utc() > dt;
        }
    }
    false
}

// ===========================================================================
// Command dispatch
// ===========================================================================

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "store" => cmd_store(args),
        "load" => cmd_load(args),
        "revoke" => cmd_revoke(args),
        "list" => cmd_list(args),
        "bundle" => cmd_bundle(args),
        "load-bundle" => cmd_load_bundle(args),
        "oauth-refresh" => cmd_oauth_refresh(args),
        _ => Err(format!("unknown credential command: {command}")),
    }
}

// ===========================================================================
// Commands
// ===========================================================================

/// Store a credential.
///
/// Usage: cos credential store <name> <value> [--tier N] [--namespace NS] [--ttl SECS]
fn cmd_store(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let (ns_opt, args) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let mut min_tier: u8 = 0;
    let mut ttl: Option<u64> = None;
    let mut refresh_cmd: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tier" if i + 1 < args.len() => {
                min_tier = args[i + 1]
                    .parse::<u8>()
                    .map_err(|_| "tier must be 0-3".to_string())?;
                if min_tier > 3 {
                    return Err("tier must be 0-3".into());
                }
                i += 2;
            }
            "--ttl" if i + 1 < args.len() => {
                ttl = Some(
                    args[i + 1]
                        .parse::<u64>()
                        .map_err(|_| "ttl must be a positive integer (seconds)".to_string())?,
                );
                i += 2;
            }
            "--refresh-cmd" if i + 1 < args.len() => {
                refresh_cmd = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err(
            "usage: cos credential store <name> <value> [--tier N] [--namespace NS] [--ttl SECS] [--refresh-cmd CMD]"
                .into(),
        );
    }

    let name = &positional[0];
    let value = &positional[1];

    // Validate name: alphanumeric, hyphens, underscores
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("credential name must be alphanumeric (hyphens/underscores allowed)".into());
    }

    let dir = namespace_dir(&namespace);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create credentials dir: {e}"))?;

    // Encrypt with AES-256-GCM
    let (value_b64, nonce_b64) = encrypt_value(value.as_bytes());

    let session = std::env::var("COS_SESSION").ok();
    let now = chrono::Utc::now();
    let stored_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let expires_at = ttl.map(|secs| {
        let exp = now + chrono::Duration::seconds(secs as i64);
        exp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });

    let cred = StoredCredential {
        name: name.clone(),
        namespace: namespace.clone(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier,
        stored_at: stored_at.clone(),
        stored_by: session,
        expires_at: expires_at.clone(),
        refresh_cmd,
    };

    let path = dir.join(format!("{name}.json"));
    let data =
        serde_json::to_string_pretty(&cred).map_err(|e| format!("failed to serialize: {e}"))?;
    fs::write(&path, data).map_err(|e| format!("failed to write credential: {e}"))?;

    // Set restrictive file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    let mut result = json!({
        "stored": name,
        "namespace": namespace,
        "min_tier": min_tier,
        "stored_at": stored_at,
    });
    if let Some(ref exp) = expires_at {
        result["expires_at"] = json!(exp);
    }
    Ok(result)
}

/// Load a credential value.
///
/// Usage: cos credential load <name> [--namespace NS]
fn cmd_load(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let name = rest.first().ok_or("usage: cos credential load <name>")?;
    let path = namespace_dir(&namespace).join(format!("{name}.json"));

    if !path.is_file() {
        return Err(format!("credential not found: {name}"));
    }

    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read credential: {e}"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse credential: {e}"))?;

    // Check tier requirement
    let current_tier = policy::current_tier().unwrap_or(0);
    if current_tier > cred.min_tier {
        return Err(format!(
            "insufficient tier: credential '{}' requires tier {} or higher, current session has tier {}",
            name, cred.min_tier, current_tier
        ));
    }

    // Check expiry
    if is_expired(&cred.expires_at) {
        // Try auto-refresh if refresh_cmd is configured
        if let Some(ref refresh_cmd) = cred.refresh_cmd {
            match execute_refresh(refresh_cmd) {
                Ok(new_value) => {
                    // Re-store the credential with new value and new expiry
                    let ttl = compute_original_ttl(&cred);
                    let (new_value_b64, new_nonce_b64) = encrypt_value(new_value.trim().as_bytes());
                    let now = chrono::Utc::now();
                    let new_expires = ttl.map(|secs| {
                        let exp = now + chrono::Duration::seconds(secs);
                        exp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                    });

                    let updated_cred = StoredCredential {
                        name: cred.name.clone(),
                        namespace: cred.namespace.clone(),
                        value_b64: new_value_b64,
                        nonce_b64: Some(new_nonce_b64),
                        min_tier: cred.min_tier,
                        stored_at: cred.stored_at.clone(),
                        stored_by: cred.stored_by.clone(),
                        expires_at: new_expires.clone(),
                        refresh_cmd: cred.refresh_cmd.clone(),
                    };

                    // Write updated credential back
                    let data = serde_json::to_string_pretty(&updated_cred)
                        .map_err(|e| format!("failed to serialize: {e}"))?;
                    fs::write(&path, data)
                        .map_err(|e| format!("failed to write refreshed credential: {e}"))?;

                    return Ok(json!({
                        "name": name,
                        "namespace": namespace,
                        "value": new_value.trim(),
                        "min_tier": cred.min_tier,
                        "refreshed": true,
                        "expires_at": new_expires,
                    }));
                }
                Err(e) => {
                    return Err(format!(
                        "credential '{}' expired and auto-refresh failed: {}",
                        name, e
                    ));
                }
            }
        }

        // No refresh_cmd — return expired error (existing behavior)
        return Err(serde_json::to_string(&json!({
            "error": format!("credential '{}' has expired", name),
            "expired": true,
            "expires_at": cred.expires_at,
        }))
        .unwrap_or_else(|_| format!("credential '{}' has expired", name)));
    }

    // Decrypt (handles both AES-GCM and legacy XOR)
    let value_bytes = decrypt_value(&cred)?;
    let value = String::from_utf8(value_bytes)
        .map_err(|e| format!("credential is not valid UTF-8: {e}"))?;

    Ok(json!({
        "name": name,
        "namespace": cred.namespace,
        "value": value,
        "min_tier": cred.min_tier,
    }))
}

/// Revoke (delete) a credential.
///
/// Usage: cos credential revoke <name> [--namespace NS]
fn cmd_revoke(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let name = rest.first().ok_or("usage: cos credential revoke <name>")?;
    let path = namespace_dir(&namespace).join(format!("{name}.json"));

    if !path.is_file() {
        return Err(format!("credential not found: {name}"));
    }

    fs::remove_file(&path).map_err(|e| format!("failed to revoke credential: {e}"))?;

    Ok(json!({
        "revoked": name,
        "namespace": namespace,
    }))
}

/// List credentials.
///
/// With `--namespace NS`: list credentials in that namespace.
/// Without `--namespace`: list all namespaces with credential counts.
fn cmd_list(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let (ns_opt, _rest) = parse_namespace_flag(args);

    match ns_opt {
        Some(namespace) => list_namespace(&namespace),
        None => list_all_namespaces(),
    }
}

/// List credentials within a single namespace.
fn list_namespace(namespace: &str) -> Result<Value, String> {
    let dir = namespace_dir(namespace);
    if !dir.exists() {
        return Ok(json!({
            "namespace": namespace,
            "credentials": [],
            "count": 0,
        }));
    }

    let mut credentials: Vec<Value> = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read credentials dir: {e}"))?;

    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".json") {
            continue;
        }
        // Skip the bundles subdirectory
        if entry.path().is_dir() {
            continue;
        }
        if let Ok(data) = fs::read_to_string(entry.path()) {
            if let Ok(cred) = serde_json::from_str::<StoredCredential>(&data) {
                let expired = is_expired(&cred.expires_at);
                let mut entry_json = json!({
                    "name": cred.name,
                    "min_tier": cred.min_tier,
                    "stored_at": cred.stored_at,
                    "stored_by": cred.stored_by,
                    "expired": expired,
                });
                if let Some(ref exp) = cred.expires_at {
                    entry_json["expires_at"] = json!(exp);
                }
                if let Some(ref cmd) = cred.refresh_cmd {
                    entry_json["refresh_cmd"] = json!(cmd);
                }
                credentials.push(entry_json);
            }
        }
    }

    credentials.sort_by(|a, b| {
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    let count = credentials.len();
    Ok(json!({
        "namespace": namespace,
        "credentials": credentials,
        "count": count,
    }))
}

/// List all namespaces and their credential counts.
fn list_all_namespaces() -> Result<Value, String> {
    let dir = credentials_dir();
    if !dir.exists() {
        return Ok(json!({
            "namespaces": [],
            "total": 0,
        }));
    }

    let mut namespaces: Vec<Value> = Vec::new();
    let mut total: usize = 0;

    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read credentials dir: {e}"))?;

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let ns_name = entry.file_name().to_string_lossy().to_string();
        let mut count: usize = 0;
        if let Ok(ns_entries) = fs::read_dir(entry.path()) {
            for ns_entry in ns_entries.flatten() {
                let fname = ns_entry.file_name().to_string_lossy().to_string();
                if fname.ends_with(".json") && ns_entry.path().is_file() {
                    count += 1;
                }
            }
        }
        total += count;
        namespaces.push(json!({
            "namespace": ns_name,
            "count": count,
        }));
    }

    namespaces.sort_by(|a, b| {
        let na = a["namespace"].as_str().unwrap_or("");
        let nb = b["namespace"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    Ok(json!({
        "namespaces": namespaces,
        "total": total,
    }))
}

/// Create a credential bundle — a named group of credential keys.
///
/// Usage: cos credential bundle <bundle-name> --keys key1,key2,key3 [--namespace NS]
fn cmd_bundle(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let mut keys: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--keys" if i + 1 < rest.len() => {
                keys = Some(rest[i + 1].clone());
                i += 2;
            }
            _ => {
                positional.push(rest[i].clone());
                i += 1;
            }
        }
    }

    let bundle_name = positional
        .first()
        .ok_or("usage: cos credential bundle <name> --keys key1,key2,key3 [--namespace NS]")?;

    let keys_str = keys.ok_or("--keys is required (comma-separated list of credential names)")?;
    let key_list: Vec<String> = keys_str.split(',').map(|s| s.trim().to_string()).collect();

    if key_list.is_empty() {
        return Err("--keys must specify at least one credential name".into());
    }

    let dir = bundles_dir(&namespace);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create bundles dir: {e}"))?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let manifest = BundleManifest {
        name: bundle_name.clone(),
        namespace: namespace.clone(),
        keys: key_list.clone(),
        created_at: now.clone(),
    };

    let path = dir.join(format!("{bundle_name}.json"));
    let data = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize bundle: {e}"))?;
    fs::write(&path, data).map_err(|e| format!("failed to write bundle: {e}"))?;

    Ok(json!({
        "bundle": bundle_name,
        "namespace": namespace,
        "keys": key_list,
        "created_at": now,
    }))
}

/// Load all credentials in a bundle as a JSON object.
///
/// Usage: cos credential load-bundle <bundle-name> [--namespace NS]
fn cmd_load_bundle(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let bundle_name = rest
        .first()
        .ok_or("usage: cos credential load-bundle <name> [--namespace NS]")?;

    let path = bundles_dir(&namespace).join(format!("{bundle_name}.json"));
    if !path.is_file() {
        return Err(format!("bundle not found: {bundle_name}"));
    }

    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read bundle: {e}"))?;
    let manifest: BundleManifest =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse bundle: {e}"))?;

    let mut credentials = serde_json::Map::new();
    let mut errors = serde_json::Map::new();

    for key in &manifest.keys {
        let cred_path = namespace_dir(&namespace).join(format!("{key}.json"));
        if !cred_path.is_file() {
            errors.insert(
                key.clone(),
                Value::String(format!("credential not found: {key}")),
            );
            continue;
        }

        let cred_data = match fs::read_to_string(&cred_path) {
            Ok(d) => d,
            Err(e) => {
                errors.insert(key.clone(), Value::String(format!("failed to read: {e}")));
                continue;
            }
        };

        let cred: StoredCredential = match serde_json::from_str(&cred_data) {
            Ok(c) => c,
            Err(e) => {
                errors.insert(key.clone(), Value::String(format!("failed to parse: {e}")));
                continue;
            }
        };

        // Check tier
        let current_tier = policy::current_tier().unwrap_or(0);
        if current_tier > cred.min_tier {
            errors.insert(
                key.clone(),
                Value::String(format!(
                    "insufficient tier: requires {}, have {}",
                    cred.min_tier, current_tier
                )),
            );
            continue;
        }

        // Check expiry
        if is_expired(&cred.expires_at) {
            errors.insert(key.clone(), Value::String("credential has expired".into()));
            continue;
        }

        match decrypt_value(&cred) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(val) => {
                    credentials.insert(key.clone(), Value::String(val));
                }
                Err(e) => {
                    errors.insert(key.clone(), Value::String(format!("not valid UTF-8: {e}")));
                }
            },
            Err(e) => {
                errors.insert(key.clone(), Value::String(e));
            }
        }
    }

    let mut result = json!({
        "bundle": bundle_name,
        "namespace": namespace,
        "credentials": credentials,
    });
    if !errors.is_empty() {
        result["errors"] = Value::Object(errors);
    }
    Ok(result)
}

// ===========================================================================
// Auto-refresh helpers
// ===========================================================================

/// Execute a refresh command and capture its stdout as the new value.
fn execute_refresh(cmd: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};

    #[cfg(unix)]
    let output = Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    #[cfg(not(unix))]
    let output = Command::new("cmd")
        .args(["/c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                let value = String::from_utf8(out.stdout)
                    .map_err(|e| format!("refresh output not valid UTF-8: {e}"))?;
                if value.trim().is_empty() {
                    return Err("refresh command produced empty output".into());
                }
                Ok(value)
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                Err(format!(
                    "refresh command failed (exit {}): {}",
                    out.status.code().unwrap_or(-1),
                    stderr.trim()
                ))
            }
        }
        Err(e) => Err(format!("failed to execute refresh command: {e}")),
    }
}

/// Compute the original TTL from stored_at and expires_at.
fn compute_original_ttl(cred: &StoredCredential) -> Option<i64> {
    let expires_str = cred.expires_at.as_ref()?;
    let stored =
        chrono::DateTime::parse_from_rfc3339(&cred.stored_at.replace('Z', "+00:00")).ok()?;
    let expires = chrono::DateTime::parse_from_rfc3339(&expires_str.replace('Z', "+00:00")).ok()?;
    let duration = expires.signed_duration_since(stored);
    Some(duration.num_seconds())
}

// ===========================================================================
// OAuth refresh
// ===========================================================================

/// Refresh an OAuth token by exchanging a refresh token for a new access token.
///
/// Usage: cos credential oauth-refresh <provider> [--namespace NS]
///
/// Supported providers: google, microsoft
///
/// Reads <PROVIDER>_REFRESH_TOKEN and <PROVIDER>_CLIENT_ID, <PROVIDER>_CLIENT_SECRET
/// from the credential store, exchanges for a new access token, and stores it.
fn cmd_oauth_refresh(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let provider = rest
        .first()
        .ok_or("usage: cos credential oauth-refresh <google|microsoft> [--namespace NS]")?;

    match provider.as_str() {
        "google" => oauth_refresh_google(&namespace),
        "microsoft" => oauth_refresh_microsoft(&namespace),
        _ => Err(format!(
            "unsupported OAuth provider: {provider}. supported: google, microsoft"
        )),
    }
}

fn oauth_refresh_google(namespace: &str) -> Result<Value, String> {
    // Load required credentials
    let refresh_token = load_credential_value("GOOGLE_REFRESH_TOKEN", namespace)?;
    let client_id = load_credential_value("GOOGLE_CLIENT_ID", namespace)?;
    let client_secret = load_credential_value("GOOGLE_CLIENT_SECRET", namespace)?;

    // POST to Google token endpoint
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        urlencoded(&refresh_token),
        urlencoded(&client_id),
        urlencoded(&client_secret),
    );

    let result = http_post(
        "https://oauth2.googleapis.com/token",
        &body,
        "application/x-www-form-urlencoded",
    )?;

    let token_data: serde_json::Value = serde_json::from_str(&result)
        .map_err(|e| format!("failed to parse token response: {e}"))?;

    let access_token = token_data["access_token"]
        .as_str()
        .ok_or("no access_token in response")?;

    let expires_in = token_data["expires_in"].as_u64().unwrap_or(3600);

    // Store the new access token
    cmd_store(&[
        "GOOGLE_ACCESS_TOKEN".into(),
        access_token.into(),
        "--tier".into(),
        "0".into(),
        "--namespace".into(),
        namespace.into(),
        "--ttl".into(),
        expires_in.to_string(),
        "--refresh-cmd".into(),
        format!("cos credential oauth-refresh google --namespace {namespace}"),
    ])?;

    Ok(json!({
        "provider": "google",
        "refreshed": true,
        "expires_in": expires_in,
        "namespace": namespace,
    }))
}

fn oauth_refresh_microsoft(namespace: &str) -> Result<Value, String> {
    let refresh_token = load_credential_value("MICROSOFT_REFRESH_TOKEN", namespace)?;
    let client_id = load_credential_value("MICROSOFT_CLIENT_ID", namespace)?;
    let client_secret = load_credential_value("MICROSOFT_CLIENT_SECRET", namespace)?;
    let tenant_id =
        load_credential_value("MICROSOFT_TENANT_ID", namespace).unwrap_or_else(|_| "common".into());

    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}&scope=https://graph.microsoft.com/.default",
        urlencoded(&refresh_token),
        urlencoded(&client_id),
        urlencoded(&client_secret),
    );

    let url = format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token");

    let result = http_post(&url, &body, "application/x-www-form-urlencoded")?;

    let token_data: serde_json::Value = serde_json::from_str(&result)
        .map_err(|e| format!("failed to parse token response: {e}"))?;

    let access_token = token_data["access_token"]
        .as_str()
        .ok_or("no access_token in response")?;

    let expires_in = token_data["expires_in"].as_u64().unwrap_or(3600);

    // Also store new refresh token if returned (Microsoft rotates them)
    if let Some(new_refresh) = token_data["refresh_token"].as_str() {
        cmd_store(&[
            "MICROSOFT_REFRESH_TOKEN".into(),
            new_refresh.into(),
            "--tier".into(),
            "0".into(),
            "--namespace".into(),
            namespace.into(),
        ])?;
    }

    cmd_store(&[
        "MICROSOFT_ACCESS_TOKEN".into(),
        access_token.into(),
        "--tier".into(),
        "0".into(),
        "--namespace".into(),
        namespace.into(),
        "--ttl".into(),
        expires_in.to_string(),
        "--refresh-cmd".into(),
        format!("cos credential oauth-refresh microsoft --namespace {namespace}"),
    ])?;

    Ok(json!({
        "provider": "microsoft",
        "refreshed": true,
        "expires_in": expires_in,
        "namespace": namespace,
    }))
}

// ===========================================================================
// HTTP and encoding helpers
// ===========================================================================

/// Simple URL-encoded POST using stdlib only.
fn http_post(url: &str, body: &str, content_type: &str) -> Result<String, String> {
    use std::process::{Command, Stdio};

    // Use curl since we can't do HTTP from Rust without dependencies
    let output = Command::new("curl")
        .args([
            "-s",
            "-S",
            "-X",
            "POST",
            "-H",
            &format!("Content-Type: {content_type}"),
            "-d",
            body,
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to execute curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("HTTP POST failed: {}", stderr.trim()));
    }

    String::from_utf8(output.stdout).map_err(|e| format!("response not valid UTF-8: {e}"))
}

/// Simple percent-encoding for URL form data.
fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

/// Load a credential value from the store (helper for oauth-refresh).
fn load_credential_value(name: &str, namespace: &str) -> Result<String, String> {
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    if !path.is_file() {
        return Err(format!(
            "credential not found: {name} (namespace: {namespace}). Store it with: cos credential store {name} <value> --namespace {namespace}"
        ));
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read: {e}"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse: {e}"))?;

    match decrypt_value(&cred) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}")),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Once,
    };

    static INIT: Once = Once::new();
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// All tests share one COS_DATA_DIR (set once). Each test uses unique
    /// credential names so there is no cross-test interference.
    fn unique_name(prefix: &str) -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{n}")
    }

    fn setup() {
        INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            std::env::set_var("COS_DATA_DIR", &dir);
        });
        std::env::remove_var("COS_SESSION");
    }

    // ---- SHA-256 ----------------------------------------------------------

    #[test]
    fn sha256_known_vectors() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb924...
        let empty = sha256::hash(b"");
        assert_eq!(
            &empty[..4],
            &[0xe3, 0xb0, 0xc4, 0x42],
            "SHA-256 empty string"
        );

        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223...
        let abc = sha256::hash(b"abc");
        assert_eq!(&abc[..4], &[0xba, 0x78, 0x16, 0xbf], "SHA-256 of 'abc'");
    }

    // ---- AES-256-GCM ------------------------------------------------------

    #[test]
    fn aes_gcm_encrypt_decrypt_roundtrip() {
        let key = sha256::hash(b"test-key-for-aes-gcm");
        let nonce = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let plaintext = b"hello, AES-256-GCM world!";

        let ct = aes_gcm::encrypt(&key, &nonce, plaintext);
        // ct should be plaintext.len() + 16 (tag) bytes
        assert_eq!(ct.len(), plaintext.len() + 16);

        let pt = aes_gcm::decrypt(&key, &nonce, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_gcm_tampered_ciphertext_fails() {
        let key = sha256::hash(b"test-key-tamper");
        let nonce = [0u8; 12];
        let ct = aes_gcm::encrypt(&key, &nonce, b"secret");

        let mut tampered = ct.clone();
        tampered[0] ^= 0xff;
        assert!(aes_gcm::decrypt(&key, &nonce, &tampered).is_err());
    }

    #[test]
    fn aes_gcm_empty_plaintext() {
        let key = sha256::hash(b"empty-test");
        let nonce = [42u8; 12];
        let ct = aes_gcm::encrypt(&key, &nonce, b"");
        assert_eq!(ct.len(), 16); // tag only
        let pt = aes_gcm::decrypt(&key, &nonce, &ct).unwrap();
        assert!(pt.is_empty());
    }

    // ---- Base64 -----------------------------------------------------------

    #[test]
    fn b64_roundtrip() {
        let data = b"hello world 12345!@#$%";
        let encoded = to_b64(data);
        let decoded = from_b64(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    // ---- Legacy XOR backward compatibility --------------------------------

    #[test]
    fn legacy_xor_backward_compat() {
        setup();
        let name = unique_name("legacy-xor");
        let namespace = "default";
        let plain = "legacy-secret-value";

        // Manually create a legacy-format credential (no nonce_b64, XOR-obfuscated).
        let key = legacy_obfuscation_key();
        let obfuscated: Vec<u8> = plain
            .as_bytes()
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % key.len()])
            .collect();
        let value_b64 = to_b64(&obfuscated);

        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let cred = StoredCredential {
            name: name.clone(),
            namespace: namespace.into(),
            value_b64,
            nonce_b64: None, // legacy — no nonce
            min_tier: 1,
            stored_at: now,
            stored_by: None,
            expires_at: None,
            refresh_cmd: None,
        };

        let dir = namespace_dir(namespace);
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.json"));
        let data = serde_json::to_string_pretty(&cred).unwrap();
        fs::write(&path, data).unwrap();

        // Load it through the normal path — should still work.
        let r = cmd_load(&[name.clone()]).unwrap();
        assert_eq!(r["value"], plain);
    }

    // ---- Store and load ---------------------------------------------------

    #[test]
    fn store_and_load() {
        setup();
        let name = unique_name("store-load");

        let r = cmd_store(&[
            name.clone(),
            "secret-value-123".into(),
            "--tier".into(),
            "1".into(),
        ])
        .unwrap();
        assert_eq!(r["stored"], name);
        assert_eq!(r["min_tier"], 1);
        assert_eq!(r["namespace"], "default");

        let r = cmd_load(&[name.clone()]).unwrap();
        assert_eq!(r["name"], name);
        assert_eq!(r["value"], "secret-value-123");
    }

    // ---- Revoke -----------------------------------------------------------

    #[test]
    fn revoke_removes_credential() {
        setup();
        let name = unique_name("revoke");

        cmd_store(&[name.clone(), "temp-value".into()]).unwrap();
        let r = cmd_revoke(&[name.clone()]).unwrap();
        assert_eq!(r["revoked"], name);

        let r = cmd_load(&[name.clone()]);
        assert!(r.is_err());
    }

    // ---- List (namespace) -------------------------------------------------

    #[test]
    fn list_shows_names_only() {
        setup();
        let a = unique_name("list-a");
        let b = unique_name("list-b");

        cmd_store(&[a.clone(), "val-a".into()]).unwrap();
        cmd_store(&[b.clone(), "val-b".into()]).unwrap();

        let r = cmd_list(&["--namespace".into(), "default".into()]).unwrap();
        assert!(r["count"].as_u64().unwrap() >= 2);
        let creds = r["credentials"].as_array().unwrap();
        for c in creds {
            assert!(c.get("value").is_none(), "values must not appear in list");
            assert!(c["name"].is_string());
        }
    }

    #[test]
    fn list_all_namespaces() {
        setup();
        let name = unique_name("ns-list");
        cmd_store(&[
            name.clone(),
            "val".into(),
            "--namespace".into(),
            "alpha".into(),
        ])
        .unwrap();

        let r = cmd_list(&[]).unwrap();
        let nss = r["namespaces"].as_array().unwrap();
        let names: Vec<&str> = nss.iter().filter_map(|n| n["namespace"].as_str()).collect();
        assert!(names.contains(&"alpha"), "alpha namespace should be listed");
    }

    // ---- Validation -------------------------------------------------------

    #[test]
    fn store_invalid_name() {
        setup();
        let r = cmd_store(&["bad/name".into(), "val".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn load_nonexistent() {
        setup();
        let name = unique_name("nonexistent");
        let r = cmd_load(&[name]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not found"));
    }

    // ---- Namespace isolation ----------------------------------------------

    #[test]
    fn namespace_isolation() {
        setup();
        let name = unique_name("ns-iso");

        // Store in namespace A
        cmd_store(&[
            name.clone(),
            "value-a".into(),
            "--namespace".into(),
            "ns-a".into(),
        ])
        .unwrap();

        // Store same name in namespace B with different value
        cmd_store(&[
            name.clone(),
            "value-b".into(),
            "--namespace".into(),
            "ns-b".into(),
        ])
        .unwrap();

        let ra = cmd_load(&[name.clone(), "--namespace".into(), "ns-a".into()]).unwrap();
        let rb = cmd_load(&[name.clone(), "--namespace".into(), "ns-b".into()]).unwrap();
        assert_eq!(ra["value"], "value-a");
        assert_eq!(rb["value"], "value-b");
    }

    // ---- TTL / expiry -----------------------------------------------------

    #[test]
    fn ttl_expired_credential() {
        setup();
        let name = unique_name("ttl-exp");

        // Store with TTL = 0 (expires immediately)
        // We achieve "already expired" by writing directly with a past expires_at.
        let (value_b64, nonce_b64) = encrypt_value(b"will-expire");
        let cred = StoredCredential {
            name: name.clone(),
            namespace: "default".into(),
            value_b64,
            nonce_b64: Some(nonce_b64),
            min_tier: 1,
            stored_at: "2020-01-01T00:00:00Z".into(),
            stored_by: None,
            expires_at: Some("2020-01-01T00:00:01Z".into()), // already past
            refresh_cmd: None,
        };
        let dir = namespace_dir("default");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(format!("{name}.json"));
        fs::write(&path, serde_json::to_string_pretty(&cred).unwrap()).unwrap();

        let r = cmd_load(&[name.clone()]);
        assert!(r.is_err());
        let err = r.unwrap_err();
        assert!(
            err.contains("expired"),
            "error should mention expiry: {err}"
        );
    }

    #[test]
    fn ttl_not_expired_credential() {
        setup();
        let name = unique_name("ttl-ok");

        // Store with large TTL — should still be valid.
        cmd_store(&[
            name.clone(),
            "still-valid".into(),
            "--ttl".into(),
            "86400".into(), // 24 hours
        ])
        .unwrap();

        let r = cmd_load(&[name.clone()]).unwrap();
        assert_eq!(r["value"], "still-valid");
    }

    #[test]
    fn list_shows_expiry() {
        setup();
        let name = unique_name("list-exp");

        cmd_store(&[name.clone(), "v".into(), "--ttl".into(), "3600".into()]).unwrap();

        let r = cmd_list(&["--namespace".into(), "default".into()]).unwrap();
        let creds = r["credentials"].as_array().unwrap();
        let found = creds.iter().find(|c| c["name"].as_str() == Some(&name));
        assert!(found.is_some(), "credential should appear in list");
        let c = found.unwrap();
        assert!(c["expires_at"].is_string());
        assert_eq!(c["expired"], false);
    }

    // ---- Bundles ----------------------------------------------------------

    #[test]
    fn bundle_create_and_load() {
        setup();
        let k1 = unique_name("bk1");
        let k2 = unique_name("bk2");
        let bundle = unique_name("bundle");

        cmd_store(&[k1.clone(), "val1".into()]).unwrap();
        cmd_store(&[k2.clone(), "val2".into()]).unwrap();

        let r = cmd_bundle(&[bundle.clone(), "--keys".into(), format!("{k1},{k2}")]).unwrap();
        assert_eq!(r["bundle"], bundle.as_str());

        let r = cmd_load_bundle(&[bundle.clone()]).unwrap();
        assert_eq!(r["credentials"][&k1], "val1");
        assert_eq!(r["credentials"][&k2], "val2");
        assert!(r.get("errors").is_none());
    }

    #[test]
    fn bundle_with_missing_key() {
        setup();
        let k1 = unique_name("bkm1");
        let missing = unique_name("bkm-missing");
        let bundle = unique_name("bundle-miss");

        cmd_store(&[k1.clone(), "present".into()]).unwrap();

        cmd_bundle(&[bundle.clone(), "--keys".into(), format!("{k1},{missing}")]).unwrap();

        let r = cmd_load_bundle(&[bundle.clone()]).unwrap();
        assert_eq!(r["credentials"][&k1], "present");
        assert!(
            r["errors"][&missing].is_string(),
            "missing key should have an error"
        );
    }

    // ---- Dispatch ---------------------------------------------------------

    #[test]
    fn run_dispatch() {
        setup();
        let name = unique_name("dispatch");

        let r = run("store", &[name.clone(), "val".into()]).unwrap();
        assert_eq!(r["stored"], name);

        let r = run("list", &["--namespace".into(), "default".into()]).unwrap();
        assert!(r["count"].as_u64().unwrap() >= 1);

        let r = run("bogus", &[]);
        assert!(r.is_err());
    }

    #[test]
    fn run_dispatch_bundle_commands() {
        setup();
        let k = unique_name("dispk");
        let b = unique_name("dispb");

        run("store", &[k.clone(), "v".into()]).unwrap();
        run("bundle", &[b.clone(), "--keys".into(), k.clone()]).unwrap();
        let r = run("load-bundle", &[b.clone()]).unwrap();
        assert_eq!(r["credentials"][&k], "v");
    }

    // ---- Auto-refresh -----------------------------------------------------

    #[test]
    fn store_with_refresh_cmd() {
        setup();
        let name = unique_name("refresh-store");
        let r = cmd_store(&[
            name.clone(),
            "initial-value".into(),
            "--ttl".into(),
            "3600".into(),
            "--refresh-cmd".into(),
            "echo new-value".into(),
        ])
        .unwrap();
        assert_eq!(r["stored"], name);

        // Verify refresh_cmd is stored
        let path = namespace_dir("default").join(format!("{name}.json"));
        let data = fs::read_to_string(&path).unwrap();
        let cred: StoredCredential = serde_json::from_str(&data).unwrap();
        assert_eq!(cred.refresh_cmd.as_deref(), Some("echo new-value"));
    }

    #[test]
    fn load_auto_refresh_on_expiry() {
        setup();
        let name = unique_name("auto-refresh");

        // Store with very short TTL and a refresh command
        cmd_store(&[
            name.clone(),
            "old-value".into(),
            "--ttl".into(),
            "0".into(), // expires immediately
            "--refresh-cmd".into(),
            "echo refreshed-value".into(),
        ])
        .unwrap();

        // Small sleep to ensure expiry
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Load should auto-refresh
        let r = cmd_load(&[name.clone()]).unwrap();
        assert_eq!(r["value"], "refreshed-value");
        assert_eq!(r["refreshed"], true);
    }

    #[test]
    fn load_expired_no_refresh_cmd_fails() {
        setup();
        let name = unique_name("no-refresh");
        cmd_store(&[name.clone(), "val".into(), "--ttl".into(), "0".into()]).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(100));

        let r = cmd_load(&[name.clone()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("expired"));
    }

    // ---- URL encoding -----------------------------------------------------

    #[test]
    fn urlencoded_special_chars() {
        assert_eq!(urlencoded("hello world"), "hello%20world");
        assert_eq!(urlencoded("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(urlencoded("simple"), "simple");
    }

    // ---- TTL computation --------------------------------------------------

    #[test]
    fn compute_ttl_from_timestamps() {
        let cred = StoredCredential {
            name: "test".into(),
            namespace: "default".into(),
            value_b64: String::new(),
            nonce_b64: None,
            min_tier: 0,
            stored_at: "2026-03-25T10:00:00Z".into(),
            stored_by: None,
            expires_at: Some("2026-03-25T11:00:00Z".into()),
            refresh_cmd: None,
        };
        let ttl = compute_original_ttl(&cred);
        assert_eq!(ttl, Some(3600));
    }

    // ---- OAuth dispatch ---------------------------------------------------

    #[test]
    fn oauth_refresh_unknown_provider() {
        setup();
        let r = cmd_oauth_refresh(&["unknown".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unsupported"));
    }

    #[test]
    fn oauth_refresh_missing_provider() {
        setup();
        let r = cmd_oauth_refresh(&[]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("usage"));
    }
}
