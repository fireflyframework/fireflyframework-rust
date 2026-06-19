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

//! The **authentication manager spine** — the Rust analog of Spring Security's
//! `AuthenticationManager` / `ProviderManager` / `AuthenticationProvider`.
//!
//! An [`AuthenticationRequest`] is an *unauthenticated* set of credentials
//! (Spring's pre-authentication `Authentication` token); an
//! [`AuthenticationProvider`] that [`supports`](AuthenticationProvider::supports)
//! that request kind turns it into an authenticated [`Authentication`]. The
//! [`ProviderManager`] is the default [`AuthenticationManager`]: it walks an
//! ordered list of providers and returns the first success.
//!
//! Where Spring uses a polymorphic token hierarchy + `supports(Class)`, the
//! Rust port uses a closed, `#[non_exhaustive]` [`AuthenticationRequest`] enum
//! — idiomatic and exhaustively matchable in-crate, while still extensible as
//! later parity tiers add credential kinds (SAML, OAuth2 code, …).
//!
//! The existing token [`Verifier`]s slot straight in via
//! [`BearerTokenAuthenticationProvider`], so a `ProviderManager` can unify
//! bearer-token and (Tier 1.2) username/password authentication behind one
//! `authenticate` call.

use std::sync::Arc;

use async_trait::async_trait;

use crate::authentication::{Authentication, SecurityError, Verifier};

/// An unauthenticated authentication request — the Rust analog of Spring's
/// pre-authentication `Authentication` token (e.g. an unauthenticated
/// `UsernamePasswordAuthenticationToken`).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum AuthenticationRequest {
    /// Username + password credentials (form login / HTTP Basic).
    UsernamePassword {
        /// The submitted username.
        username: String,
        /// The submitted (plaintext) password.
        password: String,
    },
    /// A bearer token (OAuth2 / resource server).
    BearerToken(String),
}

impl AuthenticationRequest {
    /// Builds a username/password request.
    pub fn username_password(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::UsernamePassword {
            username: username.into(),
            password: password.into(),
        }
    }

    /// Builds a bearer-token request.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::BearerToken(token.into())
    }
}

/// Authenticates an [`AuthenticationRequest`] — Spring's `AuthenticationProvider`.
#[async_trait]
pub trait AuthenticationProvider: Send + Sync {
    /// Whether this provider handles `request`'s credential kind (Spring's
    /// `supports`). The [`ProviderManager`] skips providers that return `false`.
    fn supports(&self, request: &AuthenticationRequest) -> bool;

    /// Attempts to authenticate `request`. `Ok` is the authenticated principal;
    /// `Err` is an authentication failure (Spring throws
    /// `AuthenticationException`).
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<Authentication, SecurityError>;
}

/// Resolves an [`AuthenticationRequest`] to an [`Authentication`] — Spring's
/// `AuthenticationManager`.
#[async_trait]
pub trait AuthenticationManager: Send + Sync {
    /// Authenticates `request`, or returns the failure.
    async fn authenticate(
        &self,
        request: AuthenticationRequest,
    ) -> Result<Authentication, SecurityError>;
}

/// The default [`AuthenticationManager`]: delegates to an ordered list of
/// [`AuthenticationProvider`]s and returns the first success — Spring's
/// `ProviderManager`.
///
/// Semantics: each provider that [`supports`](AuthenticationProvider::supports)
/// the request is tried in order; the first `Ok` wins. If every supporting
/// provider fails, the last error is returned. If *no* provider supports the
/// request, a provider-not-found error is returned (Spring's
/// `ProviderNotFoundException`).
#[derive(Clone, Default)]
pub struct ProviderManager {
    providers: Vec<Arc<dyn AuthenticationProvider>>,
    event_publisher: Option<Arc<dyn AuthenticationEventPublisher>>,
}

impl ProviderManager {
    /// Builds a manager over `providers`, tried in declaration order.
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn AuthenticationProvider>>) -> Self {
        Self {
            providers,
            event_publisher: None,
        }
    }

    /// Appends a provider (builder style).
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn AuthenticationProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Installs an [`AuthenticationEventPublisher`] notified of every
    /// authentication success and failure (Spring's `AuthenticationEventPublisher`).
    #[must_use]
    pub fn with_event_publisher(
        mut self,
        publisher: Arc<dyn AuthenticationEventPublisher>,
    ) -> Self {
        self.event_publisher = Some(publisher);
        self
    }

    async fn publish_success(&self, auth: &Authentication) {
        if let Some(publisher) = &self.event_publisher {
            publisher
                .publish(AuthenticationEvent::Success {
                    principal: auth.principal.clone(),
                })
                .await;
        }
    }

    async fn publish_failure(&self, request: &AuthenticationRequest, error: &SecurityError) {
        if let Some(publisher) = &self.event_publisher {
            let username = match request {
                AuthenticationRequest::UsernamePassword { username, .. } => Some(username.clone()),
                _ => None,
            };
            publisher
                .publish(AuthenticationEvent::Failure {
                    username,
                    error: error.to_string(),
                })
                .await;
        }
    }
}

impl std::fmt::Debug for ProviderManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderManager")
            .field("providers", &self.providers.len())
            .finish()
    }
}

#[async_trait]
impl AuthenticationManager for ProviderManager {
    async fn authenticate(
        &self,
        request: AuthenticationRequest,
    ) -> Result<Authentication, SecurityError> {
        let mut last_err: Option<SecurityError> = None;
        let mut supported = false;
        for provider in &self.providers {
            if !provider.supports(&request) {
                continue;
            }
            supported = true;
            match provider.authenticate(&request).await {
                Ok(auth) => {
                    self.publish_success(&auth).await;
                    return Ok(auth);
                }
                Err(err) => last_err = Some(err),
            }
        }
        let error = if supported {
            last_err.unwrap_or(SecurityError::Unauthenticated)
        } else {
            SecurityError::verification(
                "no AuthenticationProvider supports the presented credentials",
            )
        };
        self.publish_failure(&request, &error).await;
        Err(error)
    }
}

/// Adapts a token [`Verifier`] into a bearer-token [`AuthenticationProvider`],
/// so the existing JWT/JWKS verifiers slot into the [`ProviderManager`] spine.
pub struct BearerTokenAuthenticationProvider {
    verifier: Arc<dyn Verifier>,
}

impl BearerTokenAuthenticationProvider {
    /// Wraps `verifier` as a bearer-token provider.
    #[must_use]
    pub fn new(verifier: Arc<dyn Verifier>) -> Self {
        Self { verifier }
    }
}

impl std::fmt::Debug for BearerTokenAuthenticationProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BearerTokenAuthenticationProvider")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AuthenticationProvider for BearerTokenAuthenticationProvider {
    fn supports(&self, request: &AuthenticationRequest) -> bool {
        matches!(request, AuthenticationRequest::BearerToken(_))
    }

    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<Authentication, SecurityError> {
        match request {
            AuthenticationRequest::BearerToken(token) => self.verifier.verify(token).await,
            _ => Err(SecurityError::verification(
                "BearerTokenAuthenticationProvider: not a bearer-token request",
            )),
        }
    }
}

/// An authentication outcome — the Rust analog of Spring's
/// `AuthenticationSuccessEvent` / `AbstractAuthenticationFailureEvent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationEvent {
    /// A principal authenticated successfully.
    Success {
        /// The authenticated principal.
        principal: String,
    },
    /// An authentication attempt failed.
    Failure {
        /// The attempted username, when the credential kind carries one.
        username: Option<String>,
        /// The failure reason (the `SecurityError`'s display).
        error: String,
    },
}

/// Receives authentication outcome events — Spring's
/// `AuthenticationEventPublisher`. Install one on a [`ProviderManager`] via
/// [`with_event_publisher`](ProviderManager::with_event_publisher).
#[async_trait]
pub trait AuthenticationEventPublisher: Send + Sync {
    /// Handles an authentication [`AuthenticationEvent`].
    async fn publish(&self, event: AuthenticationEvent);
}

/// The default [`AuthenticationEventPublisher`]: logs successes at `info` and
/// failures at `warn` (never logging credentials).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoggingAuthenticationEventPublisher;

#[async_trait]
impl AuthenticationEventPublisher for LoggingAuthenticationEventPublisher {
    async fn publish(&self, event: AuthenticationEvent) {
        match event {
            AuthenticationEvent::Success { principal } => {
                tracing::info!(principal = %principal, "authentication success");
            }
            AuthenticationEvent::Failure { username, error } => {
                tracing::warn!(username = ?username, error = %error, "authentication failure");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VerifierFn;
    use std::sync::Mutex;

    fn auth(principal: &str) -> Authentication {
        Authentication {
            principal: principal.into(),
            ..Default::default()
        }
    }

    /// A provider that authenticates `alice`/`pw` and rejects everything else.
    struct PasswordProvider;
    #[async_trait]
    impl AuthenticationProvider for PasswordProvider {
        fn supports(&self, r: &AuthenticationRequest) -> bool {
            matches!(r, AuthenticationRequest::UsernamePassword { .. })
        }
        async fn authenticate(
            &self,
            r: &AuthenticationRequest,
        ) -> Result<Authentication, SecurityError> {
            match r {
                AuthenticationRequest::UsernamePassword { username, password }
                    if username == "alice" && password == "pw" =>
                {
                    Ok(auth("alice"))
                }
                _ => Err(SecurityError::Unauthenticated),
            }
        }
    }

    fn bearer_provider() -> Arc<dyn AuthenticationProvider> {
        let verifier = VerifierFn(|t: String| async move {
            if t == "good" {
                Ok(auth("bearer-user"))
            } else {
                Err(SecurityError::verification("bad token"))
            }
        });
        Arc::new(BearerTokenAuthenticationProvider::new(Arc::new(verifier)))
    }

    #[tokio::test]
    async fn authenticates_username_password() {
        let mgr = ProviderManager::new(vec![Arc::new(PasswordProvider)]);
        let a = mgr
            .authenticate(AuthenticationRequest::username_password("alice", "pw"))
            .await
            .unwrap();
        assert_eq!(a.principal, "alice");
        assert_eq!(
            mgr.authenticate(AuthenticationRequest::username_password("alice", "wrong"))
                .await
                .unwrap_err(),
            SecurityError::Unauthenticated
        );
    }

    #[tokio::test]
    async fn routes_bearer_to_the_verifier_provider() {
        let mgr = ProviderManager::new(vec![Arc::new(PasswordProvider), bearer_provider()]);
        let a = mgr
            .authenticate(AuthenticationRequest::bearer("good"))
            .await
            .unwrap();
        assert_eq!(a.principal, "bearer-user");
        assert!(mgr
            .authenticate(AuthenticationRequest::bearer("bad"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn no_supporting_provider_is_an_error() {
        // Only a password provider — a bearer request is unsupported.
        let mgr = ProviderManager::new(vec![Arc::new(PasswordProvider)]);
        let err = mgr
            .authenticate(AuthenticationRequest::bearer("x"))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("no AuthenticationProvider supports"),
            "got {err}"
        );
    }

    #[tokio::test]
    async fn first_supporting_success_wins() {
        // A rejecting provider precedes the accepting one; ProviderManager must
        // try both and return the second's success.
        struct Reject;
        #[async_trait]
        impl AuthenticationProvider for Reject {
            fn supports(&self, r: &AuthenticationRequest) -> bool {
                matches!(r, AuthenticationRequest::UsernamePassword { .. })
            }
            async fn authenticate(
                &self,
                _: &AuthenticationRequest,
            ) -> Result<Authentication, SecurityError> {
                Err(SecurityError::Unauthenticated)
            }
        }
        let mgr = ProviderManager::new(vec![Arc::new(Reject), Arc::new(PasswordProvider)]);
        let a = mgr
            .authenticate(AuthenticationRequest::username_password("alice", "pw"))
            .await
            .unwrap();
        assert_eq!(a.principal, "alice");
    }

    #[tokio::test]
    async fn publishes_success_and_failure_events() {
        #[derive(Default)]
        struct Recorder(Mutex<Vec<AuthenticationEvent>>);
        #[async_trait]
        impl AuthenticationEventPublisher for Recorder {
            async fn publish(&self, event: AuthenticationEvent) {
                self.0.lock().unwrap().push(event);
            }
        }
        let recorder = Arc::new(Recorder::default());
        let mgr = ProviderManager::new(vec![Arc::new(PasswordProvider)])
            .with_event_publisher(recorder.clone());

        // Success -> Success{principal}.
        mgr.authenticate(AuthenticationRequest::username_password("alice", "pw"))
            .await
            .unwrap();
        // Failure -> Failure{username, error}.
        let _ = mgr
            .authenticate(AuthenticationRequest::username_password("alice", "bad"))
            .await;

        let events = recorder.0.lock().unwrap().clone();
        assert_eq!(
            events[0],
            AuthenticationEvent::Success {
                principal: "alice".into()
            }
        );
        assert!(matches!(
            &events[1],
            AuthenticationEvent::Failure { username: Some(u), .. } if u == "alice"
        ));

        // A bearer failure (no supporting provider) -> Failure{username: None}.
        let _ = mgr.authenticate(AuthenticationRequest::bearer("x")).await;
        let events = recorder.0.lock().unwrap().clone();
        assert!(matches!(
            events.last(),
            Some(AuthenticationEvent::Failure { username: None, .. })
        ));
    }

    #[tokio::test]
    async fn with_provider_builder_composes() {
        let mgr = ProviderManager::default()
            .with_provider(Arc::new(PasswordProvider))
            .with_provider(bearer_provider());
        assert_eq!(
            mgr.authenticate(AuthenticationRequest::bearer("good"))
                .await
                .unwrap()
                .principal,
            "bearer-user"
        );
    }
}
