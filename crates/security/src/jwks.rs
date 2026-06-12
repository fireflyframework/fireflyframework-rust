//! JWKS-based OAuth2 resource-server verifier (pyfly:
//! `pyfly.security.oauth2.resource_server.JWKSTokenValidator`).
//!
//! [`JwksVerifier`] validates RS256-signed JWTs against a remote JWKS
//! endpoint: it fetches the provider's public keys once, caches them by
//! `kid`, and verifies signature, `exp` (required), and optional
//! `iss`/`aud` claims. It implements the crate's [`Verifier`] port, so
//! it drops straight into [`BearerLayer`](crate::BearerLayer).

use std::collections::HashMap;

use async_trait::async_trait;
use jsonwebtoken::{decode, decode_header, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::RwLock;

use crate::authentication::{Authentication, SecurityError, Verifier};

pub use jsonwebtoken::Algorithm;

/// One key of a JWKS document (only the RSA members the verifier
/// needs).
#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

/// A JWKS document: `{"keys": [...]}`.
#[derive(Debug, Deserialize)]
struct JwksDocument {
    #[serde(default)]
    keys: Vec<Jwk>,
}

/// Validates RS256-signed JWTs using a remote JWKS endpoint.
///
/// Fetches public keys from the JWKS URI and caches them by `kid`;
/// extracts claims to build an [`Authentication`]:
///
/// - `sub` → [`Authentication::principal`]
/// - `preferred_username` | `name` | `sub` → [`Authentication::username`]
/// - `roles` or Keycloak's `realm_access.roles` → [`Authentication::roles`]
/// - `permissions` or space-separated `scope` → [`Authentication::authorities`]
///
/// ```rust,no_run
/// use firefly_security::{BearerConfig, BearerLayer, JwksVerifier};
///
/// let verifier = JwksVerifier::new("https://auth.example.com/.well-known/jwks.json")
///     .issuer("https://auth.example.com")
///     .audience("my-api");
/// let layer = BearerLayer::new(BearerConfig::new(verifier));
/// ```
pub struct JwksVerifier {
    jwks_uri: String,
    issuer: Option<String>,
    audience: Option<String>,
    algorithms: Vec<Algorithm>,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
}

impl std::fmt::Debug for JwksVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksVerifier")
            .field("jwks_uri", &self.jwks_uri)
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("algorithms", &self.algorithms)
            .finish_non_exhaustive()
    }
}

impl JwksVerifier {
    /// Builds a verifier for `jwks_uri` with pyfly defaults: no issuer
    /// or audience validation, `RS256` only, `exp` required.
    pub fn new(jwks_uri: impl Into<String>) -> Self {
        Self {
            jwks_uri: jwks_uri.into(),
            issuer: None,
            audience: None,
            algorithms: vec![Algorithm::RS256],
            http: reqwest::Client::new(),
            keys: RwLock::new(HashMap::new()),
        }
    }

    /// Requires the `iss` claim to equal `issuer`.
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Requires the `aud` claim to contain `audience`.
    pub fn audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    /// Overrides the allowed signing algorithms (default `[RS256]`).
    pub fn algorithms(mut self, algorithms: Vec<Algorithm>) -> Self {
        self.algorithms = algorithms;
        self
    }

    /// Validates `token` and returns the decoded payload — the Rust
    /// analog of pyfly's `JWKSTokenValidator.validate`. Errors carry
    /// the pyfly message shape `Token validation failed: <detail>`.
    pub async fn validate(&self, token: &str) -> Result<Map<String, Value>, SecurityError> {
        let header = decode_header(token).map_err(validation_failed)?;
        if !self.algorithms.contains(&header.alg) {
            return Err(SecurityError::verification(format!(
                "Token validation failed: algorithm {:?} not allowed",
                header.alg
            )));
        }
        let kid = header.kid.ok_or_else(|| {
            SecurityError::verification("Token validation failed: token has no kid header")
        })?;
        let key = self.signing_key(&kid).await?;

        let mut validation = Validation::new(header.alg);
        validation.leeway = 0;
        validation.set_required_spec_claims(&["exp"]);
        if let Some(iss) = &self.issuer {
            validation.set_issuer(&[iss]);
        }
        match &self.audience {
            Some(aud) => validation.set_audience(&[aud]),
            None => validation.validate_aud = false,
        }

        let data =
            decode::<Map<String, Value>>(token, &key, &validation).map_err(validation_failed)?;
        Ok(data.claims)
    }

    /// Returns the cached decoding key for `kid`, fetching (and
    /// re-caching) the JWKS document on a miss.
    async fn signing_key(&self, kid: &str) -> Result<DecodingKey, SecurityError> {
        if let Some(key) = self.keys.read().await.get(kid) {
            return Ok(key.clone());
        }
        let fetched = self.fetch_jwks().await?;
        let mut cache = self.keys.write().await;
        *cache = fetched;
        cache.get(kid).cloned().ok_or_else(|| {
            SecurityError::verification(format!(
                "Token validation failed: no signing key found for kid {kid:?}"
            ))
        })
    }

    /// Fetches and parses the JWKS document into per-kid decoding keys.
    async fn fetch_jwks(&self) -> Result<HashMap<String, DecodingKey>, SecurityError> {
        let doc: JwksDocument = self
            .http
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| {
                SecurityError::verification(format!("Token validation failed: JWKS fetch: {e}"))
            })?
            .error_for_status()
            .map_err(|e| {
                SecurityError::verification(format!("Token validation failed: JWKS fetch: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                SecurityError::verification(format!("Token validation failed: JWKS parse: {e}"))
            })?;

        let mut keys = HashMap::new();
        for jwk in doc.keys {
            let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e) else {
                continue;
            };
            if jwk.kty != "RSA" {
                continue;
            }
            if let Ok(key) = DecodingKey::from_rsa_components(&n, &e) {
                keys.insert(kid, key);
            }
        }
        Ok(keys)
    }
}

/// Maps a verified JWT payload to an [`Authentication`] — pyfly's
/// `JWKSTokenValidator.to_security_context` claim mapping, reusable for
/// OIDC id-token claims.
pub fn claims_to_authentication(claims: &Map<String, Value>) -> Authentication {
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

    // Roles — support both the flat "roles" claim and Keycloak's
    // nested realm_access.roles structure.
    let mut roles = string_vec(claims.get("roles"));
    if roles.is_empty() {
        roles = string_vec(
            claims
                .get("realm_access")
                .and_then(Value::as_object)
                .and_then(|ra| ra.get("roles")),
        );
    }

    // Authorities — support "permissions" or space-separated "scope".
    let mut authorities = string_vec(claims.get("permissions"));
    if authorities.is_empty() {
        if let Some(scope) = claims.get("scope").and_then(Value::as_str) {
            authorities = scope.split_whitespace().map(str::to_owned).collect();
        }
    }

    Authentication {
        principal,
        username,
        roles,
        authorities,
        claims: claims.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

/// Collects the string items of a JSON array claim.
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

/// Wraps a jsonwebtoken error in the pyfly message shape.
fn validation_failed(err: jsonwebtoken::errors::Error) -> SecurityError {
    SecurityError::verification(format!("Token validation failed: {err}"))
}

#[async_trait]
impl Verifier for JwksVerifier {
    async fn verify(&self, token: &str) -> Result<Authentication, SecurityError> {
        let claims = self.validate(token).await?;
        Ok(claims_to_authentication(&claims))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claims(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    // Ported from pyfly: test_to_security_context_basic
    #[test]
    fn claims_map_basic() {
        let auth = claims_to_authentication(&claims(json!({
            "sub": "user-42",
            "roles": ["admin", "editor"],
            "permissions": ["read", "write"],
        })));
        assert_eq!(auth.principal, "user-42");
        assert_eq!(auth.roles, vec!["admin", "editor"]);
        assert_eq!(auth.authorities, vec!["read", "write"]);
    }

    // Ported from pyfly: test_to_security_context_keycloak_roles
    #[test]
    fn claims_map_keycloak_realm_access() {
        let auth = claims_to_authentication(&claims(json!({
            "sub": "kc-user",
            "realm_access": {"roles": ["realm-admin", "realm-viewer"]},
        })));
        assert_eq!(auth.principal, "kc-user");
        assert_eq!(auth.roles, vec!["realm-admin", "realm-viewer"]);
    }

    // Ported from pyfly: test_to_security_context_scope_as_permissions
    #[test]
    fn claims_map_scope_split_into_authorities() {
        let auth = claims_to_authentication(&claims(json!({
            "sub": "scope-user",
            "scope": "read write delete",
        })));
        assert_eq!(auth.authorities, vec!["read", "write", "delete"]);
    }

    #[test]
    fn claims_map_username_prefers_preferred_username() {
        let auth = claims_to_authentication(&claims(json!({
            "sub": "u1",
            "preferred_username": "alice",
            "name": "Alice Liddell",
        })));
        assert_eq!(auth.username, "alice");

        let auth = claims_to_authentication(&claims(json!({"sub": "u1", "name": "Alice"})));
        assert_eq!(auth.username, "Alice");

        let auth = claims_to_authentication(&claims(json!({"sub": "u1"})));
        assert_eq!(auth.username, "u1");
    }

    #[test]
    fn claims_map_keeps_raw_claims() {
        let auth = claims_to_authentication(&claims(json!({"sub": "u1", "tenant": "t9"})));
        assert_eq!(auth.claims["tenant"], json!("t9"));
    }
}
