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

//! `SecurityContextRepository` — the Rust analog of Spring Security's
//! `SecurityContextRepository`: where the authenticated context
//! ([`Authentication`]) is loaded from and saved to *between* requests.
//!
//! [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer) loads the
//! context through a repository at the start of each request;
//! authentication-success handlers (OAuth2 login, one-time-token, WebAuthn)
//! save it. Decoupling the layer from a fixed session attribute lets an
//! application choose its persistence strategy:
//!
//! * [`HttpSessionSecurityContextRepository`] (default) — the
//!   `firefly_session::Session`-backed store (Spring's
//!   `HttpSessionSecurityContextRepository`).
//! * [`NullSecurityContextRepository`] — never persists, for stateless
//!   (token-only) APIs (Spring's `NullSecurityContextRepository`).

use async_trait::async_trait;

use firefly_session::Session;

use crate::authentication::Authentication;
use crate::oauth2::SESSION_KEY_SECURITY_CONTEXT;

/// Loads and saves the ambient [`Authentication`] across requests — Spring's
/// `SecurityContextRepository`, scoped to the per-request
/// [`firefly_session::Session`].
#[async_trait]
pub trait SecurityContextRepository: Send + Sync {
    /// Loads the stored, *authenticated* context, or `None` when absent /
    /// anonymous / unreadable.
    async fn load(&self, session: &Session) -> Option<Authentication>;

    /// Persists `auth` so a later request can [`load`](Self::load) it.
    async fn save(&self, session: &Session, auth: &Authentication);

    /// Whether an authenticated context is currently stored.
    async fn contains(&self, session: &Session) -> bool {
        self.load(session).await.is_some()
    }
}

/// Session-backed repository — Spring's `HttpSessionSecurityContextRepository`.
///
/// Stores the JSON-serialized [`Authentication`] under a session attribute key
/// (default [`SESSION_KEY_SECURITY_CONTEXT`]). Only a fully-authenticated
/// (non-anonymous) context is loaded or saved.
#[derive(Debug, Clone)]
pub struct HttpSessionSecurityContextRepository {
    key: String,
}

impl HttpSessionSecurityContextRepository {
    /// Builds the repository keyed on the default
    /// [`SESSION_KEY_SECURITY_CONTEXT`] attribute (wire-compatible with the
    /// OAuth2-login / one-time-token / WebAuthn handlers).
    #[must_use]
    pub fn new() -> Self {
        Self {
            key: SESSION_KEY_SECURITY_CONTEXT.to_owned(),
        }
    }

    /// Builds the repository keyed on a custom session attribute.
    #[must_use]
    pub fn with_key(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }
}

impl Default for HttpSessionSecurityContextRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecurityContextRepository for HttpSessionSecurityContextRepository {
    async fn load(&self, session: &Session) -> Option<Authentication> {
        // The auth-success handlers store the context as a JSON *string*
        // (`serde_json::to_string(&auth)`); read it back that way, falling back
        // to a directly-stored object for callers that set a typed value.
        let auth = match session.attribute::<String>(&self.key).await {
            Some(serialized) => serde_json::from_str::<Authentication>(&serialized).ok()?,
            None => session.attribute::<Authentication>(&self.key).await?,
        };
        auth.is_authenticated().then_some(auth)
    }

    async fn save(&self, session: &Session, auth: &Authentication) {
        if let Ok(serialized) = serde_json::to_string(auth) {
            let _ = session.set_attribute(&self.key, serialized).await;
        }
    }
}

/// A repository that never persists — Spring's `NullSecurityContextRepository`,
/// for stateless (token-only / `STATELESS`) APIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSecurityContextRepository;

#[async_trait]
impl SecurityContextRepository for NullSecurityContextRepository {
    async fn load(&self, _session: &Session) -> Option<Authentication> {
        None
    }
    async fn save(&self, _session: &Session, _auth: &Authentication) {}
    async fn contains(&self, _session: &Session) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_session::SessionInner;

    fn user(principal: &str) -> Authentication {
        Authentication {
            principal: principal.into(),
            username: principal.into(),
            roles: vec!["USER".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn http_session_repo_round_trips_and_skips_anonymous() {
        let repo = HttpSessionSecurityContextRepository::new();
        let session = Session::new(SessionInner::new("sid"));

        // Empty session: nothing stored.
        assert!(repo.load(&session).await.is_none());
        assert!(!repo.contains(&session).await);

        // Save then load an authenticated context.
        repo.save(&session, &user("u1")).await;
        let loaded = repo.load(&session).await.expect("loaded");
        assert_eq!(loaded.principal, "u1");
        assert!(loaded.has_role("USER"));
        assert!(repo.contains(&session).await);

        // An anonymous context is never returned as authenticated.
        let anon_session = Session::new(SessionInner::new("sid2"));
        repo.save(&anon_session, &Authentication::anonymous()).await;
        assert!(repo.load(&anon_session).await.is_none());
    }

    #[tokio::test]
    async fn http_session_repo_is_wire_compatible_with_the_handlers() {
        // The OAuth2/OTT/WebAuthn handlers write the JSON string under
        // SESSION_KEY_SECURITY_CONTEXT directly; the default repo must read it.
        let session = Session::new(SessionInner::new("sid"));
        session
            .set_attribute(
                SESSION_KEY_SECURITY_CONTEXT,
                serde_json::to_string(&user("handler-user")).unwrap(),
            )
            .await
            .unwrap();
        let loaded = HttpSessionSecurityContextRepository::new()
            .load(&session)
            .await
            .expect("loaded handler-written context");
        assert_eq!(loaded.principal, "handler-user");
    }

    #[tokio::test]
    async fn load_falls_back_to_a_typed_object() {
        // A caller that stored the Authentication as a typed value (not a JSON
        // string) is read via the object fallback branch.
        let session = Session::new(SessionInner::new("sid"));
        session
            .set_attribute(SESSION_KEY_SECURITY_CONTEXT, user("typed"))
            .await
            .unwrap();
        let loaded = HttpSessionSecurityContextRepository::new()
            .load(&session)
            .await
            .expect("loaded via the typed-object fallback");
        assert_eq!(loaded.principal, "typed");
    }

    #[tokio::test]
    async fn custom_key_is_isolated() {
        let repo = HttpSessionSecurityContextRepository::with_key("CUSTOM_CTX");
        let session = Session::new(SessionInner::new("sid"));
        repo.save(&session, &user("u1")).await;
        assert!(repo.load(&session).await.is_some());
        // The default-keyed repo does not see it.
        assert!(HttpSessionSecurityContextRepository::new()
            .load(&session)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn null_repo_never_persists() {
        let repo = NullSecurityContextRepository;
        let session = Session::new(SessionInner::new("sid"));
        repo.save(&session, &user("u1")).await;
        assert!(repo.load(&session).await.is_none());
        assert!(!repo.contains(&session).await);
    }
}
