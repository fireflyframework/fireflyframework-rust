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

//! HMAC signers matching the wire shape of the `webhooks` validators.

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeMap;

type HmacSha256 = Hmac<Sha256>;

/// Returns the canonical `sha256=<hex>` signature.
pub fn sign_hmac(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Builds a `t=<unix>,v1=<hex>` `Stripe-Signature` header value.
///
/// `unix_ts` is the signing timestamp expressed in Unix seconds (the Go
/// port takes a `time.Time` and calls `.Unix()`; here the seconds value is
/// explicit). The signed payload is `<unix>.<body>`, exactly as Stripe
/// specifies.
pub fn sign_stripe(secret: &[u8], body: &[u8], unix_ts: i64) -> String {
    let mut signed = format!("{unix_ts}.").into_bytes();
    signed.extend_from_slice(body);
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&signed);
    format!(
        "t={unix_ts},v1={}",
        hex::encode(mac.finalize().into_bytes())
    )
}

/// Builds a `sha256=<hex>` `X-Hub-Signature-256` header value using the
/// same scheme as [`sign_hmac`].
pub fn sign_github(secret: &[u8], body: &[u8]) -> String {
    sign_hmac(secret, body)
}

/// Builds the `X-Twilio-Signature` header value: HMAC-SHA1 of
/// `URL + sorted(form key+value)` with the auth token, base64-encoded.
///
/// `form` is a slice of `(key, value)` pairs; keys are signed in sorted
/// order and only the first value of a repeated key participates,
/// mirroring Go's `url.Values.Get`.
pub fn sign_twilio(auth_token: &[u8], post_url: &str, form: &[(&str, &str)]) -> String {
    let mut first_values: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, value) in form {
        first_values.entry(key).or_insert(value);
    }
    let mut signed = post_url.to_string();
    for (key, value) in &first_values {
        signed.push_str(key);
        signed.push_str(value);
    }
    BASE64_STD.encode(sha1::hmac_sha1(auth_token, signed.as_bytes()))
}

/// Minimal SHA-1 + HMAC-SHA1, hand-rolled because the workspace crypto
/// catalog is SHA-2-only. SHA-1 is broken for collision resistance but
/// remains the algorithm Twilio mandates for its signature header; this
/// implementation exists solely to produce test fixtures for it.
mod sha1 {
    const BLOCK_SIZE: usize = 64;
    const DIGEST_SIZE: usize = 20;

    /// Computes the SHA-1 digest of `data` (FIPS 180-4).
    pub(super) fn sha1(data: &[u8]) -> [u8; DIGEST_SIZE] {
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
    pub(super) fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; DIGEST_SIZE] {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of Go TestSignHMAC: recompute the expectation from primitives.
    #[test]
    fn sign_hmac_matches_primitive_hmac() {
        let secret = b"s";
        let body = br#"{"x":1}"#;
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert_eq!(sign_hmac(secret, body), expected);
    }

    // Cross-port fixture generated by the Go implementation.
    #[test]
    fn sign_hmac_matches_go_fixture() {
        assert_eq!(
            sign_hmac(b"s", br#"{"x":1}"#),
            "sha256=e3e9e9ec7b99f31f6f5cb4d54d50d3902e3d99da31a1ab3c4e6138c5a5d201c8"
        );
        assert_eq!(
            sign_hmac(b"whsec_test", br#"{"type":"charge.succeeded"}"#),
            "sha256=323b21232d29ba9946f66fb4eddaef1f4ada2f6dc08e04a03f7349d54c386770"
        );
    }

    // Port of Go TestSignStripe.
    #[test]
    fn sign_stripe_has_timestamp_prefix() {
        let sig = sign_stripe(b"s", b"body", 1_700_000_000);
        assert!(sig.starts_with("t=1700000000,v1="), "sig: {sig}");
    }

    // Cross-port fixture generated by the Go implementation.
    #[test]
    fn sign_stripe_matches_go_fixture() {
        assert_eq!(
            sign_stripe(b"s", b"body", 1_700_000_000),
            "t=1700000000,v1=d7a89816da081146f8ed6b56d04d36bc5e1a4e4192ab67113cccc30a9a27296b"
        );
    }

    // Port of Go TestSignTwilio.
    #[test]
    fn sign_twilio_is_not_empty() {
        let form = [("From", "+1"), ("Body", "hi")];
        let sig = sign_twilio(b"tok", "https://example.com/cb", &form);
        assert!(!sig.is_empty());
    }

    // Cross-port fixture generated by the Go implementation.
    #[test]
    fn sign_twilio_matches_go_fixture() {
        let form = [("From", "+1"), ("Body", "hi")];
        assert_eq!(
            sign_twilio(b"tok", "https://example.com/cb", &form),
            "0kN7lDY4nCZcisXKS85HEOaZTVw="
        );
    }

    // Go's url.Values is unordered; key sort order must not depend on the
    // order pairs were supplied in.
    #[test]
    fn sign_twilio_sorts_keys() {
        let unsorted = [("From", "+1"), ("Body", "hi")];
        let sorted = [("Body", "hi"), ("From", "+1")];
        assert_eq!(
            sign_twilio(b"tok", "https://example.com/cb", &unsorted),
            sign_twilio(b"tok", "https://example.com/cb", &sorted),
        );
    }

    // Go's url.Values.Get returns the first value of a repeated key.
    #[test]
    fn sign_twilio_uses_first_value_of_repeated_key() {
        let repeated = [("From", "+1"), ("From", "+2"), ("Body", "hi")];
        assert_eq!(
            sign_twilio(b"tok", "https://example.com/cb", &repeated),
            "0kN7lDY4nCZcisXKS85HEOaZTVw=",
        );
    }

    #[test]
    fn sign_github_is_alias_of_sign_hmac() {
        assert_eq!(sign_github(b"s", b"payload"), sign_hmac(b"s", b"payload"));
    }

    // FIPS 180-4 / RFC 3174 known-answer vectors for the hand-rolled SHA-1.
    #[test]
    fn sha1_known_vectors() {
        assert_eq!(
            hex::encode(sha1::sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex::encode(sha1::sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex::encode(sha1::sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // RFC 3174 TEST3: one million repetitions of "a" (multi-block path).
        assert_eq!(
            hex::encode(sha1::sha1(&vec![b'a'; 1_000_000])),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    // RFC 2202 test cases 1-3 for HMAC-SHA1.
    #[test]
    fn hmac_sha1_rfc2202_vectors() {
        assert_eq!(
            hex::encode(sha1::hmac_sha1(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        assert_eq!(
            hex::encode(sha1::hmac_sha1(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            hex::encode(sha1::hmac_sha1(&[0xaa; 20], &[0xdd; 50])),
            "125d7342b9ac11cd91a39af48aa17b4f63f175d3"
        );
    }

    // RFC 2202 test case 6: key longer than the 64-byte block.
    #[test]
    fn hmac_sha1_long_key_is_hashed_first() {
        assert_eq!(
            hex::encode(sha1::hmac_sha1(
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }
}
