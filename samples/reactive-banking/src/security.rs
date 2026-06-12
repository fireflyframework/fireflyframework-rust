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

//! JWT authentication for the mutating routes — a self-contained HS256
//! resource-server verifier built on [`firefly_security`].
//!
//! The sample uses a symmetric (HS256) signing key so it is fully
//! self-contained (no external IdP / JWKS endpoint needed to run or test).
//! In production you would swap [`build_verifier`] for
//! [`firefly_security::JwksVerifier`] pointed at your IdP's JWKS URI — the
//! [`Verifier`] port is identical, so nothing else changes.
//!
//! - [`build_verifier`] returns a [`Verifier`] that validates the bearer
//!   token's HS256 signature and maps its claims onto an
//!   [`Authentication`].
//! - [`mint_token`] signs a token for a subject + roles — used by the SDK
//!   and the e2e tests to obtain a valid credential.
//! - [`security_layers`] composes the [`BearerLayer`] +
//!   [`FilterChain`] that protect the mutating routes (the read and stream
//!   routes stay public).

use std::collections::HashMap;

use chrono::{Duration, Utc};
use firefly_security::{
    Authentication, BearerConfig, BearerLayer, FilterChain, SecurityError, Verifier, VerifierFn,
};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// The demo signing key. A real service reads this from configuration /
/// a secret store; it is inlined here so the sample is runnable as-is.
pub const DEMO_SIGNING_KEY: &[u8] = b"reactive-banking-demo-signing-key-change-me";

/// The token issuer the verifier expects (and [`mint_token`] stamps).
pub const ISSUER: &str = "reactive-banking";

/// The HS256 claim set carried by a banking access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Claims {
    /// Subject (the principal id).
    sub: String,
    /// Issuer.
    iss: String,
    /// Granted roles (e.g. `["CUSTOMER"]`).
    roles: Vec<String>,
    /// Expiry (Unix seconds).
    exp: i64,
}

/// Mints a signed HS256 access token for `subject` with `roles`, valid for
/// one hour. The SDK and the e2e tests call this to obtain a credential the
/// [`build_verifier`] verifier will accept.
///
/// # Panics
///
/// Panics only if the JWT library cannot encode the claim set — impossible
/// for the fixed claim shape used here.
pub fn mint_token(subject: &str, roles: &[&str]) -> String {
    let claims = Claims {
        sub: subject.to_owned(),
        iss: ISSUER.to_owned(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        exp: (Utc::now() + Duration::hours(1)).timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(DEMO_SIGNING_KEY),
    )
    .expect("mint_token: HS256 encode")
}

/// Builds the resource-server [`Verifier`]: validates the token's HS256
/// signature, issuer, and expiry, then maps the claims onto an
/// [`Authentication`] (principal = `sub`, roles = `roles`, raw claims
/// retained).
///
/// A signature / expiry / issuer mismatch surfaces as a
/// [`SecurityError::Verification`], which the [`BearerLayer`] renders as a
/// `401 application/problem+json`.
pub fn build_verifier() -> impl Verifier {
    VerifierFn(|token: String| async move {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        let data = decode::<Claims>(
            &token,
            &DecodingKey::from_secret(DEMO_SIGNING_KEY),
            &validation,
        )
        .map_err(|e| SecurityError::verification(format!("invalid token: {e}")))?;

        let claims = data.claims;
        let mut raw: HashMap<String, serde_json::Value> = HashMap::new();
        raw.insert("sub".into(), serde_json::json!(claims.sub));
        raw.insert("iss".into(), serde_json::json!(claims.iss));
        raw.insert("roles".into(), serde_json::json!(claims.roles));

        Ok(Authentication {
            principal: claims.sub.clone(),
            username: claims.sub,
            roles: claims.roles,
            authorities: Vec::new(),
            claims: raw,
        })
    })
}

/// The role every mutating banking command requires.
pub const CUSTOMER_ROLE: &str = "CUSTOMER";

/// Builds the [`BearerLayer`] (token extraction + verification) and the
/// [`FilterChain`] (path-based RBAC) that protect the service.
///
/// Rules are evaluated in declaration order (first match wins), so the
/// **`GET` reads and the streaming events endpoint are permitted first**,
/// and the remaining (mutating `POST`) `/api/v1/` requests then fall through
/// to the `require(CUSTOMER)` rules. The actuator surface stays public. The
/// bearer layer wraps the chain so the [`Authentication`] the `require`
/// rules read has already been populated.
///
/// | Route                                         | Rule                          |
/// |-----------------------------------------------|-------------------------------|
/// | `GET  /api/v1/accounts/:id`                    | permit (public read)          |
/// | `GET  /api/v1/accounts/:id/events`             | permit (public stream)        |
/// | `GET  /actuator/*`                             | permit (management)           |
/// | `POST /api/v1/accounts`                        | require `CUSTOMER`            |
/// | `POST /api/v1/accounts/:id/deposit|withdraw`   | require `CUSTOMER`            |
/// | `POST /api/v1/transfers`                       | require `CUSTOMER`            |
pub fn security_layers() -> (BearerLayer, FilterChain) {
    // `allow_anonymous` lets an unauthenticated request reach the chain; the
    // chain (not the bearer layer) then decides — a 401 on a `require` route
    // without a valid token, a pass on a permitted route.
    let bearer = BearerLayer::new(BearerConfig::new(build_verifier()).allow_anonymous(true));
    let chain = FilterChain::new()
        // Public: GET reads + the streaming events endpoint + actuator.
        .permit_method("GET", "/api/v1/accounts")
        .permit("/actuator/")
        // Protected: every mutating banking command needs the CUSTOMER role.
        .require("/api/v1/accounts", &[CUSTOMER_ROLE])
        .require("/api/v1/transfers", &[CUSTOMER_ROLE])
        .any_request_permit();
    (bearer, chain)
}

#[cfg(test)]
mod tests {
    use firefly_security::Verifier;

    use super::*;

    #[tokio::test]
    async fn mint_then_verify_roundtrips_claims() {
        let token = mint_token("u-alice", &["CUSTOMER"]);
        let verifier = build_verifier();
        let auth = verifier.verify(&token).await.unwrap();
        assert_eq!(auth.principal, "u-alice");
        assert_eq!(auth.username, "u-alice");
        assert!(auth.has_role("CUSTOMER"));
    }

    #[tokio::test]
    async fn tampered_token_is_rejected() {
        let verifier = build_verifier();
        let err = verifier.verify("not.a.jwt").await.unwrap_err();
        assert!(matches!(err, SecurityError::Verification(_)));
    }

    #[tokio::test]
    async fn token_signed_with_wrong_key_is_rejected() {
        let bogus = encode(
            &Header::default(),
            &Claims {
                sub: "mallory".into(),
                iss: ISSUER.into(),
                roles: vec!["CUSTOMER".into()],
                exp: (Utc::now() + Duration::hours(1)).timestamp(),
            },
            &EncodingKey::from_secret(b"the-wrong-key"),
        )
        .unwrap();
        let verifier = build_verifier();
        assert!(verifier.verify(&bogus).await.is_err());
    }
}
