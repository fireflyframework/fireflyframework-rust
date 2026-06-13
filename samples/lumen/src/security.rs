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

//! JWT authentication for the mutating routes — built entirely on
//! [`firefly::security`] (book chapter 14, "Security").
//!
//! Lumen uses a symmetric (HS256) signing key so it is fully self-contained:
//! the framework's [`JwtService`] both **mints** the demo tokens (used by the
//! tests) and **verifies** them, so no external IdP / JWKS endpoint is needed
//! to run or test. In production you would swap [`build_verifier`] for
//! [`firefly::security::JwksVerifier`] pointed at your IdP's JWKS URI — the
//! [`Verifier`] port is identical, so nothing else changes.
//!
//! - [`mint_token`] signs a token for a subject + roles.
//! - [`build_verifier`] returns a [`Verifier`] that validates the bearer
//!   token and maps its claims onto an [`Authentication`].
//! - [`security_layers`] composes the [`BearerLayer`] (token extraction +
//!   verification) and the [`FilterChain`] (path-based RBAC) that protect the
//!   mutating routes; the read and stream routes stay public.

use firefly::security::{
    BearerConfig, BearerLayer, FilterChain, JwtService, SecurityError, Verifier, VerifierFn,
};
use serde_json::json;

/// The demo signing key. A real service reads this from configuration / a
/// secret store; it is inlined here so the sample is runnable as-is.
pub const DEMO_SIGNING_KEY: &[u8] = b"lumen-demo-signing-key-change-me";

/// The role every mutating wallet command requires.
pub const CUSTOMER_ROLE: &str = "CUSTOMER";

/// The shared HS256 service that both signs the demo tokens and verifies
/// incoming bearer tokens.
fn jwt_service() -> JwtService {
    JwtService::new(DEMO_SIGNING_KEY)
}

/// Mints a signed HS256 access token for `subject` with `roles`, valid for the
/// service's default lifetime (one hour). The tests call this to obtain a
/// credential [`build_verifier`] will accept.
///
/// # Panics
/// Panics only if the framework's JWT service cannot sign the claim set —
/// impossible for the fixed claim shape used here.
pub fn mint_token(subject: &str, roles: &[&str]) -> String {
    jwt_service()
        .encode(json!({ "sub": subject, "roles": roles }))
        .expect("mint_token: HS256 encode")
}

/// Builds the resource-server [`Verifier`]: validates the token's HS256
/// signature + expiry, then maps `sub` → principal and `roles` → roles onto an
/// [`Authentication`]. A bad signature / expired token surfaces as a
/// [`SecurityError::Verification`], which the [`BearerLayer`] renders as a
/// `401 application/problem+json`.
pub fn build_verifier() -> impl Verifier {
    VerifierFn(|token: String| async move {
        jwt_service()
            .to_authentication(&token)
            .map_err(|e: SecurityError| SecurityError::verification(format!("invalid token: {e}")))
    })
}

/// Builds the [`BearerLayer`] + [`FilterChain`] that protect the service.
///
/// Rules are evaluated in declaration order (first match wins): the public
/// `GET` reads and the management surface are permitted first, then the
/// mutating `POST /api/v1/` routes require the `CUSTOMER` role.
///
/// | Route                                          | Rule                  |
/// |------------------------------------------------|-----------------------|
/// | `GET  /api/v1/wallets/:id`                      | permit (public read)  |
/// | `GET  /actuator/*`                              | permit (management)   |
/// | `POST /api/v1/wallets`                          | require `CUSTOMER`    |
/// | `POST /api/v1/wallets/:id/deposit` / `withdraw` | require `CUSTOMER`    |
/// | `POST /api/v1/transfers`                        | require `CUSTOMER`    |
pub fn security_layers() -> (BearerLayer, FilterChain) {
    // `allow_anonymous` lets an unauthenticated request reach the chain; the
    // chain (not the bearer layer) then decides — a 401 on a `require` route
    // without a valid token, a pass on a permitted route.
    let bearer = BearerLayer::new(BearerConfig::new(build_verifier()).allow_anonymous(true));
    let chain = FilterChain::new()
        .permit_method("GET", "/api/v1/wallets")
        .permit("/actuator/")
        .require("/api/v1/wallets", &[CUSTOMER_ROLE])
        .require("/api/v1/transfers", &[CUSTOMER_ROLE])
        .any_request_permit();
    (bearer, chain)
}

#[cfg(test)]
mod tests {
    use firefly::security::Authentication;

    use super::*;

    #[tokio::test]
    async fn mint_then_verify_roundtrips_claims() {
        let token = mint_token("u-alice", &[CUSTOMER_ROLE]);
        let auth: Authentication = build_verifier().verify(&token).await.unwrap();
        assert_eq!(auth.principal, "u-alice");
        assert!(auth.has_role(CUSTOMER_ROLE));
    }

    #[tokio::test]
    async fn tampered_token_is_rejected() {
        let err = build_verifier().verify("not.a.jwt").await.unwrap_err();
        assert!(matches!(err, SecurityError::Verification(_)));
    }

    #[tokio::test]
    async fn token_signed_with_wrong_key_is_rejected() {
        let bogus = JwtService::new(b"the-wrong-key" as &[u8])
            .encode(json!({ "sub": "mallory", "roles": [CUSTOMER_ROLE] }))
            .unwrap();
        assert!(build_verifier().verify(&bogus).await.is_err());
    }
}
