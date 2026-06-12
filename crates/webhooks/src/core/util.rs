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

//! Small shared crypto helpers — the Rust spelling of Go's
//! `webhooks/core/util.go`.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Computes the hex-encoded HMAC-SHA256 of `msg` with `key` — Go's
/// `computeHMACHex`, used by the validators and the test suites.
pub(crate) fn compute_hmac_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    hex::encode(mac.finalize().into_bytes())
}

/// Computes the standard-base64 HMAC-SHA256 of `msg` with `key` — the
/// non-hex branch of the generic validator.
pub(crate) fn compute_hmac_base64(key: &[u8], msg: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD as BASE64_STD;
    use base64::Engine as _;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    BASE64_STD.encode(mac.finalize().into_bytes())
}

/// Constant-time byte-slice equality — the analog of Go's
/// `hmac.Equal`. Slices of different lengths compare unequal
/// immediately (length is not secret), matching
/// `subtle.ConstantTimeCompare`.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_compares_correctly() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn compute_hmac_hex_known_answer() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?".
        assert_eq!(
            compute_hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
