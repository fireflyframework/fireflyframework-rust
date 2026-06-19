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

//! Opaque-token introspection (RFC 7662) — the Rust analog of Spring
//! Security's `OpaqueTokenIntrospector`.
//!
//! A resource server that receives **opaque** (non-JWT) bearer tokens cannot
//! validate them locally; it asks the authorization server's introspection
//! endpoint "is this token active, and who/what does it represent?". A
//! [`RemoteTokenIntrospector`] POSTs the token to the configured RFC 7662
//! endpoint (authenticating as a client), and — when the response says
//! `active: true` — maps its claims to an [`Authentication`]. It implements
//! [`Verifier`](crate::Verifier), so it is a drop-in alternative to
//! [`JwksVerifier`](crate::JwksVerifier) behind a
//! [`BearerLayer`](crate::BearerLayer) for the opaque-token resource-server
//! pattern (the AS stays the source of truth; nothing is trusted locally).
//!
//! It fails **closed**: a transport error, a non-2xx response, a non-JSON
//! body, or `active: false`/absent all reject the token.

use async_trait::async_trait;
use http::header;
use serde_json::Value;

use crate::authentication::{Authentication, SecurityError, Verifier};
use crate::jwks::claims_to_authentication;

/// Introspects an opaque token — Spring's `OpaqueTokenIntrospector`.
#[async_trait]
pub trait TokenIntrospector: Send + Sync {
    /// Resolves `token` to an [`Authentication`], or a [`SecurityError`] when it
    /// is inactive, unknown, or unverifiable.
    async fn introspect(&self, token: &str) -> Result<Authentication, SecurityError>;
}

/// A [`TokenIntrospector`] backed by a remote RFC 7662 introspection endpoint —
/// Spring's `NimbusOpaqueTokenIntrospector`.
///
/// Authenticates to the endpoint with HTTP Basic client credentials and POSTs
/// `token=<token>` (form-encoded), per RFC 7662 §2.1.
pub struct RemoteTokenIntrospector {
    introspection_uri: String,
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
}

impl RemoteTokenIntrospector {
    /// Builds an introspector for `introspection_uri`, authenticating as
    /// `client_id` / `client_secret`.
    #[must_use]
    pub fn new(
        introspection_uri: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        Self {
            introspection_uri: introspection_uri.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            http: reqwest::Client::new(),
        }
    }
}

impl std::fmt::Debug for RemoteTokenIntrospector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteTokenIntrospector")
            .field("introspection_uri", &self.introspection_uri)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TokenIntrospector for RemoteTokenIntrospector {
    async fn introspect(&self, token: &str) -> Result<Authentication, SecurityError> {
        let response = self
            .http
            .post(&self.introspection_uri)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| {
                SecurityError::verification(format!("introspection request failed: {e}"))
            })?;
        if !response.status().is_success() {
            return Err(SecurityError::verification(format!(
                "introspection endpoint returned {}",
                response.status()
            )));
        }
        let body: Value = response.json().await.map_err(|e| {
            SecurityError::verification(format!("introspection response not JSON: {e}"))
        })?;
        authentication_from_introspection(&body)
    }
}

/// A [`RemoteTokenIntrospector`] is a resource-server [`Verifier`]: dropping it
/// into a [`BearerLayer`](crate::BearerLayer) makes the layer validate opaque
/// tokens by introspection instead of local JWT verification.
#[async_trait]
impl Verifier for RemoteTokenIntrospector {
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError> {
        self.introspect(token).await
    }
}

/// Maps an RFC 7662 introspection response to an [`Authentication`], requiring
/// `active: true`. The remaining claims (`sub`, `scope`, `username`, …) are
/// mapped exactly as JWT claims are, so a principal authenticated by
/// introspection looks identical to one authenticated by JWT.
pub(crate) fn authentication_from_introspection(
    response: &Value,
) -> Result<Authentication, SecurityError> {
    let claims = response.as_object().ok_or_else(|| {
        SecurityError::verification("introspection response is not a JSON object")
    })?;
    // RFC 7662 §2.2: `active` is REQUIRED; anything else means the token is not
    // valid for use.
    let active = claims
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !active {
        return Err(SecurityError::verification("token is inactive or unknown"));
    }
    let mut auth = claims_to_authentication(claims);
    // RFC 7662 §2.2 names the resource owner in `username`; honor it over the
    // OIDC `preferred_username`/`sub` fallback used for JWT claims.
    if let Some(username) = claims.get("username").and_then(Value::as_str) {
        auth.username = username.to_owned();
    }
    Ok(auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn active_response_maps_to_authentication() {
        let resp = json!({
            "active": true,
            "sub": "user-7",
            "username": "alice",
            "scope": "read write",
            "client_id": "svc"
        });
        let auth = authentication_from_introspection(&resp).expect("active");
        assert_eq!(auth.principal, "user-7");
        assert_eq!(auth.username, "alice");
        // Space-separated scope becomes authorities.
        assert!(auth.has_authority("read"));
        assert!(auth.has_authority("write"));
    }

    #[test]
    fn inactive_or_missing_active_is_rejected() {
        // active: false → reject.
        assert!(authentication_from_introspection(&json!({"active": false, "sub": "x"})).is_err());
        // active absent → reject (fail closed).
        assert!(authentication_from_introspection(&json!({"sub": "x"})).is_err());
        // not an object → reject.
        assert!(authentication_from_introspection(&json!("nope")).is_err());
    }
}
