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

//! Config-driven security — build a [`Verifier`] and a [`BearerLayer`] from
//! configuration instead of by hand.
//!
//! [`SecurityProperties`] binds straight from a configuration source (a plain
//! `serde` struct), and the constructors below turn it into wiring:
//!
//! * [`verifier_from_config`] yields a JWKS resource-server [`Verifier`] when a
//!   `jwk-set-uri` is configured, an HMAC [`JwtService`] verifier when a shared
//!   `secret` is configured, and `None` when neither is — the same precedence
//!   as a Spring resource server vs. a symmetric-secret setup.
//! * [`bearer_layer_from_config`] wraps that verifier in a ready-to-mount
//!   [`BearerLayer`] honouring the header name + anonymous policy.
//!
//! This is the DI-free constructor half (the same split the framework uses for
//! its content/notification adapters): the application binds the struct from
//! its config and calls these at startup; nothing here touches the DI
//! container. Config keys mirror Spring's `security.oauth2.resourceserver.jwt`.

use std::sync::Arc;

use serde::Deserialize;

use crate::{
    Algorithm, BearerConfig, BearerLayer, JwksVerifier, JwtService, SecurityError, Verifier,
};

/// The security wiring bound from configuration. Bind it from any prefix, e.g.
/// `firefly.security.*`:
///
/// ```yaml
/// firefly:
///   security:
///     jwt:
///       jwk-set-uri: "https://idp.example.com/.well-known/jwks.json"
///       issuer-uri: "https://idp.example.com/"
///       audience: "orders-api"
///     bearer:
///       allow-anonymous: false
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SecurityProperties {
    /// JWT verification settings (resource-server JWKS or symmetric HMAC).
    pub jwt: JwtProperties,
    /// Bearer-token middleware settings.
    pub bearer: BearerProperties,
}

/// JWT verification settings. A non-empty `jwk_set_uri` selects an RS256
/// resource-server verifier; otherwise a non-empty `secret` selects an HMAC
/// verifier.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JwtProperties {
    /// Remote JWKS endpoint for an RS256 resource server (Spring's
    /// `jwk-set-uri`). When set, [`verifier_from_config`] builds a
    /// [`JwksVerifier`].
    pub jwk_set_uri: String,
    /// Expected `iss` claim (optional).
    pub issuer_uri: String,
    /// Expected `aud` claim (optional).
    pub audience: String,
    /// Shared HMAC secret for a symmetric verifier. Used only when
    /// `jwk_set_uri` is empty; builds a [`JwtService`].
    pub secret: String,
    /// HMAC algorithm name — `HS256` (default), `HS384`, or `HS512`.
    pub algorithm: String,
    /// Token TTL in seconds for the HMAC service; `0` leaves the default.
    pub expiration_seconds: u64,
}

/// Bearer-middleware settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BearerProperties {
    /// Header to read the token from; empty means `Authorization`.
    pub header_name: String,
    /// Whether a missing token falls through as the anonymous principal.
    pub allow_anonymous: bool,
}

/// Builds a [`Verifier`] from [`JwtProperties`], or `None` when neither a
/// `jwk_set_uri` nor a `secret` is configured.
///
/// Precedence: a configured `jwk_set_uri` wins (RS256 resource server); else a
/// configured `secret` builds an HMAC verifier; else `None`.
pub fn verifier_from_config(
    props: &JwtProperties,
) -> Result<Option<Arc<dyn Verifier>>, SecurityError> {
    if !props.jwk_set_uri.trim().is_empty() {
        let mut verifier = JwksVerifier::new(props.jwk_set_uri.clone());
        if !props.issuer_uri.trim().is_empty() {
            verifier = verifier.issuer(props.issuer_uri.clone());
        }
        if !props.audience.trim().is_empty() {
            verifier = verifier.audience(props.audience.clone());
        }
        return Ok(Some(Arc::new(verifier)));
    }
    if !props.secret.trim().is_empty() {
        let mut service = JwtService::new(props.secret.as_bytes());
        if !props.algorithm.trim().is_empty() {
            service = service.algorithm(parse_algorithm(&props.algorithm)?)?;
        }
        if props.expiration_seconds > 0 {
            service = service.expiration_seconds(props.expiration_seconds);
        }
        return Ok(Some(Arc::new(service)));
    }
    Ok(None)
}

/// Builds a ready-to-mount [`BearerLayer`] from [`SecurityProperties`], or
/// `None` when no verifier is configured (see [`verifier_from_config`]).
pub fn bearer_layer_from_config(
    props: &SecurityProperties,
) -> Result<Option<BearerLayer>, SecurityError> {
    let Some(verifier) = verifier_from_config(&props.jwt)? else {
        return Ok(None);
    };
    // BearerConfig's verifier is a `pub Arc<dyn Verifier>`, so the erased
    // verifier drops straight in without re-wrapping.
    let config = BearerConfig {
        verifier,
        allow_anonymous: props.bearer.allow_anonymous,
        header_name: props.bearer.header_name.clone(),
        unauthorized: None,
    };
    Ok(Some(BearerLayer::new(config)))
}

/// Maps an HMAC algorithm name onto an [`Algorithm`]. Case-insensitive; only
/// the symmetric `HS*` family is valid here.
fn parse_algorithm(name: &str) -> Result<Algorithm, SecurityError> {
    match name.trim().to_ascii_uppercase().as_str() {
        "HS256" => Ok(Algorithm::HS256),
        "HS384" => Ok(Algorithm::HS384),
        "HS512" => Ok(Algorithm::HS512),
        other => Err(SecurityError::verification(format!(
            "unsupported HMAC algorithm {other:?}; use HS256, HS384, or HS512"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_jwt_settings_yield_no_verifier_or_layer() {
        let props = SecurityProperties::default();
        assert!(verifier_from_config(&props.jwt).unwrap().is_none());
        assert!(bearer_layer_from_config(&props).unwrap().is_none());
    }

    #[test]
    fn jwk_set_uri_builds_a_resource_server_verifier_and_layer() {
        let props = SecurityProperties {
            jwt: JwtProperties {
                jwk_set_uri: "https://idp.example.com/jwks.json".into(),
                issuer_uri: "https://idp.example.com/".into(),
                audience: "orders".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(verifier_from_config(&props.jwt).unwrap().is_some());
        assert!(bearer_layer_from_config(&props).unwrap().is_some());
    }

    #[tokio::test]
    async fn hmac_secret_builds_a_working_verifier() {
        let props = JwtProperties {
            secret: "super-secret-value".into(),
            ..Default::default()
        };
        let verifier = verifier_from_config(&props).unwrap().expect("verifier");
        // A token minted with the same secret verifies through the configured
        // verifier (proves the HMAC path is wired end-to-end).
        let signer = JwtService::new(props.secret.as_bytes());
        let token = signer
            .encode(serde_json::json!({ "sub": "alice" }))
            .expect("encode");
        let auth = verifier.verify(&token).await.expect("verify");
        assert_eq!(auth.principal, "alice");
    }

    #[test]
    fn jwk_set_uri_takes_precedence_over_secret() {
        // Both set → JWKS wins; the secret is ignored.
        let props = JwtProperties {
            jwk_set_uri: "https://idp.example.com/jwks.json".into(),
            secret: "ignored".into(),
            ..Default::default()
        };
        assert!(verifier_from_config(&props).unwrap().is_some());
    }

    #[test]
    fn bad_algorithm_name_errors() {
        let props = JwtProperties {
            secret: "s".into(),
            algorithm: "HS999".into(),
            ..Default::default()
        };
        assert!(verifier_from_config(&props).is_err());
    }

    #[test]
    fn properties_bind_from_a_config_document() {
        let props: SecurityProperties = serde_json::from_value(serde_json::json!({
            "jwt": { "jwk_set_uri": "https://x/jwks", "audience": "api" },
            "bearer": { "allow_anonymous": true }
        }))
        .expect("bind");
        assert_eq!(props.jwt.jwk_set_uri, "https://x/jwks");
        assert!(props.bearer.allow_anonymous);
    }
}
