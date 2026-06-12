//! OAuth2 authorization server — token endpoint with JWT issuance
//! (pyfly: `pyfly.security.oauth2.authorization_server`).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::client::ClientRegistrationRepository;
use super::token_store::TokenStore;
use crate::csrf::{constant_time_eq, random_urlsafe};

/// The OAuth2 error family — the Rust analog of pyfly's
/// `SecurityException(message, code=...)`, carrying the RFC 6749 error
/// code (`INVALID_CLIENT`, `INVALID_GRANT`, ...) alongside the
/// human-readable message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuth2Error {
    /// Machine-readable error code (pyfly `SecurityException.code`).
    pub code: String,
    /// Human-readable detail (pyfly `SecurityException` message).
    pub message: String,
}

impl OAuth2Error {
    /// Builds an error from a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A token-endpoint request (RFC 6749 §4.4 / §6 parameters).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenRequest {
    /// `"client_credentials"` or `"refresh_token"`.
    pub grant_type: String,
    /// The client's id.
    pub client_id: String,
    /// The client's secret.
    pub client_secret: String,
    /// Space-separated scopes (client_credentials grant); empty uses
    /// the registration's default scopes.
    pub scope: String,
    /// The refresh token (refresh_token grant).
    pub refresh_token: Option<String>,
}

/// A token-endpoint success response (RFC 6749 §5.1 wire shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The signed JWT access token.
    pub access_token: String,
    /// Always `"Bearer"`.
    pub token_type: String,
    /// Access-token lifetime in seconds.
    pub expires_in: u64,
    /// The (rotated) refresh token.
    pub refresh_token: String,
    /// Space-separated granted scopes.
    pub scope: String,
}

/// OAuth2 authorization server — issues HS256-signed JWT access
/// tokens. Supported grant types:
///
/// - `client_credentials` — machine-to-machine authentication
/// - `refresh_token` — exchange a refresh token for new tokens
///   (with rotation)
///
/// ```rust
/// use std::sync::Arc;
/// use firefly_security::oauth2::{
///     AuthorizationServer, ClientRegistration, InMemoryClientRegistrationRepository,
///     InMemoryTokenStore,
/// };
///
/// let repo = InMemoryClientRegistrationRepository::new([ClientRegistration::new(
///     "m2m", "m2m",
/// )
/// .client_secret("s3cret")
/// .authorization_grant_type("client_credentials")]);
/// let server = AuthorizationServer::new(
///     "signing-secret",
///     Arc::new(repo),
///     Arc::new(InMemoryTokenStore::new()),
/// )
/// .issuer("https://auth.example.com");
/// ```
pub struct AuthorizationServer {
    secret: String,
    clients: Arc<dyn ClientRegistrationRepository>,
    tokens: Arc<dyn TokenStore>,
    access_token_ttl: u64,
    refresh_token_ttl: u64,
    issuer: Option<String>,
}

impl std::fmt::Debug for AuthorizationServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationServer")
            .field("access_token_ttl", &self.access_token_ttl)
            .field("refresh_token_ttl", &self.refresh_token_ttl)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

impl AuthorizationServer {
    /// Builds a server with pyfly defaults: 1 h access tokens, 24 h
    /// refresh tokens, no `iss` claim.
    pub fn new(
        secret: impl Into<String>,
        clients: Arc<dyn ClientRegistrationRepository>,
        tokens: Arc<dyn TokenStore>,
    ) -> Self {
        Self {
            secret: secret.into(),
            clients,
            tokens,
            access_token_ttl: 3600,
            refresh_token_ttl: 86400,
            issuer: None,
        }
    }

    /// Sets the access-token lifetime in seconds (default 3600).
    pub fn access_token_ttl(mut self, seconds: u64) -> Self {
        self.access_token_ttl = seconds;
        self
    }

    /// Sets the refresh-token lifetime in seconds (default 86400).
    pub fn refresh_token_ttl(mut self, seconds: u64) -> Self {
        self.refresh_token_ttl = seconds;
        self
    }

    /// Sets the `iss` claim stamped on issued access tokens.
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    /// Issues tokens for `request` — the pyfly
    /// `AuthorizationServer.token` flow: constant-time client
    /// authentication, then per-grant handling. Error codes match
    /// pyfly exactly (`INVALID_CLIENT`, `UNAUTHORIZED_CLIENT`,
    /// `INVALID_REQUEST`, `INVALID_GRANT`, `UNSUPPORTED_GRANT_TYPE`).
    pub async fn token(&self, request: &TokenRequest) -> Result<TokenResponse, OAuth2Error> {
        // Authenticate client (constant-time secret comparison to
        // avoid a timing side-channel that could leak the secret).
        let registration = self.clients.find_by_registration_id(&request.client_id);
        let registration = match registration {
            Some(r)
                if constant_time_eq(
                    r.client_secret.as_bytes(),
                    request.client_secret.as_bytes(),
                ) =>
            {
                r
            }
            _ => {
                return Err(OAuth2Error::new(
                    "INVALID_CLIENT",
                    "Invalid client credentials",
                ))
            }
        };

        match request.grant_type.as_str() {
            "client_credentials" => {
                // The client must be registered for the
                // client_credentials grant — prevents grant-type
                // confusion (a client registered only for
                // authorization_code must not use it).
                if registration.authorization_grant_type != "client_credentials" {
                    return Err(OAuth2Error::new(
                        "UNAUTHORIZED_CLIENT",
                        format!(
                            "Client '{}' is not authorized for grant type 'client_credentials'",
                            request.client_id
                        ),
                    ));
                }
                let scopes = if request.scope.is_empty() {
                    registration.scopes.join(" ")
                } else {
                    request
                        .scope
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                self.issue(&registration.client_id, &scopes).await
            }
            "refresh_token" => {
                let Some(refresh_token) = request.refresh_token.as_deref() else {
                    return Err(OAuth2Error::new(
                        "INVALID_REQUEST",
                        "Refresh token required",
                    ));
                };
                let token_data = self.tokens.find(refresh_token).await?;
                let Some(token_data) = token_data else {
                    return Err(OAuth2Error::new("INVALID_GRANT", "Invalid refresh token"));
                };
                // Verify client matches.
                if token_data.get("client_id").and_then(|v| v.as_str())
                    != Some(registration.client_id.as_str())
                {
                    return Err(OAuth2Error::new(
                        "INVALID_GRANT",
                        "Refresh token client mismatch",
                    ));
                }
                // Check expiration.
                let exp = token_data.get("exp").and_then(|v| v.as_u64()).unwrap_or(0);
                if exp < unix_now() {
                    self.tokens.revoke(refresh_token).await?;
                    return Err(OAuth2Error::new("INVALID_GRANT", "Refresh token expired"));
                }
                // Revoke old refresh token (rotation), then issue new.
                self.tokens.revoke(refresh_token).await?;
                let scope = token_data
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.issue(&registration.client_id, &scope).await
            }
            other => Err(OAuth2Error::new(
                "UNSUPPORTED_GRANT_TYPE",
                format!("Unsupported grant type: {other}"),
            )),
        }
    }

    /// Revokes a refresh token.
    pub async fn revoke(&self, token_id: &str) -> Result<(), OAuth2Error> {
        self.tokens.revoke(token_id).await
    }

    /// Mints an access token + rotated refresh token for `client_id`
    /// with `scope`.
    async fn issue(&self, client_id: &str, scope: &str) -> Result<TokenResponse, OAuth2Error> {
        let now = unix_now();
        let mut access_payload = json!({
            "sub": client_id,
            "scope": scope,
            "iat": now,
            "exp": now + self.access_token_ttl,
        });
        if let Some(iss) = &self.issuer {
            access_payload["iss"] = json!(iss);
        }
        let access_token = encode(
            &Header::default(), // HS256
            &access_payload,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| OAuth2Error::new("SERVER_ERROR", format!("token signing: {e}")))?;

        let refresh_token = random_urlsafe(32);
        self.tokens
            .store(
                &refresh_token,
                json!({
                    "client_id": client_id,
                    "scope": scope,
                    "exp": now + self.refresh_token_ttl,
                }),
            )
            .await?;

        Ok(TokenResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: self.access_token_ttl,
            refresh_token,
            scope: scope.to_string(),
        })
    }
}

/// Seconds since the Unix epoch.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_secs()
}
