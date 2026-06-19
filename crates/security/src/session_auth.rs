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

//! Session-backed authentication restore + the `firefly-session` bridge
//! for the OAuth2 login flow (pyfly:
//! `pyfly.security.oauth2.session_security_filter.OAuth2SessionSecurityFilter`
//! and the cookie-keyed `LoginSessionStore` behind
//! `pyfly.security.oauth2.login`).
//!
//! Two pieces close the browser-login → authenticated-request loop:
//!
//! 1. [`SessionAuthenticationLayer`] — a tower layer that reads the
//!    `SECURITY_CONTEXT` attribute stored by the OAuth2 login handler from
//!    the request's [`firefly_session::Session`], deserializes it into an
//!    [`Authentication`], and inserts it into the request extensions so
//!    [`BearerLayer`](crate::BearerLayer), [`FilterChain`](crate::FilterChain),
//!    and the [`guards`](crate::guards) see the session-established
//!    principal. When no authenticated context is stored, an
//!    [`Authentication::anonymous`] is inserted (unless one is already
//!    present) so downstream filters always have a context — exactly
//!    pyfly's `OAuth2SessionSecurityFilter` behaviour.
//!
//! 2. [`SessionLoginSessionStore`] — a [`LoginSessionStore`] that resolves
//!    a per-browser [`firefly_session::Session`] from the request's session
//!    cookie and adapts it (via [`SessionLoginSession`]) to the login flow's
//!    [`LoginSession`] trait, persisting through any
//!    [`firefly_session::SessionStore`] (memory/redis/postgres). This
//!    replaces [`FixedLoginSessionStore`](crate::oauth2::FixedLoginSessionStore)
//!    in multi-user deployments so OAuth2 login and subsequent authenticated
//!    requests share one real, distributed session.
//!
//! # Wiring order
//!
//! Mount [`firefly_session::SessionLayer`] outermost, then
//! [`SessionAuthenticationLayer`], then any [`BearerLayer`]: axum runs the
//! last-added layer first, so add `BearerLayer` last so a bearer token can
//! still override (or coexist with) a session-established context — pyfly
//! runs `OAuth2SessionSecurityFilter` *after* the JWT filter so the
//! session context takes priority. To reproduce pyfly's "session wins"
//! ordering, place [`SessionAuthenticationLayer`] so it runs *after* the
//! bearer layer (add it before/below the bearer layer).

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::Request;
use axum::response::Response;
use http::header::COOKIE;
use http::HeaderMap;
use tokio::sync::Mutex;
use tower::{Layer, Service};

use firefly_session::{
    new_session_id, Session, SessionConfig, SessionInner, SessionSigner, SessionStore,
};

use crate::authentication::Authentication;
use crate::oauth2::{LoginSession, LoginSessionStore};
use crate::security_context::{HttpSessionSecurityContextRepository, SecurityContextRepository};

/// Tower [`Layer`] that restores an [`Authentication`] from the request's
/// [`firefly_session::Session`] — the Rust port of pyfly's
/// `OAuth2SessionSecurityFilter`.
///
/// Reads the `SECURITY_CONTEXT` attribute (the JSON the OAuth2 login
/// handler stores on successful login), deserializes it into an
/// [`Authentication`], and inserts it into the request extensions when it
/// names a real principal. When the session carries no authenticated
/// context, an [`Authentication::anonymous`] is inserted unless one is
/// already present, so downstream filters/handlers always have a context.
///
/// Requires [`firefly_session::SessionLayer`] to run *before* it (it reads
/// the `Session` handle from the request extensions). A request without a
/// `Session` handle is treated as having no session: an anonymous context
/// is inserted.
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use axum::{routing::get, Router};
/// use firefly_security::SessionAuthenticationLayer;
/// use firefly_session::{MemorySessionStore, SessionLayer, SessionStore};
///
/// let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
/// let app: Router = Router::new()
///     .route("/me", get(|| async { "ok" }))
///     // last-added layer runs first: SessionLayer must run before the
///     // restore layer, so add it after.
///     .layer(SessionAuthenticationLayer::new())
///     .layer(SessionLayer::new(store));
/// ```
#[derive(Clone)]
pub struct SessionAuthenticationLayer {
    anonymous_fallback: bool,
    repository: Arc<dyn SecurityContextRepository>,
}

impl SessionAuthenticationLayer {
    /// Builds the layer with pyfly defaults: an [`Authentication::anonymous`]
    /// is inserted when the session carries no authenticated context, and the
    /// context is loaded via the default
    /// [`HttpSessionSecurityContextRepository`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            anonymous_fallback: true,
            repository: Arc::new(HttpSessionSecurityContextRepository::new()),
        }
    }

    /// Controls whether an [`Authentication::anonymous`] is inserted when no
    /// session context is found (default `true`, matching pyfly). When set
    /// to `false`, an unauthenticated request is left without any
    /// [`Authentication`] in its extensions, so a following
    /// [`BearerLayer`](crate::BearerLayer) can take over cleanly.
    #[must_use]
    pub fn anonymous_fallback(mut self, enabled: bool) -> Self {
        self.anonymous_fallback = enabled;
        self
    }

    /// Sets the [`SecurityContextRepository`] used to load the per-request
    /// context (default [`HttpSessionSecurityContextRepository`]). Use a
    /// [`NullSecurityContextRepository`](crate::NullSecurityContextRepository)
    /// for a stateless surface, or a custom-keyed repository.
    #[must_use]
    pub fn with_repository(mut self, repository: Arc<dyn SecurityContextRepository>) -> Self {
        self.repository = repository;
        self
    }
}

impl Default for SessionAuthenticationLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SessionAuthenticationLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionAuthenticationLayer")
            .field("anonymous_fallback", &self.anonymous_fallback)
            .finish()
    }
}

impl<S> Layer<S> for SessionAuthenticationLayer {
    type Service = SessionAuthenticationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SessionAuthenticationService {
            inner,
            anonymous_fallback: self.anonymous_fallback,
            repository: self.repository.clone(),
        }
    }
}

/// The tower service produced by [`SessionAuthenticationLayer`].
#[derive(Clone)]
pub struct SessionAuthenticationService<S> {
    inner: S,
    anonymous_fallback: bool,
    repository: Arc<dyn SecurityContextRepository>,
}

impl<S> Service<Request> for SessionAuthenticationService<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request) -> Self::Future {
        let anonymous_fallback = self.anonymous_fallback;
        let repository = self.repository.clone();
        // Standard tower buffering: invoke the version we drove to readiness.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        // Clone the (Send+Sync) Session handle out of the request before any
        // await so the future does not borrow the non-`Sync` request across
        // an await point.
        let session = req.extensions().get::<Session>().cloned();
        let already_present = req.extensions().get::<Authentication>().is_some();

        Box::pin(async move {
            let restored = match session {
                Some(session) => repository.load(&session).await,
                None => None,
            };
            // Make the context ambient two ways: insert it into the request
            // extensions (read by `FilterChain` / `guards` / handlers) AND scope
            // the task-local `CURRENT_AUTH` around the downstream call (read by
            // the `#[pre_authorize]` / `#[post_authorize]` macros, `check_access`,
            // and `current_authentication()`) — exactly as `BearerLayer` does.
            // Without the task-local scope, method security silently fails for a
            // session-authenticated caller.
            let scoped = match restored {
                Some(auth) => {
                    req.extensions_mut().insert(auth.clone());
                    Some(auth)
                }
                None if anonymous_fallback && !already_present => {
                    let anon = Authentication::anonymous();
                    req.extensions_mut().insert(anon.clone());
                    Some(anon)
                }
                None => None,
            };
            match scoped {
                Some(auth) => crate::with_authentication_scope(auth, inner.call(req)).await,
                None => inner.call(req).await,
            }
        })
    }
}

/// A [`LoginSession`] backed by a real [`firefly_session::Session`] handle.
///
/// Wraps the per-browser session resolved by [`SessionLoginSessionStore`]
/// so the OAuth2 login flow stores `state`/`nonce`/PKCE-verifier and the
/// final `SECURITY_CONTEXT` through `firefly-session`'s store
/// (memory/redis/postgres), exactly where
/// [`SessionAuthenticationLayer`] later reads them back.
pub struct SessionLoginSession {
    session: Session,
    store: Arc<dyn SessionStore>,
    ttl: Duration,
}

impl std::fmt::Debug for SessionLoginSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLoginSession")
            .finish_non_exhaustive()
    }
}

impl SessionLoginSession {
    /// Persists the wrapped session through the [`SessionStore`] (or deletes
    /// it when invalidated, also clearing the pre-rotation id). Called after
    /// every mutation so a distributed store stays consistent across the
    /// stateless OAuth2 callback hops.
    async fn persist(&self) {
        let inner = self.session.lock().await;
        if let Some(prev) = inner.previous_id() {
            if prev != inner.id() {
                let _ = self.store.delete(prev).await;
            }
        }
        if inner.invalidated() {
            let _ = self.store.delete(inner.id()).await;
        } else {
            let _ = self.store.save(inner.id(), inner.data(), self.ttl).await;
        }
    }
}

#[async_trait]
impl LoginSession for SessionLoginSession {
    async fn get_attribute(&self, key: &str) -> Option<String> {
        // OAuth2 attributes (state/nonce/verifier/SECURITY_CONTEXT) are
        // stored as JSON strings by this bridge; read them back as strings.
        self.session.attribute::<String>(key).await
    }

    async fn set_attribute(&self, key: &str, value: String) {
        let _ = self.session.set_attribute(key, value).await;
        self.persist().await;
    }

    async fn remove_attribute(&self, key: &str) {
        self.session.remove_attribute(key).await;
        self.persist().await;
    }

    async fn rotate_id(&self) {
        self.session.rotate_id().await;
        self.persist().await;
    }

    async fn invalidate(&self) {
        self.session.invalidate().await;
        self.persist().await;
    }

    async fn id(&self) -> Option<String> {
        Some(self.session.id().await)
    }
}

/// A [`LoginSessionStore`] that resolves a per-browser
/// [`firefly_session::Session`] from the request's session cookie and backs
/// it onto a real [`firefly_session::SessionStore`].
///
/// This is the production counterpart to
/// [`FixedLoginSessionStore`](crate::oauth2::FixedLoginSessionStore): each
/// browser gets its own session keyed by the session cookie, persisted to
/// memory/Redis/Postgres, so OAuth2 login and the subsequent authenticated
/// requests (restored by [`SessionAuthenticationLayer`]) share one session.
///
/// Construct it with the same [`SessionConfig`] (and optional
/// [`SessionSigner`]) used by [`firefly_session::SessionLayer`] so the
/// cookie name, TTL, and signing agree.
pub struct SessionLoginSessionStore {
    store: Arc<dyn SessionStore>,
    config: Arc<SessionConfig>,
    signer: Option<Arc<SessionSigner>>,
    /// Caches the [`Session`] resolved for each session id within the
    /// process so two calls in the same request (authorize → set state) see
    /// the same handle.
    resolved: Mutex<std::collections::HashMap<String, Session>>,
}

impl std::fmt::Debug for SessionLoginSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLoginSessionStore")
            .field("config", &self.config)
            .field("signed", &self.signer.is_some())
            .finish_non_exhaustive()
    }
}

impl SessionLoginSessionStore {
    /// Builds the store over `store` with the default [`SessionConfig`].
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            config: Arc::new(SessionConfig::default()),
            signer: None,
            resolved: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Builds the store with an explicit `config` (cookie name, TTL, …).
    #[must_use]
    pub fn from_config(config: SessionConfig, store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            config: Arc::new(config),
            signer: None,
            resolved: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Attaches the HMAC [`SessionSigner`] used by the session layer so the
    /// cookie value verifies the same way.
    #[must_use]
    pub fn with_signer(mut self, signer: SessionSigner) -> Self {
        self.signer = Some(Arc::new(signer));
        self
    }

    /// Extracts the session id from the `Cookie` header for the configured
    /// cookie name, verifying+stripping the signature when one is set.
    fn cookie_id(&self, headers: &HeaderMap) -> Option<String> {
        let raw = cookie_value(headers, &self.config.cookie_name)?;
        match &self.signer {
            Some(s) => s.verify(&raw),
            None => Some(raw),
        }
    }
}

#[async_trait]
impl LoginSessionStore for SessionLoginSessionStore {
    async fn session(&self, headers: &HeaderMap) -> Arc<dyn LoginSession> {
        let cookie_id = self.cookie_id(headers);

        // Resolve (and cache) the per-id Session handle: load from the store
        // on a cookie hit, else mint a fresh session.
        let session = {
            let id = cookie_id.clone();
            let mut cache = self.resolved.lock().await;
            let resolved = if let Some(id) = &id {
                if let Some(existing) = cache.get(id) {
                    Some(existing.clone())
                } else if let Ok(Some(data)) = self.store.get(id).await {
                    let s = Session::new(SessionInner::load(id.clone(), data));
                    cache.insert(id.clone(), s.clone());
                    Some(s)
                } else {
                    None
                }
            } else {
                None
            };
            match resolved {
                Some(s) => s,
                None => {
                    let s = Session::new(SessionInner::new(new_session_id()));
                    let sid = s.id().await;
                    cache.insert(sid, s.clone());
                    s
                }
            }
        };

        Arc::new(SessionLoginSession {
            session,
            store: self.store.clone(),
            ttl: self.config.idle_timeout(),
        }) as Arc<dyn LoginSession>
    }
}

/// Parses the `Cookie` request header (`a=1; b=2`) and returns the value for
/// `name`. Mirrors `firefly_session`'s own cookie parsing (trim per RFC 6265).
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth2::SESSION_KEY_SECURITY_CONTEXT;
    use firefly_session::MemorySessionStore;

    // Context load/restore semantics now live in `security_context.rs`
    // (HttpSessionSecurityContextRepository) and are exercised there; the
    // SessionAuthenticationLayer's use of the repository is covered by the
    // `service_scopes_*` tests below.

    #[test]
    fn cookie_value_parses_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "a=1; PYFLY_SESSION=abc; b=2".parse().unwrap());
        assert_eq!(
            cookie_value(&headers, "PYFLY_SESSION").as_deref(),
            Some("abc")
        );
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[tokio::test]
    async fn store_persists_and_reloads_attributes_by_cookie() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let login_store = SessionLoginSessionStore::new(store.clone());

        // First request: no cookie -> fresh session, set an attribute.
        let s1 = login_store.session(&HeaderMap::new()).await;
        s1.set_attribute("oauth2_state", "xyz".into()).await;

        // Find the session id the bridge minted (only one in cache).
        let sid = {
            let cache = login_store.resolved.lock().await;
            cache.keys().next().cloned().unwrap()
        };
        // It was persisted to the real store.
        assert!(store.exists(&sid).await.unwrap());

        // A second store instance (simulating a fresh process / next hop)
        // resolves the same session from the cookie and reads the attribute.
        let next = SessionLoginSessionStore::new(store.clone());
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, format!("PYFLY_SESSION={sid}").parse().unwrap());
        let s2 = next.session(&headers).await;
        assert_eq!(
            s2.get_attribute("oauth2_state").await.as_deref(),
            Some("xyz")
        );
    }

    #[tokio::test]
    async fn store_persists_security_context_for_restore() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let login_store = SessionLoginSessionStore::new(store.clone());

        let s1 = login_store.session(&HeaderMap::new()).await;
        s1.rotate_id().await; // anti-fixation, as the login handler does
        let auth = Authentication {
            principal: "u1".into(),
            ..Default::default()
        };
        s1.set_attribute(
            SESSION_KEY_SECURITY_CONTEXT,
            serde_json::to_string(&auth).unwrap(),
        )
        .await;

        // The post-rotation id is the live one.
        let sid = {
            let cache = login_store.resolved.lock().await;
            // The cache holds the original id (keyed at resolution time); the
            // live id is whatever the handle now reports.
            let any = cache.values().next().unwrap().clone();
            any.id().await
        };
        let data = store.get(&sid).await.unwrap().unwrap();
        let stored = data.get(SESSION_KEY_SECURITY_CONTEXT).unwrap();
        // Stored as a JSON string holding the serialized Authentication.
        let s = stored.as_str().unwrap();
        let back: Authentication = serde_json::from_str(s).unwrap();
        assert_eq!(back.principal, "u1");
    }

    #[tokio::test]
    async fn invalidate_deletes_from_store() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let login_store = SessionLoginSessionStore::new(store.clone());
        let s1 = login_store.session(&HeaderMap::new()).await;
        s1.set_attribute("k", "v".into()).await;
        let sid = {
            let cache = login_store.resolved.lock().await;
            cache.keys().next().cloned().unwrap()
        };
        assert!(store.exists(&sid).await.unwrap());
        s1.invalidate().await;
        assert!(!store.exists(&sid).await.unwrap());
    }

    // H1: the layer must scope the *task-local* `CURRENT_AUTH` (what
    // `#[pre_authorize]` / `check_access` read), not just the request
    // extension — otherwise method security silently fails for a
    // session-authenticated caller (it only worked behind `BearerLayer`).
    #[tokio::test]
    async fn service_scopes_task_local_for_session_authenticated_request() {
        use std::sync::Mutex;
        use tower::ServiceExt;

        let session = Session::new(SessionInner::new("sid"));
        let auth = Authentication {
            principal: "u1".into(),
            username: "alice".into(),
            roles: vec!["ADMIN".into()],
            ..Default::default()
        };
        session
            .set_attribute(
                SESSION_KEY_SECURITY_CONTEXT,
                serde_json::to_string(&auth).unwrap(),
            )
            .await
            .unwrap();

        // Inner service records what `current_authentication()` (the task-local)
        // reports at call time — the contract method security depends on.
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let probe = seen.clone();
        let inner = tower::service_fn(move |_req: Request| {
            let probe = probe.clone();
            async move {
                *probe.lock().unwrap() = crate::current_authentication().map(|a| a.principal);
                Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
            }
        });

        let mut req = Request::new(axum::body::Body::empty());
        req.extensions_mut().insert(session);

        let _ = SessionAuthenticationLayer::new()
            .layer(inner)
            .oneshot(req)
            .await
            .unwrap();

        assert_eq!(seen.lock().unwrap().as_deref(), Some("u1"));
    }

    // H1 (anonymous path): with the default anonymous fallback, the layer
    // should scope an anonymous context so downstream method security sees a
    // present-but-anonymous principal (Spring's AnonymousAuthenticationFilter).
    #[tokio::test]
    async fn service_scopes_anonymous_when_no_session_context() {
        use std::sync::Mutex;
        use tower::ServiceExt;

        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let probe = seen.clone();
        let inner = tower::service_fn(move |_req: Request| {
            let probe = probe.clone();
            async move {
                *probe.lock().unwrap() = crate::current_authentication().map(|a| a.principal);
                Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
            }
        });

        // No Session handle at all -> anonymous fallback applies.
        let req = Request::new(axum::body::Body::empty());
        let _ = SessionAuthenticationLayer::new()
            .layer(inner)
            .oneshot(req)
            .await
            .unwrap();

        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some(crate::authentication::ANONYMOUS_ID)
        );
    }
}
