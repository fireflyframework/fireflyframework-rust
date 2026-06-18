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

//! Standalone symmetric JWT primitive (pyfly: `pyfly.security.jwt.JWTService`).
//!
//! [`JwtService`] encodes and decodes HMAC-signed JWTs (HS256 default,
//! HS384/HS512 configurable) without any IdP, user store, or HTTP layer.
//! It is the reusable counterpart to the RS256, verify-only
//! [`JwksVerifier`](crate::JwksVerifier): use it for symmetric-token APIs,
//! workers, CLIs, and inter-service tokens where both ends share a secret.
//!
//! Behaviour mirrors pyfly exactly:
//!
//! - [`JwtService::encode`] adds an `exp` claim (`now + expiration_seconds`)
//!   when the payload does not already carry one, so every issued token
//!   expires.
//! - [`JwtService::decode`] requires a valid signature **and** an `exp`
//!   claim — a token minted without `exp` (which would never expire) is
//!   rejected. Errors carry the pyfly message shape `Invalid token: <detail>`.
//! - [`JwtService::to_authentication`] decodes the token and maps `sub`,
//!   `roles`, and `permissions` to an [`Authentication`] (pyfly's
//!   `to_security_context`).
//!
//! Because it satisfies the crate's [`Verifier`] port, it drops straight
//! into [`BearerLayer`](crate::BearerLayer) for symmetric resource servers.
//!
//! ```rust
//! use firefly_security::JwtService;
//! use serde_json::json;
//!
//! let svc = JwtService::new("super-secret");
//! let token = svc.encode(json!({ "sub": "u1", "roles": ["ADMIN"] })).unwrap();
//! let auth = svc.to_authentication(&token).unwrap();
//! assert_eq!(auth.principal, "u1");
//! assert!(auth.has_role("ADMIN"));
//! ```

use async_trait::async_trait;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{Map, Value};

use crate::authentication::{Authentication, SecurityError, Verifier};

/// Default token lifetime applied by [`JwtService::encode`] when the
/// payload carries no `exp` claim (pyfly: `expiration_seconds=3600`).
pub const DEFAULT_EXPIRATION_SECONDS: u64 = 3600;

/// Symmetric (HMAC) JWT encode/decode service — the Rust port of
/// pyfly's `JWTService`.
///
/// Construct with [`JwtService::new`] for HS256 defaults, then optionally
/// override the algorithm with [`JwtService::algorithm`] and the default
/// expiration with [`JwtService::expiration_seconds`].
///
/// All three HMAC algorithms (`HS256`/`HS384`/`HS512`) are accepted; an
/// asymmetric algorithm passed to [`JwtService::algorithm`] is rejected at
/// construction time because the same shared secret cannot both sign and
/// verify an RSA/EC token — use [`JwksVerifier`](crate::JwksVerifier) for
/// those.
pub struct JwtService {
    algorithm: Algorithm,
    expiration_seconds: u64,
    leeway_seconds: u64,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl std::fmt::Debug for JwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtService")
            .field("algorithm", &self.algorithm)
            .field("expiration_seconds", &self.expiration_seconds)
            .finish_non_exhaustive()
    }
}

impl JwtService {
    /// Builds an HS256 service over `secret` with the default 1-hour token
    /// lifetime (pyfly's `JWTService(secret)` defaults).
    pub fn new(secret: impl AsRef<[u8]>) -> Self {
        let bytes = secret.as_ref();
        Self {
            algorithm: Algorithm::HS256,
            expiration_seconds: DEFAULT_EXPIRATION_SECONDS,
            leeway_seconds: crate::jwks::DEFAULT_CLOCK_SKEW_SECONDS,
            encoding_key: EncodingKey::from_secret(bytes),
            decoding_key: DecodingKey::from_secret(bytes),
        }
    }

    /// Overrides the signing algorithm. Only the HMAC family
    /// (`HS256`/`HS384`/`HS512`) is symmetric; passing any other algorithm
    /// returns [`SecurityError::Verification`] because a single shared
    /// secret cannot sign and verify an asymmetric token.
    ///
    /// Mirrors pyfly's `algorithm` constructor argument (which is also
    /// HMAC in practice).
    pub fn algorithm(mut self, algorithm: Algorithm) -> Result<Self, SecurityError> {
        if !is_hmac(algorithm) {
            return Err(SecurityError::verification(format!(
                "JwtService: algorithm {algorithm:?} is not a symmetric (HMAC) algorithm"
            )));
        }
        self.algorithm = algorithm;
        Ok(self)
    }

    /// Sets the default token lifetime applied by [`Self::encode`] when the
    /// payload carries no `exp` claim (pyfly: `expiration_seconds`).
    pub fn expiration_seconds(mut self, seconds: u64) -> Self {
        self.expiration_seconds = seconds;
        self
    }

    /// Overrides the clock-skew leeway in seconds applied to `exp`/`nbf`
    /// validation in [`Self::decode`] (default
    /// [`DEFAULT_CLOCK_SKEW_SECONDS`](crate::DEFAULT_CLOCK_SKEW_SECONDS),
    /// matching Spring's 60s `JwtTimestampValidator`).
    pub fn clock_skew_seconds(mut self, seconds: u64) -> Self {
        self.leeway_seconds = seconds;
        self
    }

    /// The configured signing algorithm.
    pub fn signing_algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Encodes `payload` into a signed JWT.
    ///
    /// When `payload` is a JSON object without an `exp` claim, an
    /// `exp = now + expiration_seconds` claim is injected so the token
    /// expires (pyfly: `encode` adds `exp`). A non-object payload, or one
    /// already carrying `exp`, is signed verbatim.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Verification`] if signing fails (its
    /// message uses the pyfly `Invalid token: <detail>` shape).
    pub fn encode(&self, payload: Value) -> Result<String, SecurityError> {
        let claims = self.with_default_exp(payload);
        encode(&Header::new(self.algorithm), &claims, &self.encoding_key).map_err(invalid_token)
    }

    /// Encodes a [`Map`] of claims (a convenience over [`Self::encode`] for
    /// callers that already hold a claims map).
    pub fn encode_claims(&self, claims: Map<String, Value>) -> Result<String, SecurityError> {
        self.encode(Value::Object(claims))
    }

    /// Decodes and validates `token`, returning its claims.
    ///
    /// Requires a valid signature **and** an `exp` claim; a token without
    /// `exp` is rejected (pyfly: `options={"require": ["exp"]}`).
    /// Audience validation is disabled (pyfly does not set an audience).
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::Verification`] with the pyfly message shape
    /// `Invalid token: <detail>` when the token is invalid, expired, or
    /// lacks `exp`.
    pub fn decode(&self, token: &str) -> Result<Map<String, Value>, SecurityError> {
        let mut validation = Validation::new(self.algorithm);
        // Spring's JwtTimestampValidator allows a small clock-skew window
        // (default 60s) on `exp`/`nbf`; the JWKS verifier matches.
        validation.leeway = self.leeway_seconds;
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp"]);
        validation.validate_aud = false;
        let data = decode::<Map<String, Value>>(token, &self.decoding_key, &validation)
            .map_err(invalid_token)?;
        Ok(data.claims)
    }

    /// Decodes `token` and maps its claims to an [`Authentication`] —
    /// pyfly's `to_security_context`. `sub` → [`Authentication::principal`]
    /// (also used as the username when no friendlier claim is present),
    /// `roles` → [`Authentication::roles`], and `permissions` →
    /// [`Authentication::authorities`]. Every claim is retained on
    /// [`Authentication::claims`].
    ///
    /// # Errors
    ///
    /// Propagates [`Self::decode`]'s errors.
    pub fn to_authentication(&self, token: &str) -> Result<Authentication, SecurityError> {
        let claims = self.decode(token)?;
        Ok(authentication_from_claims(&claims))
    }

    /// Injects an `exp` claim into an object payload that lacks one,
    /// leaving non-object payloads and pre-`exp`'d payloads untouched.
    fn with_default_exp(&self, payload: Value) -> Value {
        let Value::Object(mut map) = payload else {
            return payload;
        };
        if !map.contains_key("exp") {
            let exp = now_secs().saturating_add(self.expiration_seconds);
            map.insert("exp".to_string(), Value::from(exp));
        }
        Value::Object(map)
    }
}

/// Maps decoded JWT claims to an [`Authentication`] the way pyfly's
/// `JWTService.to_security_context` builds a `SecurityContext`: `sub` is
/// the principal/username, `roles`/`permissions` populate the role and
/// authority lists, and all claims are kept.
pub fn authentication_from_claims(claims: &Map<String, Value>) -> Authentication {
    let principal = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let username = ["preferred_username", "name"]
        .iter()
        .find_map(|k| claims.get(*k).and_then(Value::as_str))
        .unwrap_or(&principal)
        .to_string();
    Authentication {
        principal,
        username,
        roles: string_vec(claims.get("roles")),
        authorities: string_vec(claims.get("permissions")),
        claims: claims.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

/// Collects the string items of a JSON array claim (ignoring non-strings).
fn string_vec(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `algorithm` is in the HMAC (symmetric) family.
fn is_hmac(algorithm: Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512
    )
}

/// Wraps a jsonwebtoken error in the pyfly message shape.
fn invalid_token(err: jsonwebtoken::errors::Error) -> SecurityError {
    SecurityError::verification(format!("Invalid token: {err}"))
}

/// The current wall-clock time in epoch seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[async_trait]
impl Verifier for JwtService {
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError> {
        self.to_authentication(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn svc() -> JwtService {
        JwtService::new("test-secret")
    }

    // Ported from pyfly: test_encode_decode_roundtrip
    #[test]
    fn encode_decode_roundtrip() {
        let svc = svc();
        let token = svc
            .encode(json!({ "sub": "u1", "roles": ["ADMIN"] }))
            .unwrap();
        let claims = svc.decode(&token).unwrap();
        assert_eq!(claims["sub"], json!("u1"));
        assert_eq!(claims["roles"], json!(["ADMIN"]));
        // exp was injected.
        assert!(claims.contains_key("exp"));
    }

    // Ported from pyfly: test_encode_adds_exp_when_missing
    #[test]
    fn encode_adds_exp_when_missing() {
        let svc = svc().expiration_seconds(120);
        let token = svc.encode(json!({ "sub": "u1" })).unwrap();
        let claims = svc.decode(&token).unwrap();
        let exp = claims["exp"].as_u64().unwrap();
        let now = now_secs();
        // exp is roughly now + 120 (allow a little wall-clock slack).
        assert!(
            exp >= now + 100 && exp <= now + 140,
            "exp was {exp}, now {now}"
        );
    }

    #[test]
    fn encode_preserves_explicit_exp() {
        let svc = svc();
        let explicit = now_secs() + 5000;
        let token = svc.encode(json!({ "sub": "u1", "exp": explicit })).unwrap();
        let claims = svc.decode(&token).unwrap();
        assert_eq!(claims["exp"].as_u64().unwrap(), explicit);
    }

    // Ported from pyfly: test_decode_rejects_token_without_exp
    #[test]
    fn decode_rejects_token_without_exp() {
        let svc = svc();
        // Mint a token with an explicit far-future exp, then mint one
        // with no exp by signing a raw header.payload.sig directly.
        // Easiest: encode with exp, then craft one WITHOUT exp via a second
        // service whose payload bypasses with_default_exp by being signed raw.
        // We instead assert that a token carrying only `sub` (and exp added)
        // round-trips, and that stripping exp fails. Construct an exp-less
        // token by signing claims directly.
        let header = Header::new(Algorithm::HS256);
        let key = EncodingKey::from_secret(b"test-secret");
        let exp_less = encode(&header, &json!({ "sub": "u1" }), &key).unwrap();
        let err = svc.decode(&exp_less).unwrap_err();
        assert!(err.to_string().starts_with("Invalid token:"), "got {err}");
    }

    // Ported from pyfly: test_decode_rejects_invalid_signature
    #[test]
    fn decode_rejects_wrong_secret() {
        let issuer = JwtService::new("secret-a");
        let verifier = JwtService::new("secret-b");
        let token = issuer.encode(json!({ "sub": "u1" })).unwrap();
        let err = verifier.decode(&token).unwrap_err();
        assert!(err.to_string().starts_with("Invalid token:"), "got {err}");
    }

    #[test]
    fn decode_rejects_expired() {
        let svc = svc();
        // exp well in the past (beyond any clock leeway).
        let token = svc
            .encode(json!({ "sub": "u1", "exp": now_secs() - 3600 }))
            .unwrap();
        let err = svc.decode(&token).unwrap_err();
        assert!(err.to_string().starts_with("Invalid token:"), "got {err}");
    }

    // H6: an exp just barely in the past (within the default 60s clock-skew
    // leeway) is still accepted, matching Spring's JwtTimestampValidator.
    #[test]
    fn decode_allows_exp_within_clock_skew_leeway() {
        let svc = svc();
        let token = svc
            .encode(json!({ "sub": "u1", "exp": now_secs() - 30 }))
            .unwrap();
        assert!(svc.decode(&token).is_ok());
    }

    // H7: a token whose nbf is in the future (beyond leeway) is rejected.
    #[test]
    fn decode_rejects_future_nbf() {
        let svc = svc();
        let token = svc
            .encode(json!({ "sub": "u1", "nbf": now_secs() + 3600, "exp": now_secs() + 7200 }))
            .unwrap();
        assert!(svc.decode(&token).is_err());
    }

    // Ported from pyfly: test_to_security_context
    #[test]
    fn to_authentication_maps_claims() {
        let svc = svc();
        let token = svc
            .encode(json!({
                "sub": "user-42",
                "roles": ["admin", "editor"],
                "permissions": ["read", "write"],
            }))
            .unwrap();
        let auth = svc.to_authentication(&token).unwrap();
        assert_eq!(auth.principal, "user-42");
        assert_eq!(auth.username, "user-42");
        assert_eq!(auth.roles, vec!["admin", "editor"]);
        assert_eq!(auth.authorities, vec!["read", "write"]);
    }

    #[test]
    fn to_authentication_defaults_missing_lists() {
        let svc = svc();
        let token = svc.encode(json!({ "sub": "u1" })).unwrap();
        let auth = svc.to_authentication(&token).unwrap();
        assert_eq!(auth.principal, "u1");
        assert!(auth.roles.is_empty());
        assert!(auth.authorities.is_empty());
    }

    #[test]
    fn to_authentication_prefers_friendly_username() {
        let svc = svc();
        let token = svc
            .encode(json!({ "sub": "u1", "preferred_username": "alice" }))
            .unwrap();
        let auth = svc.to_authentication(&token).unwrap();
        assert_eq!(auth.username, "alice");
    }

    #[test]
    fn hs384_and_hs512_roundtrip() {
        for alg in [Algorithm::HS384, Algorithm::HS512] {
            let svc = JwtService::new("k").algorithm(alg).unwrap();
            assert_eq!(svc.signing_algorithm(), alg);
            let token = svc.encode(json!({ "sub": "u1" })).unwrap();
            assert_eq!(svc.decode(&token).unwrap()["sub"], json!("u1"));
        }
    }

    #[test]
    fn algorithm_rejects_asymmetric() {
        let err = JwtService::new("k")
            .algorithm(Algorithm::RS256)
            .unwrap_err();
        assert!(err.to_string().contains("not a symmetric"), "got {err}");
    }

    #[test]
    fn algorithm_mismatch_fails_decode() {
        let issuer = JwtService::new("k");
        let verifier = JwtService::new("k").algorithm(Algorithm::HS512).unwrap();
        let token = issuer.encode(json!({ "sub": "u1" })).unwrap();
        // HS256 token verified under HS512 fails.
        assert!(verifier.decode(&token).is_err());
    }

    #[tokio::test]
    async fn verifier_port_authenticates() {
        let svc = svc();
        let token = svc
            .encode(json!({ "sub": "u1", "roles": ["USER"] }))
            .unwrap();
        let auth = svc.verify(&token).await.unwrap();
        assert_eq!(auth.principal, "u1");
        assert!(auth.has_role("USER"));
        assert!(svc.verify("garbage").await.is_err());
    }

    #[test]
    fn non_object_payload_signed_verbatim() {
        let svc = svc();
        // A bare array payload cannot carry exp; decode (which requires exp)
        // then fails — but encode itself must succeed.
        let token = svc.encode(json!([1, 2, 3]));
        assert!(token.is_ok());
    }
}
