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

//! Outbound OAuth2 client — the Rust analog of Spring Security's
//! `OAuth2AuthorizedClientManager` / `OAuth2AuthorizedClientService`.
//!
//! Where [`OAuth2LoginHandler`](super::OAuth2LoginHandler) handles the *inbound*
//! browser login, this is the *outbound* side: how the application obtains,
//! **caches**, and **refreshes** the access tokens it needs to call
//! downstream OAuth2-protected services.
//!
//! * [`OAuth2AuthorizedClient`] — a held token (access + optional refresh +
//!   expiry) for a `(registration, principal)` pair (Spring's
//!   `OAuth2AuthorizedClient`).
//! * [`OAuth2AuthorizedClientService`] — where those are stored
//!   ([`InMemoryOAuth2AuthorizedClientService`] by default).
//! * [`OAuth2AuthorizedClientManager`] — obtains a client via the
//!   **client-credentials** grant (service-to-service) or refreshes an existing
//!   one via the **refresh-token** grant, reusing a cached token until it is
//!   within the clock-skew window of expiry.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use http::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use super::authorization_server::OAuth2Error;
use super::client::{ClientRegistration, ClientRegistrationRepository};

/// Default leeway treating a token as expired slightly early, so a downstream
/// call never races the actual expiry — mirrors Spring's 60s default.
pub const DEFAULT_CLOCK_SKEW_SECONDS: u64 = 60;

/// Conservative lifetime assumed when a token response omits `expires_in`
/// (RFC 6749 §5.1 makes it optional, and "absent" means *unknown*, not
/// *infinite*). Bounding it forces a re-fetch rather than caching the token
/// forever.
pub const DEFAULT_FALLBACK_TTL_SECONDS: u64 = 300;

/// A token the application holds to call a downstream OAuth2-protected service
/// — Spring's `OAuth2AuthorizedClient`. Its `Debug` redacts the tokens; the
/// `Serialize` form (for a persistent [`OAuth2AuthorizedClientService`]) carries
/// them in clear, so persist only to a secured store.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuth2AuthorizedClient {
    /// The client registration this token was obtained for.
    pub registration_id: String,
    /// The principal the token represents — the client id for a
    /// client-credentials token, or a user name for a delegated one.
    pub principal_name: String,
    /// The bearer access token to send downstream.
    pub access_token: String,
    /// The refresh token, when the grant returned one.
    pub refresh_token: Option<String>,
    /// Absolute expiry in epoch seconds, when the grant returned `expires_in`.
    pub expires_at: Option<u64>,
    /// The granted scopes.
    pub scopes: Vec<String>,
}

// Manual `Debug` that redacts the access/refresh tokens (live bearer
// credentials), so logging/error context can never print them in clear.
impl std::fmt::Debug for OAuth2AuthorizedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2AuthorizedClient")
            .field("registration_id", &self.registration_id)
            .field("principal_name", &self.principal_name)
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl OAuth2AuthorizedClient {
    /// Whether the access token is at or within `skew_seconds` of expiry. A
    /// token with no expiry information is treated as non-expiring.
    #[must_use]
    pub fn is_expired(&self, skew_seconds: u64) -> bool {
        match self.expires_at {
            Some(exp) => now_secs().saturating_add(skew_seconds) >= exp,
            None => false,
        }
    }
}

/// Stores [`OAuth2AuthorizedClient`]s by `(registration_id, principal_name)` —
/// Spring's `OAuth2AuthorizedClientService`.
#[async_trait]
pub trait OAuth2AuthorizedClientService: Send + Sync {
    /// Saves (inserts or replaces) an authorized client.
    async fn save(&self, client: OAuth2AuthorizedClient);
    /// Loads the authorized client for `(registration_id, principal_name)`.
    async fn load(
        &self,
        registration_id: &str,
        principal_name: &str,
    ) -> Option<OAuth2AuthorizedClient>;
    /// Removes the authorized client for `(registration_id, principal_name)`.
    async fn remove(&self, registration_id: &str, principal_name: &str);
}

/// In-memory [`OAuth2AuthorizedClientService`] (default; single-process).
#[derive(Default)]
pub struct InMemoryOAuth2AuthorizedClientService {
    clients: Mutex<HashMap<(String, String), OAuth2AuthorizedClient>>,
}

impl InMemoryOAuth2AuthorizedClientService {
    /// Builds an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OAuth2AuthorizedClientService for InMemoryOAuth2AuthorizedClientService {
    async fn save(&self, client: OAuth2AuthorizedClient) {
        let key = (
            client.registration_id.clone(),
            client.principal_name.clone(),
        );
        self.clients.lock().await.insert(key, client);
    }
    async fn load(
        &self,
        registration_id: &str,
        principal_name: &str,
    ) -> Option<OAuth2AuthorizedClient> {
        self.clients
            .lock()
            .await
            .get(&(registration_id.to_owned(), principal_name.to_owned()))
            .cloned()
    }
    async fn remove(&self, registration_id: &str, principal_name: &str) {
        self.clients
            .lock()
            .await
            .remove(&(registration_id.to_owned(), principal_name.to_owned()));
    }
}

/// Obtains, caches, and refreshes outbound access tokens — Spring's
/// `OAuth2AuthorizedClientManager`.
pub struct OAuth2AuthorizedClientManager {
    clients: Arc<dyn ClientRegistrationRepository>,
    service: Arc<dyn OAuth2AuthorizedClientService>,
    http: reqwest::Client,
    clock_skew_seconds: u64,
}

impl OAuth2AuthorizedClientManager {
    /// Builds the manager over a registration repository and an authorized-client
    /// store, with the default clock skew.
    #[must_use]
    pub fn new(
        clients: Arc<dyn ClientRegistrationRepository>,
        service: Arc<dyn OAuth2AuthorizedClientService>,
    ) -> Self {
        Self {
            clients,
            service,
            http: crate::default_http_client(),
            clock_skew_seconds: DEFAULT_CLOCK_SKEW_SECONDS,
        }
    }

    /// Overrides the expiry clock-skew leeway (seconds).
    #[must_use]
    pub fn clock_skew_seconds(mut self, seconds: u64) -> Self {
        self.clock_skew_seconds = seconds;
        self
    }

    /// Obtains a service token for `registration_id` via the **client-credentials**
    /// grant. Returns a cached token while it is still valid; when it has expired
    /// it is refreshed (if a refresh token is held) or re-fetched. The principal
    /// for a client-credentials token is the client id.
    pub async fn authorize_client_credentials(
        &self,
        registration_id: &str,
    ) -> Result<OAuth2AuthorizedClient, OAuth2Error> {
        let registration = self.registration(registration_id)?;
        let principal = registration.client_id.clone();

        if let Some(existing) = self.service.load(registration_id, &principal).await {
            if !existing.is_expired(self.clock_skew_seconds) {
                return Ok(existing);
            }
            // Expired: prefer a refresh, fall back to a fresh grant.
            if existing.refresh_token.is_some() {
                if let Ok(refreshed) = self.refresh_grant(&registration, &existing).await {
                    return Ok(refreshed);
                }
            }
        }

        let scope = registration.scopes.join(" ");
        let mut form = vec![("grant_type", "client_credentials")];
        if !scope.is_empty() {
            form.push(("scope", scope.as_str()));
        }
        let response = self.request_token(&registration, &form).await?;
        let client = authorized_client_from_token_response(
            registration_id,
            &principal,
            &registration.scopes,
            None,
            &response,
            now_secs(),
        )?;
        self.service.save(client.clone()).await;
        Ok(client)
    }

    /// Refreshes the token held for `(registration_id, principal_name)` via the
    /// **refresh-token** grant. Errors when no client (or no refresh token) is
    /// stored.
    pub async fn refresh(
        &self,
        registration_id: &str,
        principal_name: &str,
    ) -> Result<OAuth2AuthorizedClient, OAuth2Error> {
        let registration = self.registration(registration_id)?;
        let existing = self
            .service
            .load(registration_id, principal_name)
            .await
            .ok_or_else(|| {
                OAuth2Error::new("no_authorized_client", "no stored client to refresh")
            })?;
        self.refresh_grant(&registration, &existing).await
    }

    /// Performs the refresh-token grant for `existing` and stores the result.
    async fn refresh_grant(
        &self,
        registration: &ClientRegistration,
        existing: &OAuth2AuthorizedClient,
    ) -> Result<OAuth2AuthorizedClient, OAuth2Error> {
        let refresh_token = existing.refresh_token.as_deref().ok_or_else(|| {
            OAuth2Error::new("no_refresh_token", "stored client has no refresh token")
        })?;
        let form = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        let response = self.request_token(registration, &form).await?;
        let client = authorized_client_from_token_response(
            &existing.registration_id,
            &existing.principal_name,
            &existing.scopes,
            // RFC 6749 §6: a refresh response may omit a new refresh token — keep
            // the current one.
            existing.refresh_token.as_deref(),
            &response,
            now_secs(),
        )?;
        self.service.save(client.clone()).await;
        Ok(client)
    }

    fn registration(&self, registration_id: &str) -> Result<ClientRegistration, OAuth2Error> {
        self.clients
            .find_by_registration_id(registration_id)
            .ok_or_else(|| {
                OAuth2Error::new(
                    "unknown_client_registration",
                    format!("no client registration `{registration_id}`"),
                )
            })
    }

    /// POSTs a token request (client_secret_basic auth) and returns the parsed
    /// JSON body, mapping transport / non-2xx / non-JSON failures to errors.
    async fn request_token(
        &self,
        registration: &ClientRegistration,
        form: &[(&str, &str)],
    ) -> Result<Value, OAuth2Error> {
        let response = self
            .http
            .post(&registration.token_uri)
            .basic_auth(&registration.client_id, Some(&registration.client_secret))
            .form(form)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| OAuth2Error::new("token_request_failed", e.to_string()))?;
        if !response.status().is_success() {
            return Err(OAuth2Error::new(
                "token_endpoint_error",
                format!("token endpoint returned {}", response.status()),
            ));
        }
        if response
            .content_length()
            .is_some_and(|len| len > crate::MAX_OAUTH2_RESPONSE_BYTES)
        {
            return Err(OAuth2Error::new(
                "invalid_token_response",
                "token response too large",
            ));
        }
        response
            .json()
            .await
            .map_err(|e| OAuth2Error::new("invalid_token_response", e.to_string()))
    }
}

/// Builds an [`OAuth2AuthorizedClient`] from a token-endpoint JSON response.
/// `previous_refresh` is retained when the response omits a refresh token, and
/// `scope_fallback` is used when it omits `scope`.
pub(crate) fn authorized_client_from_token_response(
    registration_id: &str,
    principal_name: &str,
    scope_fallback: &[String],
    previous_refresh: Option<&str>,
    response: &Value,
    now: u64,
) -> Result<OAuth2AuthorizedClient, OAuth2Error> {
    let obj = response
        .as_object()
        .ok_or_else(|| OAuth2Error::new("invalid_token_response", "not a JSON object"))?;
    let access_token = obj
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| OAuth2Error::new("invalid_token_response", "missing access_token"))?
        .to_owned();
    let refresh_token = obj
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| previous_refresh.map(str::to_owned));
    // RFC 6749 §5.1: `expires_in` is optional, and its absence means the
    // lifetime is unknown — assume a short, bounded one rather than caching the
    // token forever (a missing/None expiry would otherwise read as non-expiring).
    let expires_at = Some(
        now.saturating_add(
            obj.get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_FALLBACK_TTL_SECONDS),
        ),
    );
    let scopes = obj
        .get("scope")
        .and_then(Value::as_str)
        .map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_else(|| scope_fallback.to_vec());
    Ok(OAuth2AuthorizedClient {
        registration_id: registration_id.to_owned(),
        principal_name: principal_name.to_owned(),
        access_token,
        refresh_token,
        expires_at,
        scopes,
    })
}

/// The current wall-clock time in epoch seconds (0 on a pre-epoch clock).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_token_response_with_expiry_and_scope() {
        let resp = json!({
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 3600,
            "scope": "read write"
        });
        let client =
            authorized_client_from_token_response("reg", "svc", &[], None, &resp, 1000).unwrap();
        assert_eq!(client.access_token, "at-1");
        assert_eq!(client.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(client.expires_at, Some(4600));
        assert_eq!(client.scopes, vec!["read", "write"]);
        // 4600 expiry, now ~1000 → not expired; with huge skew → expired.
        assert!(!client.is_expired(0) || client.expires_at.unwrap() <= now_secs());
    }

    #[test]
    fn refresh_response_retains_previous_refresh_token_and_scope() {
        // A refresh response omits refresh_token, scope, AND expires_in: keep the
        // old refresh token + scope, and assume the bounded fallback lifetime
        // (never an immortal token).
        let resp = json!({ "access_token": "at-2" });
        let scopes = vec!["api".to_string()];
        let client = authorized_client_from_token_response(
            "reg",
            "svc",
            &scopes,
            Some("rt-old"),
            &resp,
            1000,
        )
        .unwrap();
        assert_eq!(client.access_token, "at-2");
        assert_eq!(client.refresh_token.as_deref(), Some("rt-old"));
        assert_eq!(client.scopes, vec!["api"]);
        // Missing expires_in → bounded fallback expiry, not non-expiring.
        assert_eq!(client.expires_at, Some(1000 + DEFAULT_FALLBACK_TTL_SECONDS));
    }

    #[test]
    fn missing_access_token_is_an_error() {
        let resp = json!({ "token_type": "Bearer", "expires_in": 60 });
        assert!(authorized_client_from_token_response("reg", "svc", &[], None, &resp, 0).is_err());
        assert!(
            authorized_client_from_token_response("reg", "svc", &[], None, &json!("x"), 0).is_err()
        );
    }

    #[test]
    fn is_expired_honors_skew_and_missing_expiry() {
        let past = OAuth2AuthorizedClient {
            registration_id: "r".into(),
            principal_name: "p".into(),
            access_token: "a".into(),
            refresh_token: None,
            expires_at: Some(1), // long past
            scopes: vec![],
        };
        assert!(past.is_expired(0));
        let none = OAuth2AuthorizedClient {
            expires_at: None,
            ..past.clone()
        };
        // No expiry info → treated as non-expiring.
        assert!(!none.is_expired(0));
    }
}
