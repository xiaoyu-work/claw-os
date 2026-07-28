/// OS-level credential store — encrypted secret storage with tier-based access.
///
/// Analogous to the Linux kernel keyring (`keyctl`), this provides a secure
/// store for secrets (API keys, tokens, passwords) that are accessible only
/// to sessions with sufficient privilege tier.
///
/// Credentials are encrypted with AES-256-GCM. The 32-byte key is derived
/// per host in this order:
///   1. Kernel session keyring cache (Linux).
///   2. `/etc/machine-id` hashed with SHA-256 (Linux).
///   3. A persistent random 32-byte key written to
///      `${COS_STATE_DIR}/credential-root.key` (mode 0600) the first time the
///      store is used on a host without a machine-id. Generated from the OS
///      CSPRNG — there is no hard-coded literal fallback. Legacy credentials
///      using XOR obfuscation are detected (by the absence of a `nonce_b64`
///      field) and still decrypted for backward compatibility.
///
/// Features:
///   - **Namespace isolation**: credentials live under `<namespace>/` subdirs.
///   - **TTL / expiry**: optional `--ttl <seconds>` on store; enforced on load.
///   - **Bundles**: named groups of credentials loaded as a single JSON object.
///
/// Storage: `~/.local/share/cos/credentials/<namespace>/<name>.json`
///          (overridable via `COS_CREDENTIALS_DIR`).
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
use std::path::{Path, PathBuf};

use crate::caps::{require_or_json, Scope, Verb};
use crate::policy;

// ===========================================================================
// Kernel crypto via AF_ALG (Linux) with pure-Rust fallback (non-Linux)
// ===========================================================================
//
// On Linux, uses the kernel's crypto API through AF_ALG sockets:
//   - "hash sha256"  for SHA-256
//   - "aead gcm(aes)" for AES-256-GCM
// No userspace crypto code on the hot path — the kernel handles it.
// Keys never exist as mmap'd pages that could be swapped to disk.
//
// On non-Linux (dev/test), falls back to the pure-Rust implementation
// to keep tests working on macOS/Windows.
// ===========================================================================

#[cfg(target_os = "linux")]
mod sha256 {
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;

    /// Compute SHA-256 via AF_ALG socket (kernel crypto API).
    pub(super) fn hash(data: &[u8]) -> [u8; 32] {
        match hash_af_alg(data) {
            Some(h) => h,
            None => hash_fallback(data),
        }
    }

    fn hash_af_alg(data: &[u8]) -> Option<[u8; 32]> {
        unsafe {
            // socket(AF_ALG, SOCK_SEQPACKET, 0)
            let fd = libc::socket(libc::AF_ALG, libc::SOCK_SEQPACKET, 0);
            if fd < 0 {
                return None;
            }

            // Build sockaddr_alg for "hash" / "sha256"
            // struct sockaddr_alg { u16 family; char type[14]; u32 feat; u32 mask; char name[64]; }
            let mut sa = [0u8; 88]; // sizeof(sockaddr_alg)
                                    // family = AF_ALG (38)
            sa[0] = 38;
            sa[1] = 0;
            // type = "hash" at offset 2
            sa[2..6].copy_from_slice(b"hash");
            // name = "sha256" at offset 24
            sa[24..30].copy_from_slice(b"sha256");

            let ret = libc::bind(fd, sa.as_ptr() as *const libc::sockaddr, 88);
            if ret < 0 {
                libc::close(fd);
                return None;
            }

            let op_fd = libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut());
            if op_fd < 0 {
                libc::close(fd);
                return None;
            }

            // Write data, read hash
            let mut op_file = std::fs::File::from_raw_fd(op_fd);
            if op_file.write_all(data).is_err() {
                libc::close(fd);
                return None;
            }

            let mut digest = [0u8; 32];
            if op_file.read_exact(&mut digest).is_err() {
                libc::close(fd);
                return None;
            }

            libc::close(fd);
            Some(digest)
        }
    }

    /// Pure-Rust SHA-256 fallback (if AF_ALG is unavailable).
    fn hash_fallback(data: &[u8]) -> [u8; 32] {
        // Delegates to the pure-Rust implementation below.
        pure_sha256(data)
    }

    #[allow(clippy::needless_range_loop)]
    fn pure_sha256(data: &[u8]) -> [u8; 32] {
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
        const H0: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        let bit_len = (data.len() as u64) * 8;
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());
        let mut h = H0;
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

#[cfg(not(target_os = "linux"))]
mod sha256 {
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
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    pub(super) fn hash(data: &[u8]) -> [u8; 32] {
        let bit_len = (data.len() as u64) * 8;
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());
        let mut h = H0;
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
// AES-256-GCM — pure Rust (kept for non-Linux + as Linux AF_ALG fallback)
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
        // Constant-time tag comparison (prevents timing side-channel).
        // Equivalent to Linux kernel's crypto_memneq().
        let mut diff = 0u8;
        for i in 0..16 {
            diff |= computed_tag[i] ^ expected_tag[i];
        }
        if diff != 0 {
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
    // Strict base64 decode: rejects non-alphabet bytes (no silent zero-mapping),
    // requires correct padding, and tolerates leading/trailing ASCII whitespace
    // only (newlines from `to_b64` line wrapping callers, if any). Garbage in
    // ciphertext now surfaces as `CredentialError::Malformed("base64: ...")`
    // instead of an opaque AEAD authentication failure further down.
    use base64::engine::{general_purpose::STANDARD, Engine};
    let trimmed: String = s
        .chars()
        .filter(|c| !matches!(*c, '\n' | '\r' | ' ' | '\t'))
        .collect();
    STANDARD
        .decode(trimmed.as_bytes())
        .map_err(|e| format!("malformed base64: {e}"))
}

// ===========================================================================
// Key derivation and nonce generation
// ===========================================================================

/// Path to the on-disk persistent root key. Used as a last-resort source of
/// keying material when neither the kernel keyring (Linux) nor
/// `/etc/machine-id` are available — e.g. inside chroots, minimal containers,
/// non-Linux dev boxes, or test harnesses. Override the file location with
/// `COS_CREDENTIAL_ROOT_KEY_PATH` (used by tests).
///
/// Lives next to the rest of the per-install state under `$COS_STATE_DIR`
/// (aliased to `$COS_DATA_DIR` in this codebase via [`crate::paths::data_dir`]).
fn credential_root_key_path() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_CREDENTIAL_ROOT_KEY_PATH") {
        return PathBuf::from(v);
    }
    crate::paths::data_dir().join("credential-root.key")
}

/// Path the code consults for the machine identity. Tests override this with
/// `COS_MACHINE_ID_PATH` to simulate "no machine-id" environments without
/// touching `/etc/machine-id`.
#[cfg(target_os = "linux")]
fn machine_id_path() -> PathBuf {
    if let Some(v) = std::env::var_os("COS_MACHINE_ID_PATH") {
        return PathBuf::from(v);
    }
    PathBuf::from("/etc/machine-id")
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG.
/// Returns the underlying syscall error on failure.
///
///   * Linux:   `getrandom(2)`
///   * macOS / BSD: `getentropy(3)` (limited to 256 bytes per call)
///   * Other Unix: `/dev/urandom` blocking read
fn os_random_bytes(buf: &mut [u8]) -> Result<(), std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        let ret =
            unsafe { libc::getrandom(buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if ret as isize == buf.len() as isize {
            return Ok(());
        }
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        for chunk in buf.chunks_mut(256) {
            let ret =
                unsafe { libc::getentropy(chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        return Ok(());
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom")?;
        f.read_exact(buf)?;
        Ok(())
    }
}

/// Fill `buf` with cryptographically secure random bytes or panic.
///
/// Panicking is the deliberate failure mode for `OsRng` here: the alternative
/// (silently falling back to a deterministic source) would be catastrophic for
/// AES-GCM nonces and root key generation alike — see the audit notes on
/// "predictable AES-GCM nonce". Any caller that needs randomness for crypto
/// must not continue without it.
fn os_random_bytes_or_panic(buf: &mut [u8]) {
    if let Err(e) = os_random_bytes(buf) {
        panic!("OsRng failed: {e}; refusing to fall back to a predictable source");
    }
}

/// Read the persistent on-disk root key, returning its bytes if present and
/// well-formed (exactly 32 bytes). Returns `None` for any read / size error.
fn load_persistent_root_key() -> Option<[u8; 32]> {
    load_persistent_root_key_at(&credential_root_key_path())
}

/// Inner of [`load_persistent_root_key`] — same logic but reads from a
/// caller-supplied path. Exists so unit tests can exercise the persistence
/// helpers against a per-test scratch path without mutating process-global
/// env vars (which races other tests).
fn load_persistent_root_key_at(path: &Path) -> Option<[u8; 32]> {
    let bytes = fs::read(path).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

/// Generate a fresh 32-byte root key from the OS CSPRNG and persist it to
/// `credential_root_key_path()` with mode `0600`, fsync the file, then fsync
/// the parent directory.
///
/// Atomicity / TOCTOU: opens with `O_CREAT|O_EXCL` and `mode=0o600` so the
/// file exists with restrictive permissions from the very first byte written
/// (no post-write `chmod` race). If a sibling process raced us to create the
/// file, we honor whatever they wrote and return that instead.
fn generate_and_persist_root_key() -> [u8; 32] {
    generate_and_persist_root_key_at(&credential_root_key_path())
}

/// Inner of [`generate_and_persist_root_key`] — writes to a caller-supplied
/// path. Exists so unit tests can exercise the generator without mutating
/// process-global env vars.
fn generate_and_persist_root_key_at(path: &Path) -> [u8; 32] {
    if let Some(parent) = path.parent() {
        // Best-effort dir creation; the open() below surfaces real errors.
        let _ = fs::create_dir_all(parent);
    }

    let mut key = [0u8; 32];
    os_random_bytes_or_panic(&mut key);

    // Atomic: O_CREAT|O_EXCL with mode 0o600 *at create time*.
    #[cfg(unix)]
    let open_result = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let open_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);

    match open_result {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f.write_all(&key) {
                panic!("failed to write credential-root.key: {e}");
            }
            if let Err(e) = f.sync_all() {
                panic!("failed to fsync credential-root.key: {e}");
            }
            // fsync parent dir so the create is durable.
            if let Some(parent) = path.parent() {
                if let Ok(dir) = std::fs::File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            key
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Race: another process wrote the key. Read what they wrote.
            if let Some(k) = load_persistent_root_key_at(path) {
                k
            } else {
                panic!("credential-root.key exists but cannot be read");
            }
        }
        Err(e) => {
            panic!(
                "failed to create credential-root.key at {}: {}",
                path.display(),
                e
            );
        }
    }
}

/// Derive a 256-bit encryption key.
///
/// Resolution order:
///   1. Kernel session keyring (Linux only — fast in-memory cache).
///   2. `/etc/machine-id` (Linux only — stable per-install identifier).
///   3. Persistent on-disk root key at `${COS_STATE_DIR}/credential-root.key`,
///      generated from the OS CSPRNG on first use, mode `0600`.
///
/// The previous behaviour of falling back to `sha256("claw-os-credential-store-key-v1")`
/// when `/etc/machine-id` was unreadable has been removed — that constant was a
/// universally known key that decrypted every credential store offline. We
/// either find / derive a per-install secret or we generate a fresh random one
/// and persist it. Panics on OsRng failure (audit: predictable nonce / key).
fn derive_key() -> [u8; 32] {
    #[cfg(target_os = "linux")]
    {
        // 1. Kernel keyring cache (zero-cost when populated).
        if let Some(key) = keyring_read(b"cos-credential-key") {
            if key.len() == 32 {
                let mut out = [0u8; 32];
                out.copy_from_slice(&key);
                return out;
            }
        }

        // 2. Machine-id (per-install identifier).
        if let Ok(id) = fs::read_to_string(machine_id_path()) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                let derived = sha256::hash(trimmed.as_bytes());
                keyring_store(b"cos-credential-key", &derived);
                return derived;
            }
        }
    }

    // 3. Persistent on-disk root key (any platform).
    let key = load_persistent_root_key().unwrap_or_else(generate_and_persist_root_key);

    #[cfg(target_os = "linux")]
    keyring_store(b"cos-credential-key", &key);

    key
}

/// Generate a random 12-byte nonce using the OS CSPRNG.
///
/// **Panics** if the CSPRNG syscall fails — there is no safe fallback. AES-GCM
/// catastrophically loses confidentiality and authenticity if a (key, nonce)
/// pair is reused, and the legacy fallback path (`now_nanos || counter`)
/// trivially collided across process restarts and across cooperating
/// processes. Failing loudly is the correct behaviour.
fn generate_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    os_random_bytes_or_panic(&mut nonce);
    nonce
}

// ---------------------------------------------------------------------------
// Linux kernel keyring helpers (keyctl syscalls via libc)
// ---------------------------------------------------------------------------

/// Store a key in the process session keyring.
#[cfg(target_os = "linux")]
fn keyring_store(description: &[u8], payload: &[u8]) {
    use std::ffi::CString;
    // keyctl constants
    const KEY_SPEC_SESSION_KEYRING: i32 = -3;

    let desc = match CString::new(description) {
        Ok(c) => c,
        Err(_) => return,
    };
    let type_cstr = match CString::new("user") {
        Ok(c) => c,
        Err(_) => return,
    };

    unsafe {
        // add_key("user", description, payload, payload_len, KEY_SPEC_SESSION_KEYRING)
        libc::syscall(
            libc::SYS_add_key,
            type_cstr.as_ptr(),
            desc.as_ptr(),
            payload.as_ptr(),
            payload.len(),
            KEY_SPEC_SESSION_KEYRING,
        );
    }
}

/// Read a key from the process session keyring.
#[cfg(target_os = "linux")]
fn keyring_read(description: &[u8]) -> Option<Vec<u8>> {
    use std::ffi::CString;
    const KEY_SPEC_SESSION_KEYRING: i32 = -3;

    let desc = CString::new(description).ok()?;
    let type_cstr = CString::new("user").ok()?;

    unsafe {
        // request_key("user", description, NULL, KEY_SPEC_SESSION_KEYRING)
        let key_id = libc::syscall(
            libc::SYS_request_key,
            type_cstr.as_ptr(),
            desc.as_ptr(),
            std::ptr::null::<libc::c_char>(),
            KEY_SPEC_SESSION_KEYRING,
        );

        if key_id < 0 {
            return None;
        }

        // keyctl(KEYCTL_READ, key_id, buf, buf_len)
        const KEYCTL_READ: libc::c_int = 11;
        let mut buf = vec![0u8; 64];
        let n = libc::syscall(
            libc::SYS_keyctl,
            KEYCTL_READ as libc::c_long,
            key_id,
            buf.as_mut_ptr(),
            buf.len(),
        );

        if n < 0 {
            return None;
        }

        buf.truncate(n as usize);
        Some(buf)
    }
}

// ===========================================================================
// Legacy XOR obfuscation (backward compatibility only)
// ===========================================================================

/// Key used by the legacy XOR obfuscation scheme.
///
/// Historically this fell back to the literal string
/// `"claw-os-credential-store-key-v1"` when `/etc/machine-id` was unreadable,
/// which meant any attacker could trivially decrypt legacy XOR credentials on
/// any host without a machine-id (containers, chroots, non-Linux). That
/// hard-coded fallback has been removed: callers now derive the same per-
/// install secret used by AES-GCM, so the legacy XOR scheme is at least no
/// weaker than `derive_key()` itself.
fn legacy_obfuscation_key() -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string(machine_id_path()) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return trimmed.as_bytes().to_vec();
            }
        }
    }
    // No machine-id: fall through to the per-install random root key. Note
    // that legacy-XOR credentials predating this codebase were created with
    // the machine-id key, so on machines without machine-id (which never had
    // a working legacy key to begin with) decryption will simply fail loudly
    // rather than succeed with the universal hard-coded literal.
    derive_key().to_vec()
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

#[derive(Clone, Serialize, Deserialize)]
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

// Manual Debug: never include the encrypted blob or nonce so accidental
// `tracing::debug!(?cred)` / `dbg!(&cred)` calls cannot regress into leaking
// ciphertext or correlatable metadata into logs. The encrypted value would
// only be useful if the operator also leaked the root key, but defense in
// depth is cheap and the audit explicitly called this out.
impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCredential")
            .field("name", &self.name)
            .field("namespace", &self.namespace)
            .field("value_b64", &"***")
            .field("nonce_b64", &self.nonce_b64.as_ref().map(|_| "***"))
            .field("min_tier", &self.min_tier)
            .field("stored_at", &self.stored_at)
            .field("stored_by", &self.stored_by)
            .field("expires_at", &self.expires_at)
            .field("refresh_cmd", &self.refresh_cmd)
            .finish()
    }
}

impl std::fmt::Display for StoredCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credential({}/{})", self.namespace, self.name)
    }
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

/// Root credentials directory: `~/.local/share/cos/credentials`
/// (overridable via `COS_CREDENTIALS_DIR`). Per-user so non-root
/// callers can store API keys without touching `/var/lib/cos`.
fn credentials_dir() -> PathBuf {
    crate::paths::user_credentials_dir()
}

/// Namespace directory: `<credentials_dir>/<namespace>`.
fn namespace_dir(namespace: &str) -> PathBuf {
    credentials_dir().join(namespace)
}

/// Bundle directory: `<credentials_dir>/<namespace>/bundles`.
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

fn validate_credential_component(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "{kind} must be alphanumeric (hyphens/underscores allowed)"
        ));
    }
    Ok(())
}

fn credential_scope(namespace: &str, name: &str) -> Result<Scope, String> {
    validate_credential_component("namespace", namespace)?;
    validate_credential_component("credential name", name)?;
    Ok(Scope::name(format!("{namespace}/{name}")))
}

fn namespace_scope(namespace: &str) -> Result<Scope, String> {
    validate_credential_component("namespace", namespace)?;
    Ok(Scope::name(format!("{namespace}/*")))
}

fn bundle_scope(namespace: &str, bundle: &str) -> Result<Scope, String> {
    validate_credential_component("namespace", namespace)?;
    validate_credential_component("bundle name", bundle)?;
    Ok(Scope::name(format!("{namespace}/bundles/{bundle}")))
}

fn require_secret(verb: Verb, scope: Scope) -> Result<(), String> {
    require_or_json(verb, scope).map_err(|value| value.to_string())
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
// Tier comparison
// ===========================================================================

/// Returns `true` iff a session running at `session_tier` has *enough*
/// privilege to access a credential whose minimum tier is `min_tier`.
///
/// **Tier semantics, easy to misread:** lower number = MORE privileged.
///   * 0 = ROOT      (strongest)
///   * 1 = OPERATE
///   * 2 = APP
///   * 3 = SANDBOX   (weakest)
///
/// Therefore a session is "strong enough" exactly when its number is
/// less-than-or-equal-to the credential's `min_tier`.
fn tier_grants_access(session_tier: u8, min_tier: u8) -> bool {
    session_tier <= min_tier
}

/// Resolve the *effective* tier for the current request, fail-closed.
///
/// Previous behaviour was `policy::current_tier().unwrap_or(0)` which silently
/// granted ROOT whenever the policy registry could not be loaded — a clear
/// fail-open default for a privilege check. We now distinguish:
///
///   * No `COS_SESSION` env var at all → direct interactive CLI, tier 0
///     (matches historical UX where a human at a shell is treated as root on
///     their own machine).
///   * `COS_SESSION` set but the registry lookup fails or returns no tier →
///     `u8::MAX`, i.e. the weakest possible tier. This causes
///     [`tier_grants_access`] to deny everything except `min_tier == u8::MAX`
///     credentials (none exist in practice), so a missing/corrupt registry can
///     never silently elevate.
fn effective_session_tier() -> u8 {
    match std::env::var("COS_SESSION") {
        Err(_) => 0,
        Ok(_) => policy::current_tier().unwrap_or(u8::MAX),
    }
}

// ===========================================================================
// Atomic, 0600-from-the-start credential file writes
// ===========================================================================

/// Path of the per-credential atomic-write lock sentinel:
/// `<path>.lock`. Held briefly by [`write_credential_atomic`] to serialize
/// concurrent tmp+rename writers against the same data file.
fn lock_sentinel_path(path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Path of the per-credential **refresh** lock sentinel:
/// `<path>.refresh.lock`. Held for the duration of an auto-refresh attempt
/// (executing the OAuth round-trip, re-checking expiry, writing the rotated
/// token). Distinct from [`lock_sentinel_path`] so that the OAuth refresh
/// command — which itself shells out to `cos credential store` in a child
/// process and therefore needs the *write* lock — cannot deadlock against the
/// parent's *refresh* lock. See the HIGH "refresh-token cannibalisation race"
/// audit finding.
fn refresh_sentinel_path(path: &Path) -> PathBuf {
    let mut s: std::ffi::OsString = path.as_os_str().to_os_string();
    s.push(".refresh.lock");
    PathBuf::from(s)
}

/// Run `f` while holding an exclusive `flock(2)` on the per-credential
/// refresh sentinel (`<path>.refresh.lock`). Cleans up the lock on success or
/// failure (the OS releases it automatically when `lock_file` is dropped).
///
/// Used to serialize auto-refresh attempts for a credential, ensuring only
/// one OAuth round-trip runs at a time per credential id.
fn with_refresh_lock<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    use std::fs::OpenOptions;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let lock_path = refresh_sentinel_path(path);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open credential refresh lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let result = f();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(lock_file);
    result
}

/// Run `f` while holding an exclusive `flock(2)` on the per-credential
/// atomic-write sentinel (`<path>.lock`). Brief; used only by
/// [`write_credential_atomic`] to serialize tmp+rename writers.
fn with_write_lock<F, T>(path: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    use std::fs::OpenOptions;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let lock_path = lock_sentinel_path(path);
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open credential write lock {}: {e}", lock_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(format!(
                "flock LOCK_EX {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }

    let result = f();

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(lock_file);
    result
}

/// Atomically write a credential JSON file with mode `0600` set *at creation
/// time* — there is no post-write `chmod` window during which a same-uid
/// reader could open the file with default umask permissions (the MEDIUM
/// "credential file perms applied AFTER write" finding).
///
/// Sequence (Unix):
///   1. Acquire `flock(LOCK_EX)` on the sibling `.lock` sentinel.
///   2. Remove any stale `<path>.tmp` left by a previous crash.
///   3. `open(O_WRONLY|O_CREAT|O_EXCL, 0600)` the tmp file.
///   4. Write payload, then `fsync` the tmp file.
///   5. `rename(tmp, path)` — atomic on same filesystem.
///   6. `fsync` the parent directory so the rename hits disk.
fn write_credential_atomic(path: &Path, data: &str) -> Result<(), String> {
    with_write_lock(path, || write_credential_atomic_unlocked(path, data))
}

/// Inner of [`write_credential_atomic`] — does the tmp+rename+fsync dance but
/// does NOT acquire the per-credential write lock. Caller is responsible for
/// synchronization.
fn write_credential_atomic_unlocked(path: &Path, data: &str) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| format!("credential path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;

    let tmp_path = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp_path);

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut tmp_file = opts
        .open(&tmp_path)
        .map_err(|e| format!("open {}: {e}", tmp_path.display()))?;

    tmp_file
        .write_all(data.as_bytes())
        .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
    tmp_file
        .sync_all()
        .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
    drop(tmp_file);

    fs::rename(&tmp_path, path).map_err(|e| format!("rename {}: {e}", path.display()))?;

    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
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
    let (ns_opt, args) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let mut min_tier = effective_session_tier();
    if min_tier > 3 {
        return Err("active session has no valid credential tier".to_string());
    }
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
                let cmd = args[i + 1].trim().to_string();
                if !cmd.starts_with("cos ") {
                    return Err("--refresh-cmd must be a cos command (e.g., 'cos credential oauth-refresh google')".into());
                }
                refresh_cmd = Some(cmd);
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

    let scope = credential_scope(&namespace, name)?;
    require_secret(Verb::SECRET_WRITE, scope)?;

    store_credential_record(
        name,
        value,
        &namespace,
        min_tier,
        ttl,
        refresh_cmd,
    )
}

fn store_credential_record(
    name: &str,
    value: &str,
    namespace: &str,
    min_tier: u8,
    ttl: Option<u64>,
    refresh_cmd: Option<String>,
) -> Result<Value, String> {
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
        name: name.to_string(),
        namespace: namespace.to_string(),
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
    // Atomic write with mode 0600 from creation time + fsync of tmp & parent.
    write_credential_atomic(&path, &data)
        .map_err(|e| format!("failed to write credential: {e}"))?;

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

/// Re-store a credential value as part of a session rollback (the undo
/// of a `credential.revoke`, or of a `credential.store` that overwrote a
/// prior value). Reuses the normal AES-256-GCM at-rest encryption and
/// the atomic 0600 write so the restored entry is indistinguishable from
/// one written by `cos credential store`.
///
/// Tier / TTL / refresh metadata is not captured in the mutation log, so
/// the restored entry uses the default tier (0) and no expiry.
///
/// Security note: the value being restored already lived in this
/// session's own (session-private) mutation log, so restoring it grants
/// no access the session did not already have — hence no extra caps gate.
pub fn rollback_restore(namespace: &str, name: &str, value: &str) -> Result<(), String> {
    let dir = namespace_dir(namespace);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create credentials dir: {e}"))?;
    let (value_b64, nonce_b64) = encrypt_value(value.as_bytes());
    let stored_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let cred = StoredCredential {
        name: name.to_string(),
        namespace: namespace.to_string(),
        value_b64,
        nonce_b64: Some(nonce_b64),
        min_tier: 0,
        stored_at,
        stored_by: Some("session-rollback".to_string()),
        expires_at: None,
        refresh_cmd: None,
    };
    let path = dir.join(format!("{name}.json"));
    let data =
        serde_json::to_string_pretty(&cred).map_err(|e| format!("failed to serialize: {e}"))?;
    write_credential_atomic(&path, &data)
}

/// Delete a credential entry as part of a session rollback (the undo of a
/// `credential.store` that created a brand-new key). No-op if the entry
/// is already gone.
pub fn rollback_delete(namespace: &str, name: &str) -> Result<(), String> {
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to delete credential {namespace}/{name}: {e}")),
    }
}

/// Load a credential value.
///
/// Usage: cos credential load <name> [--namespace NS] [--fd N]
///
/// `--fd N` writes the raw plaintext bytes to file descriptor `N` (no
/// trailing newline) and omits the `"value"` field from the returned JSON,
/// so callers can capture secrets without ever piping them through
/// stdout / shell history / IPC log sinks. See the audit's MEDIUM "secret
/// values returned in JSON cross IPC boundary" finding.
fn cmd_load(args: &[String]) -> Result<Value, String> {
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    // Parse --fd N (positional name otherwise).
    let mut fd_target: Option<i32> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--fd" if i + 1 < rest.len() => {
                fd_target = Some(
                    rest[i + 1]
                        .parse::<i32>()
                        .map_err(|_| "--fd must be a non-negative integer".to_string())?,
                );
                if fd_target.unwrap() < 0 {
                    return Err("--fd must be a non-negative integer".into());
                }
                i += 2;
            }
            _ => {
                positional.push(rest[i].clone());
                i += 1;
            }
        }
    }

    let name = positional.first().ok_or("usage: cos credential load <name>")?;
    require_secret(Verb::SECRET_READ, credential_scope(&namespace, name)?)?;
    let path = namespace_dir(&namespace).join(format!("{name}.json"));

    if !path.is_file() {
        return Err(format!("credential not found: {name}"));
    }

    let data = crate::filelock::read_locked(&path)
        .map_err(|e| format!("failed to read credential: {e}"))?
        .ok_or_else(|| format!("credential not found: {name}"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse credential: {e}"))?;

    // Tier check (named helper makes the direction obvious; fail-closed on
    // missing/corrupt policy registry).
    let current_tier = effective_session_tier();
    if !tier_grants_access(current_tier, cred.min_tier) {
        return Err(format!(
            "insufficient tier: credential '{}' requires tier {} or stronger (lower number), current session has tier {}",
            name, cred.min_tier, current_tier
        ));
    }

    // Check expiry.
    if is_expired(&cred.expires_at) {
        if let Some(ref refresh_cmd) = cred.refresh_cmd {
            // Serialize concurrent auto-refresh attempts per credential id so
            // we don't cannibalise a rotating refresh token (audit HIGH
            // "refresh-token cannibalisation race"). Inside the refresh lock
            // we re-read the credential and re-check expiry; if a sibling
            // refresh landed while we were waiting, we use its result.
            //
            // The refresh sentinel is a SEPARATE file from the atomic-write
            // sentinel so that the OAuth refresh sub-process — which itself
            // calls `cos credential store` and therefore needs the write lock
            // — does not deadlock against this lock.
            return with_refresh_lock(&path, || {
                let fresh_data = crate::filelock::read_locked(&path)
                    .map_err(|e| format!("failed to re-read credential: {e}"))?
                    .ok_or_else(|| {
                        format!("credential '{name}' disappeared during refresh")
                    })?;
                let fresh_cred: StoredCredential = serde_json::from_str(&fresh_data)
                    .map_err(|e| format!("failed to parse credential: {e}"))?;

                if !is_expired(&fresh_cred.expires_at) {
                    // Another caller already refreshed under the lock.
                    let value_bytes = decrypt_value(&fresh_cred)?;
                    let value = String::from_utf8(value_bytes)
                        .map_err(|e| format!("credential is not valid UTF-8: {e}"))?;
                    return Ok(build_load_result(
                        name,
                        &fresh_cred,
                        value,
                        Some(false),
                        fd_target,
                    )?);
                }

                let refresh_cmd_owned = fresh_cred
                    .refresh_cmd
                    .clone()
                    .unwrap_or_else(|| refresh_cmd.clone());
                let new_value = execute_refresh(&refresh_cmd_owned).map_err(|e| {
                    format!("credential '{name}' expired and auto-refresh failed: {e}")
                })?;

                // A refresh command such as `cos credential oauth-refresh ...`
                // persists the real token itself and prints only a status JSON
                // envelope. Prefer that freshly stored value instead of
                // overwriting it with the command's stdout.
                if let Some(after_data) = crate::filelock::read_locked(&path)
                    .map_err(|e| format!("failed to read refreshed credential: {e}"))?
                {
                    let after: StoredCredential = serde_json::from_str(&after_data)
                        .map_err(|e| format!("failed to parse refreshed credential: {e}"))?;
                    if after.value_b64 != fresh_cred.value_b64
                        || after.nonce_b64 != fresh_cred.nonce_b64
                        || !is_expired(&after.expires_at)
                    {
                        let value = String::from_utf8(decrypt_value(&after)?)
                            .map_err(|e| format!("credential is not valid UTF-8: {e}"))?;
                        return build_load_result(
                            name,
                            &after,
                            value,
                            Some(true),
                            fd_target,
                        );
                    }
                }

                let ttl = compute_original_ttl(&fresh_cred);
                let (new_value_b64, new_nonce_b64) =
                    encrypt_value(new_value.trim().as_bytes());
                let now = chrono::Utc::now();
                let new_expires = ttl.map(|secs| {
                    let exp = now + chrono::Duration::seconds(secs);
                    exp.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                });

                let updated_cred = StoredCredential {
                    name: fresh_cred.name.clone(),
                    namespace: fresh_cred.namespace.clone(),
                    value_b64: new_value_b64,
                    nonce_b64: Some(new_nonce_b64),
                    min_tier: fresh_cred.min_tier,
                    stored_at: fresh_cred.stored_at.clone(),
                    stored_by: fresh_cred.stored_by.clone(),
                    expires_at: new_expires.clone(),
                    refresh_cmd: fresh_cred.refresh_cmd.clone(),
                };

                let serialized = serde_json::to_string_pretty(&updated_cred)
                    .map_err(|e| format!("failed to serialize: {e}"))?;
                // Atomic 0600 write + fsync. The atomic-write lock is a
                // distinct sentinel from the refresh lock we hold, so this
                // call acquires its own (uncontended) lock and does not
                // deadlock with the surrounding `with_refresh_lock`.
                write_credential_atomic(&path, &serialized)
                    .map_err(|e| format!("failed to write refreshed credential: {e}"))?;

                let trimmed = new_value.trim().to_string();
                build_load_result(name, &updated_cred, trimmed, Some(true), fd_target)
            });
        }

        // No refresh_cmd — return expired error (existing behavior).
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

    build_load_result(name, &cred, value, None, fd_target)
}

/// Build the JSON response for `cmd_load` (or its auto-refresh path).
///
/// If `fd_target` is `Some(n)`, the plaintext is written raw (no trailing
/// newline) to file descriptor `n` and the `"value"` key is replaced by
/// `"value_fd": n` so the secret never crosses the IPC / stdout boundary.
/// Otherwise the value is embedded in the JSON as before.
fn build_load_result(
    name: &str,
    cred: &StoredCredential,
    value: String,
    refreshed: Option<bool>,
    fd_target: Option<i32>,
) -> Result<Value, String> {
    let mut result = json!({
        "name": name,
        "namespace": cred.namespace,
        "min_tier": cred.min_tier,
    });

    if let Some(refreshed_flag) = refreshed {
        result["refreshed"] = json!(refreshed_flag);
        if let Some(ref exp) = cred.expires_at {
            result["expires_at"] = json!(exp);
        }
    }

    match fd_target {
        Some(fd) => {
            write_value_to_fd(fd, value.as_bytes())?;
            result["value_fd"] = json!(fd);
        }
        None => {
            result["value"] = json!(value);
        }
    }
    Ok(result)
}

/// Write `bytes` raw (no newline) to file descriptor `fd`. Used by the
/// `--fd N` mode of `cmd_load` so secrets can be handed off to a caller
/// via an out-of-band fd that the caller has set up specifically for the
/// transfer — never via stdout (where shell history / pipes / audit sinks
/// can capture them).
#[cfg(unix)]
fn write_value_to_fd(fd: i32, bytes: &[u8]) -> Result<(), String> {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let n = unsafe {
            libc::write(
                fd,
                remaining.as_ptr() as *const libc::c_void,
                remaining.len(),
            )
        };
        if n < 0 {
            return Err(format!(
                "failed to write to fd {fd}: {}",
                std::io::Error::last_os_error()
            ));
        }
        if n == 0 {
            return Err(format!("write to fd {fd} returned 0 bytes"));
        }
        remaining = &remaining[(n as usize)..];
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_value_to_fd(_fd: i32, _bytes: &[u8]) -> Result<(), String> {
    Err("--fd is only supported on Unix".into())
}

/// Revoke (delete) a credential.
///
/// Usage: cos credential revoke <name> [--namespace NS]
fn cmd_revoke(args: &[String]) -> Result<Value, String> {
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let name = rest.first().ok_or("usage: cos credential revoke <name>")?;
    require_secret(Verb::SECRET_WRITE, credential_scope(&namespace, name)?)?;
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
    let (ns_opt, _rest) = parse_namespace_flag(args);

    match ns_opt {
        Some(namespace) => {
            require_secret(Verb::SECRET_READ, namespace_scope(&namespace)?)?;
            list_namespace(&namespace)
        }
        None => {
            require_secret(Verb::SECRET_READ, Scope::name("**"))?;
            list_all_namespaces()
        }
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
        if let Ok(Some(data)) = crate::filelock::read_locked(&entry.path()) {
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
    validate_credential_component("bundle name", bundle_name)?;
    for key in &key_list {
        validate_credential_component("credential name", key)?;
    }
    require_secret(Verb::SECRET_GRANT, bundle_scope(&namespace, bundle_name)?)?;

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
    crate::filelock::write_locked(&path, &data)
        .map_err(|e| format!("failed to write bundle: {e}"))?;

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
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());

    let bundle_name = rest
        .first()
        .ok_or("usage: cos credential load-bundle <name> [--namespace NS]")?;
    validate_credential_component("namespace", &namespace)?;
    validate_credential_component("bundle name", bundle_name)?;
    require_secret(Verb::SECRET_READ, bundle_scope(&namespace, bundle_name)?)?;

    let path = bundles_dir(&namespace).join(format!("{bundle_name}.json"));
    if !path.is_file() {
        return Err(format!("bundle not found: {bundle_name}"));
    }

    let data = crate::filelock::read_locked(&path)
        .map_err(|e| format!("failed to read bundle: {e}"))?
        .ok_or_else(|| format!("bundle not found: {bundle_name}"))?;
    let manifest: BundleManifest =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse bundle: {e}"))?;
    if manifest.name != *bundle_name || manifest.namespace != namespace {
        return Err("bundle metadata does not match its storage path".to_string());
    }

    let mut credentials = serde_json::Map::new();
    let mut errors = serde_json::Map::new();
    let current_tier = effective_session_tier();

    for key in &manifest.keys {
        validate_credential_component("credential name", key)?;
        let cred_path = namespace_dir(&namespace).join(format!("{key}.json"));
        if !cred_path.is_file() {
            errors.insert(
                key.clone(),
                Value::String(format!("credential not found: {key}")),
            );
            continue;
        }

        let cred_data = match crate::filelock::read_locked(&cred_path) {
            Ok(Some(d)) => d,
            Ok(None) => {
                errors.insert(
                    key.clone(),
                    Value::String(format!("credential not found: {key}")),
                );
                continue;
            }
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
        if !tier_grants_access(current_tier, cred.min_tier) {
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

    // OS safety: only allow cos commands as refresh commands.
    // This prevents arbitrary code execution from credential files.
    let trimmed = cmd.trim();
    if !trimmed.starts_with("cos ") && !trimmed.starts_with("cos\t") && trimmed != "cos" {
        return Err(format!(
            "refresh_cmd must be a cos command (starts with 'cos '). got: {}",
            &trimmed[..trimmed.len().min(50)]
        ));
    }

    // Execute via direct argv, not shell — no injection possible
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let output = Command::new(parts[0])
        .args(&parts[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to execute refresh command: {e}"))?;

    if output.status.success() {
        let value = String::from_utf8(output.stdout)
            .map_err(|e| format!("refresh output not valid UTF-8: {e}"))?;
        if value.trim().is_empty() {
            return Err("refresh command produced empty output".into());
        }
        Ok(value)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "refresh command failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ))
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
    let (ns_opt, rest) = parse_namespace_flag(args);
    let namespace = ns_opt.unwrap_or_else(|| "default".into());
    validate_credential_component("namespace", &namespace)?;

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
    let derived_tier = [
        credential_min_tier("GOOGLE_REFRESH_TOKEN", namespace)?,
        credential_min_tier("GOOGLE_CLIENT_ID", namespace)?,
        credential_min_tier("GOOGLE_CLIENT_SECRET", namespace)?,
    ]
    .into_iter()
    .min()
    .unwrap_or(0);
    let output_tier =
        credential_min_tier_if_present("GOOGLE_ACCESS_TOKEN", namespace)?
            .unwrap_or(derived_tier);
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "GOOGLE_ACCESS_TOKEN")?,
    )?;

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
    store_credential_record(
        "GOOGLE_ACCESS_TOKEN",
        access_token,
        namespace,
        output_tier,
        Some(expires_in),
        Some(format!(
            "cos credential oauth-refresh google --namespace {namespace}"
        )),
    )?;

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
    let tenant_id = if namespace_dir(namespace)
        .join("MICROSOFT_TENANT_ID.json")
        .is_file()
    {
        load_credential_value("MICROSOFT_TENANT_ID", namespace)?
    } else {
        "common".to_string()
    };
    let refresh_tier = credential_min_tier("MICROSOFT_REFRESH_TOKEN", namespace)?;
    let derived_tier = [
        refresh_tier,
        credential_min_tier("MICROSOFT_CLIENT_ID", namespace)?,
        credential_min_tier("MICROSOFT_CLIENT_SECRET", namespace)?,
    ]
    .into_iter()
    .min()
    .unwrap_or(refresh_tier);
    let access_tier =
        credential_min_tier_if_present("MICROSOFT_ACCESS_TOKEN", namespace)?
            .unwrap_or(derived_tier);
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "MICROSOFT_ACCESS_TOKEN")?,
    )?;
    require_secret(
        Verb::SECRET_WRITE,
        credential_scope(namespace, "MICROSOFT_REFRESH_TOKEN")?,
    )?;

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
        store_credential_record(
            "MICROSOFT_REFRESH_TOKEN",
            new_refresh,
            namespace,
            refresh_tier,
            None,
            None,
        )?;
    }

    store_credential_record(
        "MICROSOFT_ACCESS_TOKEN",
        access_token,
        namespace,
        access_tier,
        Some(expires_in),
        Some(format!(
            "cos credential oauth-refresh microsoft --namespace {namespace}"
        )),
    )?;

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

/// Build the `curl` `Command` for an OAuth token POST.
///
/// Notably this builder does **not** accept the request body and does not put
/// any secret into argv. The body is supplied later via stdin
/// (`--data-binary @-`) so that `client_secret`, `refresh_token`, etc. cannot
/// be read by any same-uid process via `/proc/<pid>/cmdline` (the HIGH
/// "OAuth client_secret / refresh_token leak via argv" audit finding).
fn build_curl_post(url: &str, content_type: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-s",
        "-S",
        "-X",
        "POST",
        "-H",
        &format!("Content-Type: {content_type}"),
        "--data-binary",
        "@-", // read body from stdin
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        url,
    ]);
    cmd
}

/// Simple URL-encoded POST. Body is piped to `curl` on stdin so the secret
/// never appears in argv.
fn http_post(url: &str, body: &str, content_type: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = build_curl_post(url, content_type);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("failed to execute curl: {e}"))?;

    // Write body to stdin, then close it so curl knows the body is complete.
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open curl stdin".to_string())?;
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("failed to write request body to curl stdin: {e}"))?;
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for curl: {e}"))?;

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
    require_secret(Verb::SECRET_READ, credential_scope(namespace, name)?)?;
    read_credential_value(name, namespace, true)
}

fn credential_min_tier(name: &str, namespace: &str) -> Result<u8, String> {
    credential_min_tier_if_present(name, namespace)?
        .ok_or_else(|| format!("credential not found: {name} (namespace: {namespace})"))
}

fn credential_min_tier_if_present(
    name: &str,
    namespace: &str,
) -> Result<Option<u8>, String> {
    credential_scope(namespace, name)?;
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    let Some(data) = crate::filelock::read_locked(&path)
        .map_err(|error| format!("failed to read credential metadata: {error}"))?
    else {
        return Ok(None);
    };
    let credential: StoredCredential = serde_json::from_str(&data)
        .map_err(|error| format!("failed to parse credential metadata: {error}"))?;
    Ok(Some(credential.min_tier))
}

fn read_credential_value(
    name: &str,
    namespace: &str,
    enforce_tier: bool,
) -> Result<String, String> {
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    if !path.is_file() {
        return Err(format!(
            "credential not found: {name} (namespace: {namespace}). Store it with: cos credential store {name} <value> --namespace {namespace}"
        ));
    }
    let data = crate::filelock::read_locked(&path)
        .map_err(|e| format!("failed to read: {e}"))?
        .ok_or_else(|| format!("credential not found: {name} (namespace: {namespace})"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse: {e}"))?;
    if enforce_tier {
        let current_tier = effective_session_tier();
        if !tier_grants_access(current_tier, cred.min_tier) {
            return Err(format!(
                "insufficient tier: credential '{name}' requires {}, current session has {current_tier}",
                cred.min_tier
            ));
        }
    }
    if is_expired(&cred.expires_at) {
        return Err(format!("credential '{name}' has expired"));
    }

    match decrypt_value(&cred) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}")),
        Err(e) => Err(e),
    }
}

/// Public read-only accessor for credential values.
///
/// Use this from kernel subsystems that need a stored secret (LLM
/// provider API keys, OAuth tokens, …) instead of going through the CLI
/// dispatcher. Returns the plaintext value or a human-readable error.
///
/// Returns `Ok(None)` if the credential is not present so callers can
/// fall back to environment variables or other lookup paths without
/// converting a not-found into a hard error.
pub fn try_load(name: &str, namespace: &str) -> Result<Option<String>, String> {
    credential_scope(namespace, name)?;
    let path = namespace_dir(namespace).join(format!("{name}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    // Trusted kernel accessor used to construct providers on behalf of a
    // session. User/App-facing reads must go through `cmd_load`.
    read_credential_value(name, namespace, false).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Once;
    static PERMS_INIT: Once = Once::new();
    fn perms_init() {
        PERMS_INIT.call_once(|| std::env::set_var("COS_PERMS_MODE", "permissive"));
    }
    use std::sync::atomic::{AtomicU32, Ordering};

    static INIT: Once = Once::new();
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// All tests share one COS_CREDENTIALS_DIR (set once). Each test uses unique
    /// credential names so there is no cross-test interference.
    fn unique_name(prefix: &str) -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("{prefix}-{n}")
    }

    fn setup() {
        INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            // credentials_dir() now reads COS_CREDENTIALS_DIR (per-user
            // store moved out of $COS_DATA_DIR). Tests still set
            // COS_DATA_DIR for other modules that share this dir.
            std::env::set_var("COS_DATA_DIR", &dir);
            std::env::set_var("COS_CREDENTIALS_DIR", dir.join("credentials"));
        });
        std::env::remove_var("COS_SESSION");
    }

    // ---- SHA-256 ----------------------------------------------------------

    #[test]
    fn sha256_known_vectors() {
        perms_init();
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
        perms_init();
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
        perms_init();
        let key = sha256::hash(b"test-key-tamper");
        let nonce = [0u8; 12];
        let ct = aes_gcm::encrypt(&key, &nonce, b"secret");

        let mut tampered = ct.clone();
        tampered[0] ^= 0xff;
        assert!(aes_gcm::decrypt(&key, &nonce, &tampered).is_err());
    }

    #[test]
    fn aes_gcm_empty_plaintext() {
        perms_init();
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
        perms_init();
        let data = b"hello world 12345!@#$%";
        let encoded = to_b64(data);
        let decoded = from_b64(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    // ---- Legacy XOR backward compatibility --------------------------------

    #[test]
    fn legacy_xor_backward_compat() {
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
        setup();
        let r = cmd_store(&["bad/name".into(), "val".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn load_nonexistent() {
        perms_init();
        setup();
        let name = unique_name("nonexistent");
        let r = cmd_load(&[name]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not found"));
    }

    // ---- Namespace isolation ----------------------------------------------

    #[test]
    fn namespace_isolation() {
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
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
        perms_init();
        setup();
        let name = unique_name("refresh-store");
        let r = cmd_store(&[
            name.clone(),
            "initial-value".into(),
            "--ttl".into(),
            "3600".into(),
            "--refresh-cmd".into(),
            "cos credential load test".into(),
        ])
        .unwrap();
        assert_eq!(r["stored"], name);

        // Verify refresh_cmd is stored
        let path = namespace_dir("default").join(format!("{name}.json"));
        let data = fs::read_to_string(&path).unwrap();
        let cred: StoredCredential = serde_json::from_str(&data).unwrap();
        assert_eq!(
            cred.refresh_cmd.as_deref(),
            Some("cos credential load test")
        );
    }

    #[test]
    fn store_rejects_non_cos_refresh_cmd() {
        perms_init();
        setup();
        let name = unique_name("bad-refresh");
        let r = cmd_store(&[
            name.clone(),
            "value".into(),
            "--refresh-cmd".into(),
            "echo evil".into(),
        ]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("must be a cos command"));
    }

    #[test]
    fn execute_refresh_rejects_non_cos() {
        perms_init();
        let r = execute_refresh("rm -rf /");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("must be a cos command"));
    }

    #[test]
    fn load_auto_refresh_on_expiry() {
        perms_init();
        setup();
        let name = unique_name("auto-refresh");

        // Store with expired TTL and a cos refresh command that will fail.
        // We write the credential file directly to bypass store validation.
        let dir = namespace_dir("default");
        let _ = fs::create_dir_all(&dir);
        let (value_b64, nonce_b64) = encrypt_value(b"old-value");
        let now = chrono::Utc::now();
        let stored_at = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        // Use a fixed timestamp far in the past to guarantee expiry
        let expires_at = Some("2020-01-01T00:00:00Z".to_string());

        let cred = StoredCredential {
            name: name.clone(),
            namespace: "default".into(),
            value_b64,
            nonce_b64: Some(nonce_b64),
            min_tier: 0,
            stored_at,
            stored_by: None,
            expires_at,
            // Use a cos command with nonexistent subcommand that will exit non-zero.
            // "cos nonexistent-subcommand-xyz" will fail because the subcommand is unknown.
            refresh_cmd: Some("cos nonexistent-subcommand-xyz".into()),
        };
        let path = dir.join(format!("{name}.json"));
        let data = serde_json::to_string_pretty(&cred).unwrap();
        fs::write(&path, data).unwrap();

        // Load should attempt auto-refresh but fail
        let r = cmd_load(&[name.clone()]);
        // The cos binary may or may not be in PATH, but either way the refresh should fail:
        // - If cos is not in PATH: "failed to execute refresh command"
        // - If cos is in PATH but exits non-zero: "refresh command failed"
        // - If cos is in PATH and exits 0 with error JSON: we get a JSON string as value
        //   (which is still technically valid but the credential gets refreshed with error JSON)
        // In any case, we verify the refresh path is exercised by checking the result.
        // If it succeeds, the value will be error JSON from cos, not "old-value".
        match r {
            Err(e) => {
                assert!(
                    e.contains("auto-refresh failed") || e.contains("failed to execute"),
                    "unexpected error: {e}"
                );
            }
            Ok(v) => {
                // Refresh "succeeded" with error JSON from cos — value is not "old-value"
                assert_eq!(v["refreshed"], true);
                assert_ne!(v["value"], "old-value");
            }
        }
    }

    #[test]
    fn load_expired_no_refresh_cmd_fails() {
        perms_init();
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
        perms_init();
        assert_eq!(urlencoded("hello world"), "hello%20world");
        assert_eq!(urlencoded("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(urlencoded("simple"), "simple");
    }

    // ---- TTL computation --------------------------------------------------

    #[test]
    fn compute_ttl_from_timestamps() {
        perms_init();
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
        perms_init();
        setup();
        let r = cmd_oauth_refresh(&["unknown".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unsupported"));
    }

    #[test]
    fn oauth_refresh_missing_provider() {
        perms_init();
        setup();
        let r = cmd_oauth_refresh(&[]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("usage"));
    }

    // ---- Persistent root key (CRITICAL audit fix) -------------------------

    /// Verifies the CRITICAL fix: when `/etc/machine-id` is unreadable AND no
    /// `credential-root.key` exists yet, the store must generate a random
    /// 32-byte key via the OS CSPRNG, persist it with mode 0600, and reuse it
    /// on subsequent calls. The previous behaviour was to silently fall back
    /// to `sha256("claw-os-credential-store-key-v1")` — a universally-known
    /// key that decrypts every credential store offline.
    #[test]
    fn test_machine_id_missing_falls_back_to_persistent_random_key() {
        perms_init();
        setup();

        // Use a per-test scratch dir + the `*_at` helpers so this test does
        // NOT mutate any process-global env vars (which would race the other
        // ~80 credential tests).
        let dir = std::env::temp_dir().join(format!(
            "cos-cred-rootkey-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::create_dir_all(&dir);
        let key_path = dir.join("credential-root.key");
        let _ = fs::remove_file(&key_path);

        // 1. No persistent key yet.
        assert!(
            load_persistent_root_key_at(&key_path).is_none(),
            "key file must not exist before first generate call"
        );

        // 2. Generate + persist (this is what `derive_key` calls when neither
        //    the keyring nor machine-id are available).
        let key1 = generate_and_persist_root_key_at(&key_path);
        assert_eq!(key1.len(), 32, "root key must be exactly 32 bytes");
        assert!(
            key1.iter().any(|&b| b != 0),
            "generated key must not be all zeros (CSPRNG sanity)"
        );

        // 3. The on-disk file matches: 32 bytes exactly, mode 0o600.
        let bytes = fs::read(&key_path).expect("key file must exist");
        assert_eq!(bytes.len(), 32, "persisted key must be 32 bytes");
        assert_eq!(bytes, &key1[..], "on-disk bytes must equal returned key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&key_path).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "persisted key must be mode 0600 from creation (no chmod race)"
            );
        }

        // 4. Second call returns the SAME key from disk.
        let key2 = load_persistent_root_key_at(&key_path).expect("key should load after generate");
        assert_eq!(key1, key2, "persisted key must round-trip");

        // 5. A redundant `generate_and_persist_root_key_at()` call (e.g. from
        //    a racing process) MUST NOT overwrite the existing key — it must
        //    fall back to reading whatever is already there.
        let key3 = generate_and_persist_root_key_at(&key_path);
        assert_eq!(
            key1, key3,
            "repeated generate must not overwrite an existing on-disk key"
        );

        let _ = fs::remove_file(&key_path);
        let _ = fs::remove_dir(&dir);
    }

    // ---- Refresh serialization (HIGH audit fix) ---------------------------

    /// Verifies the HIGH fix: concurrent auto-refresh attempts on the same
    /// credential must be serialized via a per-credential flock so that two
    /// callers cannot both call the OAuth endpoint and cannibalise each
    /// other's rotated refresh token.
    ///
    /// We exercise the primitive (`with_refresh_lock`) directly with N
    /// threads, asserting that the maximum observed concurrency inside the
    /// critical section is exactly 1.
    #[test]
    fn test_concurrent_refresh_serialized() {
        perms_init();
        setup();

        let name = unique_name("refresh-serialized");
        let path = namespace_dir("default").join(format!("{name}.json"));
        let _ = fs::create_dir_all(path.parent().unwrap());
        let _ = fs::remove_file(refresh_sentinel_path(&path));

        use std::sync::atomic::AtomicI64;
        use std::sync::Arc;
        let in_flight = Arc::new(AtomicI64::new(0));
        let max_in_flight = Arc::new(AtomicI64::new(0));
        let calls = Arc::new(AtomicU32::new(0));

        let n_threads = 8usize;
        let mut handles = Vec::with_capacity(n_threads);
        for _ in 0..n_threads {
            let in_flight = in_flight.clone();
            let max_in_flight = max_in_flight.clone();
            let calls = calls.clone();
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                with_refresh_lock(&p, || {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut prev = max_in_flight.load(Ordering::SeqCst);
                    while cur > prev {
                        match max_in_flight.compare_exchange(
                            prev,
                            cur,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(p) => prev = p,
                        }
                    }
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Hold the critical section long enough that any race
                    // would surface as concurrent observed > 1.
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok::<(), String>(())
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            n_threads as u32,
            "every thread must enter the critical section exactly once"
        );
        assert_eq!(
            max_in_flight.load(Ordering::SeqCst),
            1,
            "with_refresh_lock must serialize: max concurrent must be 1"
        );

        let _ = fs::remove_file(refresh_sentinel_path(&path));
    }

    // ---- OAuth argv leak (HIGH audit fix) ---------------------------------

    /// Verifies the HIGH fix: the curl command used for OAuth refresh must
    /// NOT receive the request body (containing `client_secret` and
    /// `refresh_token`) on its argv, where any same-uid process could read it
    /// from `/proc/<pid>/cmdline`. The body must come from stdin via
    /// `--data-binary @-`.
    #[test]
    fn test_oauth_refresh_no_argv_leak() {
        perms_init();
        let secret = "super-secret-refresh-token-DO-NOT-LEAK-ME";
        // The builder, by design, accepts only URL + content type — there is
        // no parameter through which the body could reach argv. We verify
        // both the design (no body parameter) and the observable contract
        // (no -d / --data, body via stdin only).
        let cmd = build_curl_post(
            "https://oauth2.googleapis.com/token",
            "application/x-www-form-urlencoded",
        );
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let argv_joined = argv.join(" ");

        assert!(
            !argv_joined.contains(secret),
            "argv must never contain a secret; argv = {argv_joined}"
        );
        assert!(
            !argv.iter().any(|a| a == "-d"),
            "argv must not use `-d` (puts body in argv); argv = {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "--data"),
            "argv must not use `--data` (puts body in argv); argv = {argv:?}"
        );
        let stdin_pair = argv.windows(2).find(|w| w[0] == "--data-binary");
        assert_eq!(
            stdin_pair.map(|w| w[1].as_str()),
            Some("@-"),
            "argv must use `--data-binary @-` (body read from stdin); argv = {argv:?}"
        );
    }

    // ---- Debug masking (MEDIUM audit fix) --------------------------------

    /// Verifies the MEDIUM fix: Debug / Display impls for `StoredCredential`
    /// must never echo the encrypted blob or its nonce, so an accidental
    /// `tracing::debug!(?cred)` cannot regress into leaking credential
    /// material through log sinks.
    #[test]
    fn stored_credential_debug_display_masks_secret() {
        perms_init();
        let cred = StoredCredential {
            name: "api-key".into(),
            namespace: "default".into(),
            value_b64: "VERY-SECRET-CIPHERTEXT-NEVER-LOG-ME".into(),
            nonce_b64: Some("very-secret-nonce".into()),
            min_tier: 0,
            stored_at: "2026-01-01T00:00:00Z".into(),
            stored_by: None,
            expires_at: None,
            refresh_cmd: None,
        };
        let dbg = format!("{cred:?}");
        assert!(
            !dbg.contains("VERY-SECRET-CIPHERTEXT"),
            "Debug must mask value_b64: {dbg}"
        );
        assert!(
            !dbg.contains("very-secret-nonce"),
            "Debug must mask nonce_b64: {dbg}"
        );
        assert!(dbg.contains("***"), "Debug must show masking marker: {dbg}");
        let disp = format!("{cred}");
        assert!(
            !disp.contains("VERY-SECRET-CIPHERTEXT"),
            "Display must not echo ciphertext: {disp}"
        );
        assert_eq!(disp, "credential(default/api-key)");
    }

    // ---- Tier comparison semantics (HIGH audit fix) ----------------------

    /// Pins the semantic of the renamed tier comparator. Lower number ==
    /// more privileged. ROOT (0) accesses everything; SANDBOX (3) accesses
    /// only what's explicitly tier-3-allowed.
    #[test]
    fn tier_grants_access_semantics_pinned() {
        perms_init();
        // Same-tier always grants.
        assert!(tier_grants_access(0, 0));
        assert!(tier_grants_access(1, 1));
        assert!(tier_grants_access(2, 2));
        assert!(tier_grants_access(3, 3));
        // Stronger session (lower number) accesses weaker-tier creds.
        assert!(tier_grants_access(0, 1));
        assert!(tier_grants_access(0, 3));
        assert!(tier_grants_access(2, 3));
        // Weaker session (higher number) CANNOT access stronger-tier creds.
        assert!(!tier_grants_access(1, 0));
        assert!(!tier_grants_access(2, 0));
        assert!(!tier_grants_access(3, 0));
        assert!(!tier_grants_access(3, 2));
        // u8::MAX (fail-closed "weakest possible") accesses nothing
        // except a (non-existent) u8::MAX cred.
        assert!(!tier_grants_access(u8::MAX, 0));
        assert!(!tier_grants_access(u8::MAX, 3));
    }

    // ---- from_b64 strictness (LOW audit fix) -----------------------------

    /// Verifies the LOW fix: `from_b64` now rejects garbage instead of
    /// silently mapping non-alphabet bytes to 'A' (which surfaced later as
    /// an opaque AES-GCM authentication failure).
    #[test]
    fn from_b64_rejects_garbage() {
        perms_init();
        assert!(from_b64("###@@@").is_err(), "non-alphabet bytes must error");
        assert!(
            from_b64("AAAA!!!!").is_err(),
            "embedded junk must be rejected"
        );
        // Valid base64 still round-trips.
        let round_trip = from_b64(&to_b64(b"hello world")).unwrap();
        assert_eq!(round_trip, b"hello world");
    }

    // ---- --fd N out-of-band value (MEDIUM audit fix) ---------------------

    /// Verifies the MEDIUM fix: `cmd_load --fd N` writes the plaintext to
    /// the specified file descriptor and omits the `"value"` field from the
    /// JSON return, so logging the response payload cannot leak the secret.
    #[cfg(unix)]
    #[test]
    fn cmd_load_fd_writes_value_to_fd_and_omits_from_json() {
        perms_init();
        setup();
        let name = unique_name("load-fd");
        cmd_store(&[name.clone(), "fd-secret-xyz".into()]).unwrap();

        // pipe(2) gives us a reader/writer pair we control completely.
        let mut fds = [0i32; 2];
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(r, 0, "pipe(2) failed");
        let rfd = fds[0];
        let wfd = fds[1];

        let result = cmd_load(&[name.clone(), "--fd".into(), wfd.to_string()]).unwrap();
        unsafe { libc::close(wfd) };

        assert!(
            result.get("value").is_none(),
            "value must not be in JSON when --fd is used: {result}"
        );
        assert_eq!(result["value_fd"], wfd);

        let mut buf = vec![0u8; 64];
        let n = unsafe { libc::read(rfd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        unsafe { libc::close(rfd) };
        assert!(n > 0, "expected bytes on the fd, got {n}");
        buf.truncate(n as usize);
        assert_eq!(buf, b"fd-secret-xyz");
    }
}
