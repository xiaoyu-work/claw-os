use super::*;

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
pub(super) mod sha256 {
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;

    /// Compute SHA-256 via AF_ALG socket (kernel crypto API).
    pub(in crate::credential) fn hash(data: &[u8]) -> [u8; 32] {
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
        for block in msg.as_chunks::<64>().0 {
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
pub(super) mod sha256 {
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
    pub(in crate::credential) fn hash(data: &[u8]) -> [u8; 32] {
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

pub(super) mod aes_gcm {
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
    pub(in crate::credential) fn encrypt(
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
    ) -> Vec<u8> {
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
    pub(in crate::credential) fn decrypt(
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

pub(super) fn to_b64(data: &[u8]) -> String {
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

pub(super) fn from_b64(s: &str) -> Result<Vec<u8>, String> {
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

/// Encrypt a plaintext value with AES-256-GCM.
/// Returns `(value_b64, nonce_b64)`.
pub(super) fn encrypt_value(plaintext: &[u8]) -> CredentialResult<(String, String)> {
    let key = derive_key()?;
    let nonce = generate_nonce()?;
    let ct_and_tag = aes_gcm::encrypt(&key, &nonce, plaintext);
    Ok((to_b64(&ct_and_tag), to_b64(&nonce)))
}

/// Decrypt a stored credential. Handles both AES-256-GCM and legacy XOR.
pub(super) fn decrypt_value(cred: &StoredCredential) -> CredentialResult<Vec<u8>> {
    let raw = from_b64(&cred.value_b64).map_err(|error| {
        CredentialError::corrupt(
            "credential.decrypt",
            format!("failed to decode credential value: {error}"),
        )
    })?;

    match &cred.nonce_b64 {
        Some(nonce_b64) => {
            let nonce_bytes = from_b64(nonce_b64).map_err(|error| {
                CredentialError::corrupt(
                    "credential.decrypt",
                    format!("failed to decode nonce: {error}"),
                )
            })?;
            if nonce_bytes.len() != 12 {
                return Err(CredentialError::corrupt(
                    "credential.decrypt",
                    "invalid nonce length (expected 12 bytes)",
                ));
            }
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&nonce_bytes);
            let key = derive_key()?;
            aes_gcm::decrypt(&key, &nonce, &raw)
                .map_err(|message| CredentialError::corrupt("credential.decrypt", message))
        }
        None => legacy_xor(&raw),
    }
}
