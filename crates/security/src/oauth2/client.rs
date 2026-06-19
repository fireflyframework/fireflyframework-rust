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

//! OAuth2 client registration — provider configuration and repository
//! (pyfly: `pyfly.security.oauth2.client`).

use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// OAuth2 client registration configuration.
///
/// Represents the configuration needed to interact with an OAuth2
/// provider as a client application. Build one with
/// [`ClientRegistration::new`] + the fluent setters, or use the
/// [`google`], [`github`], and [`keycloak`] presets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientRegistration {
    /// Unique id this registration is looked up by.
    pub registration_id: String,
    /// The OAuth2 client identifier.
    pub client_id: String,
    /// The OAuth2 client secret (empty for public clients).
    pub client_secret: String,
    /// The grant this client is registered for
    /// (default `"authorization_code"`).
    pub authorization_grant_type: String,
    /// The redirect URI for the authorization-code flow.
    pub redirect_uri: String,
    /// Default scopes requested at authorization time.
    pub scopes: Vec<String>,
    /// The provider's authorization endpoint.
    pub authorization_uri: String,
    /// The provider's token endpoint.
    pub token_uri: String,
    /// The provider's userinfo endpoint.
    pub user_info_uri: String,
    /// The provider's JWKS endpoint (enables OIDC id-token validation).
    pub jwks_uri: String,
    /// The provider's issuer URI (`iss` claim).
    pub issuer_uri: String,
    /// Human-friendly provider name.
    pub provider_name: String,
    /// Enable PKCE (RFC 7636, S256) on the authorization_code flow.
    /// Recommended for public clients (no client_secret); harmless and
    /// more secure for confidential clients too.
    pub use_pkce: bool,
    /// The OIDC RP-initiated-logout `end_session_endpoint`; when set, logout
    /// redirects the browser here to also end the session at the provider.
    pub end_session_endpoint: String,
    /// Where the provider should send the browser back after RP-initiated
    /// logout (`post_logout_redirect_uri`).
    pub post_logout_redirect_uri: String,
}

impl ClientRegistration {
    /// Builds a registration with pyfly defaults: empty URIs/secret,
    /// grant type `"authorization_code"`, PKCE off.
    pub fn new(registration_id: impl Into<String>, client_id: impl Into<String>) -> Self {
        Self {
            registration_id: registration_id.into(),
            client_id: client_id.into(),
            authorization_grant_type: "authorization_code".into(),
            ..Self::default()
        }
    }

    /// Sets the client secret.
    pub fn client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = secret.into();
        self
    }

    /// Sets the registered grant type
    /// (e.g. `"client_credentials"`).
    pub fn authorization_grant_type(mut self, grant_type: impl Into<String>) -> Self {
        self.authorization_grant_type = grant_type.into();
        self
    }

    /// Sets the redirect URI.
    pub fn redirect_uri(mut self, uri: impl Into<String>) -> Self {
        self.redirect_uri = uri.into();
        self
    }

    /// Sets the default scopes.
    pub fn scopes(mut self, scopes: &[&str]) -> Self {
        self.scopes = scopes.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Sets the authorization endpoint.
    pub fn authorization_uri(mut self, uri: impl Into<String>) -> Self {
        self.authorization_uri = uri.into();
        self
    }

    /// Sets the token endpoint.
    pub fn token_uri(mut self, uri: impl Into<String>) -> Self {
        self.token_uri = uri.into();
        self
    }

    /// Sets the userinfo endpoint.
    pub fn user_info_uri(mut self, uri: impl Into<String>) -> Self {
        self.user_info_uri = uri.into();
        self
    }

    /// Sets the JWKS endpoint.
    pub fn jwks_uri(mut self, uri: impl Into<String>) -> Self {
        self.jwks_uri = uri.into();
        self
    }

    /// Sets the issuer URI.
    pub fn issuer_uri(mut self, uri: impl Into<String>) -> Self {
        self.issuer_uri = uri.into();
        self
    }

    /// Sets the provider display name.
    pub fn provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    /// Enables or disables PKCE S256 on the authorization-code flow.
    pub fn use_pkce(mut self, enabled: bool) -> Self {
        self.use_pkce = enabled;
        self
    }

    /// Sets the OIDC `end_session_endpoint` for RP-initiated logout.
    pub fn end_session_endpoint(mut self, uri: impl Into<String>) -> Self {
        self.end_session_endpoint = uri.into();
        self
    }

    /// Sets the `post_logout_redirect_uri` the provider returns to after
    /// RP-initiated logout.
    pub fn post_logout_redirect_uri(mut self, uri: impl Into<String>) -> Self {
        self.post_logout_redirect_uri = uri.into();
        self
    }
}

/// Creates a [`ClientRegistration`] pre-configured for Google OAuth2.
pub fn google(client_id: &str, client_secret: &str, redirect_uri: &str) -> ClientRegistration {
    ClientRegistration::new("google", client_id)
        .client_secret(client_secret)
        .redirect_uri(redirect_uri)
        .scopes(&["openid", "profile", "email"])
        .authorization_uri("https://accounts.google.com/o/oauth2/v2/auth")
        .token_uri("https://oauth2.googleapis.com/token")
        .user_info_uri("https://www.googleapis.com/oauth2/v3/userinfo")
        .jwks_uri("https://www.googleapis.com/oauth2/v3/certs")
        .issuer_uri("https://accounts.google.com")
        .provider_name("Google")
}

/// Creates a [`ClientRegistration`] pre-configured for GitHub OAuth2.
pub fn github(client_id: &str, client_secret: &str, redirect_uri: &str) -> ClientRegistration {
    ClientRegistration::new("github", client_id)
        .client_secret(client_secret)
        .redirect_uri(redirect_uri)
        .scopes(&["read:user", "user:email"])
        .authorization_uri("https://github.com/login/oauth/authorize")
        .token_uri("https://github.com/login/oauth/access_token")
        .user_info_uri("https://api.github.com/user")
        .provider_name("GitHub")
}

/// Creates a [`ClientRegistration`] pre-configured for Keycloak,
/// deriving the OIDC endpoints from the realm `issuer_uri` (e.g.
/// `https://keycloak.example.com/realms/myrealm`).
pub fn keycloak(
    client_id: &str,
    client_secret: &str,
    issuer_uri: &str,
    redirect_uri: &str,
) -> ClientRegistration {
    let base = issuer_uri.trim_end_matches('/');
    ClientRegistration::new("keycloak", client_id)
        .client_secret(client_secret)
        .redirect_uri(redirect_uri)
        .scopes(&["openid", "profile", "email"])
        .authorization_uri(format!("{base}/protocol/openid-connect/auth"))
        .token_uri(format!("{base}/protocol/openid-connect/token"))
        .user_info_uri(format!("{base}/protocol/openid-connect/userinfo"))
        .jwks_uri(format!("{base}/protocol/openid-connect/certs"))
        .issuer_uri(issuer_uri)
        .end_session_endpoint(format!("{base}/protocol/openid-connect/logout"))
        .provider_name("Keycloak")
}

/// Port for retrieving OAuth2 client registrations (pyfly:
/// `ClientRegistrationRepository` protocol).
pub trait ClientRegistrationRepository: Send + Sync {
    /// Returns the registration with the given id, if any.
    fn find_by_registration_id(&self, registration_id: &str) -> Option<ClientRegistration>;
}

/// In-memory client registration repository, keyed by
/// `registration_id`.
#[derive(Debug, Default)]
pub struct InMemoryClientRegistrationRepository {
    registrations: RwLock<HashMap<String, ClientRegistration>>,
}

impl InMemoryClientRegistrationRepository {
    /// Builds a repository from any number of registrations.
    pub fn new(registrations: impl IntoIterator<Item = ClientRegistration>) -> Self {
        Self {
            registrations: RwLock::new(
                registrations
                    .into_iter()
                    .map(|r| (r.registration_id.clone(), r))
                    .collect(),
            ),
        }
    }

    /// Adds (or replaces) a registration after construction.
    pub fn add(&self, registration: ClientRegistration) {
        self.registrations
            .write()
            .expect("registration lock poisoned")
            .insert(registration.registration_id.clone(), registration);
    }

    /// Returns all stored registrations.
    pub fn registrations(&self) -> Vec<ClientRegistration> {
        self.registrations
            .read()
            .expect("registration lock poisoned")
            .values()
            .cloned()
            .collect()
    }
}

impl ClientRegistrationRepository for InMemoryClientRegistrationRepository {
    fn find_by_registration_id(&self, registration_id: &str) -> Option<ClientRegistration> {
        self.registrations
            .read()
            .expect("registration lock poisoned")
            .get(registration_id)
            .cloned()
    }
}
