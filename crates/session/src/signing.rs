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

//! [`SessionSigner`] — optional HMAC-SHA256 signing of session ids in the
//! cookie value, so a tampered or forged cookie is rejected before the
//! store is ever consulted.
//!
//! pyfly issues the bare session id as the cookie value (the store id *is*
//! a 128-bit UUID, unguessable, and the only authority); this is a Rust
//! hardening the brief calls for ("signed session ids (hmac)"). When a
//! [`crate::SessionLayer`] is configured with a signer, the cookie value
//! becomes `<id>.<base64url(hmac)>`; on read the MAC is verified in
//! constant time and stripped back to the id before the store lookup. With
//! no signer configured the cookie carries the raw id (pyfly parity).
//!
//! The signature covers only the id, not the session data — the data lives
//! server-side in the store, exactly as in pyfly, so this is integrity for
//! the *cookie*, not a client-side session.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The separator between the session id and its base64url signature in a
/// signed cookie value.
const SEP: char = '.';

/// Signs and verifies session-id cookie values with HMAC-SHA256.
///
/// Construct with a secret key ([`SessionSigner::new`]). [`Self::sign`]
/// produces the cookie value; [`Self::verify`] checks it and returns the
/// embedded id, or `None` if the value is malformed or the MAC does not
/// match (constant-time comparison).
#[derive(Clone)]
pub struct SessionSigner {
    key: Vec<u8>,
}

impl std::fmt::Debug for SessionSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak the key.
        f.debug_struct("SessionSigner").finish_non_exhaustive()
    }
}

impl SessionSigner {
    /// Creates a signer keyed by `secret`. Any non-empty byte string works;
    /// 32+ random bytes are recommended.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { key: secret.into() }
    }

    fn mac(&self, id: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(id.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// Returns the cookie value for `session_id`: `<id>.<base64url(hmac)>`.
    #[must_use]
    pub fn sign(&self, session_id: &str) -> String {
        format!("{session_id}{SEP}{}", self.mac(session_id))
    }

    /// Verifies a signed cookie `value` and returns the embedded session id
    /// when the MAC matches (constant-time), or `None` otherwise.
    #[must_use]
    pub fn verify(&self, value: &str) -> Option<String> {
        let (id, sig) = value.rsplit_once(SEP)?;
        let provided = URL_SAFE_NO_PAD.decode(sig).ok()?;
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(id.as_bytes());
        // `verify_slice` is a constant-time comparison.
        mac.verify_slice(&provided).ok()?;
        Some(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrips() {
        let signer = SessionSigner::new("secret-key");
        let signed = signer.sign("abc123");
        assert!(signed.starts_with("abc123."));
        assert_eq!(signer.verify(&signed).as_deref(), Some("abc123"));
    }

    #[test]
    fn tampered_id_is_rejected() {
        let signer = SessionSigner::new("secret-key");
        let signed = signer.sign("abc123");
        let tampered = signed.replacen("abc123", "evil12", 1);
        assert_eq!(signer.verify(&tampered), None);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let signed = SessionSigner::new("key-a").sign("abc123");
        assert_eq!(SessionSigner::new("key-b").verify(&signed), None);
    }

    #[test]
    fn malformed_value_is_rejected() {
        let signer = SessionSigner::new("secret-key");
        assert_eq!(signer.verify("no-separator"), None);
        assert_eq!(signer.verify("id.@@@not-base64@@@"), None);
        assert_eq!(signer.verify(""), None);
    }

    #[test]
    fn id_with_dots_roundtrips() {
        // rsplit_once keeps dotted ids intact (signature is the last field).
        let signer = SessionSigner::new("secret-key");
        let signed = signer.sign("a.b.c");
        assert_eq!(signer.verify(&signed).as_deref(), Some("a.b.c"));
    }

    #[test]
    fn debug_does_not_leak_key() {
        let dbg = format!("{:?}", SessionSigner::new("top-secret"));
        assert!(!dbg.contains("top-secret"));
    }
}
