// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Minimal SHA-1 + HMAC-SHA1, hand-rolled because the workspace crypto
//! catalog is SHA-2-only. SHA-1 is broken for collision resistance but
//! remains the algorithm Twilio mandates for its `X-Twilio-Signature`
//! header; this implementation exists solely to verify that scheme. It
//! is byte-identical to the copy `firefly-testkit` uses to *produce*
//! Twilio fixtures.

const BLOCK_SIZE: usize = 64;
const DIGEST_SIZE: usize = 20;

/// Computes the SHA-1 digest of `data` (FIPS 180-4).
pub(crate) fn sha1(data: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    // Pad: 0x80, zeros to 56 mod 64, then the 64-bit big-endian bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % BLOCK_SIZE != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(BLOCK_SIZE) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; DIGEST_SIZE];
    for (slot, word) in out.chunks_exact_mut(4).zip(h.iter()) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Computes HMAC-SHA1 (RFC 2104) of `message` with `key`.
pub(crate) fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut block_key = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        block_key[..DIGEST_SIZE].copy_from_slice(&sha1(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(BLOCK_SIZE + message.len());
    inner.extend(block_key.iter().map(|b| b ^ 0x36));
    inner.extend_from_slice(message);
    let inner_digest = sha1(&inner);

    let mut outer = Vec::with_capacity(BLOCK_SIZE + DIGEST_SIZE);
    outer.extend(block_key.iter().map(|b| b ^ 0x5c));
    outer.extend_from_slice(&inner_digest);
    sha1(&outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    // FIPS 180-4 / RFC 3174 known-answer vectors.
    #[test]
    fn sha1_known_vectors() {
        assert_eq!(
            hex::encode(sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex::encode(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex::encode(sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    // RFC 2202 test cases 1-2 plus the long-key case 6.
    #[test]
    fn hmac_sha1_rfc2202_vectors() {
        assert_eq!(
            hex::encode(hmac_sha1(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        assert_eq!(
            hex::encode(hmac_sha1(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            hex::encode(hmac_sha1(
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }
}
