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

//! One-time-token (magic-link) login — the Rust analog of Spring Security
//! 6.4's `oneTimeTokenLogin()`.
//!
//! A passwordless flow in two legs:
//!
//! 1. `POST /ott/generate` (`username`) mints a single-use, time-limited token
//!    via [`OneTimeTokenService`] and hands it to a
//!    [`OneTimeTokenGenerationSuccessHandler`] for out-of-band delivery (email
//!    / SMS magic link). The HTTP response never reveals the token or whether
//!    the account exists.
//! 2. The user clicks the delivered link — `GET /login/ott?token=…` — which
//!    [`consume`](OneTimeTokenService::consume)s the token (single-use,
//!    expiry-checked), builds an [`Authentication`], rotates the session id
//!    (anti-fixation), stores the security context in the
//!    [`firefly_session::Session`], and redirects.
//!
//! [`InMemoryOneTimeTokenService`] ships for single-process apps; a
//! distributed deployment supplies its own [`OneTimeTokenService`] over a
//! shared store. The default [`LoggingOttHandler`] only records that a token
//! was issued (never the token value) — wire a real delivery handler in
//! production.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::extract::{Form, Query, State};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use http::{header, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;

use firefly_session::Session;

use crate::authentication::Authentication;
use crate::csrf::random_urlsafe;
use crate::oauth2::SESSION_KEY_SECURITY_CONTEXT;

/// Default one-time-token lifetime (5 minutes), matching the short window
/// expected of a magic link.
pub const DEFAULT_OTT_TTL_SECONDS: u64 = 300;

/// A minted one-time token: its opaque `value`, the `username` it
/// authenticates, and the epoch-seconds `expires_at` after which it is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneTimeToken {
    /// The opaque, high-entropy token value embedded in the magic link.
    pub value: String,
    /// The username the token will authenticate when consumed.
    pub username: String,
    /// Expiry as epoch seconds.
    pub expires_at: u64,
}

/// Mints and redeems one-time login tokens — Spring's `OneTimeTokenService`.
#[async_trait]
pub trait OneTimeTokenService: Send + Sync {
    /// Issues a fresh single-use token for `username`.
    async fn generate(&self, username: &str) -> OneTimeToken;
    /// Redeems `value`, returning the username iff the token is known,
    /// unexpired, and not already used. The token is invalidated (single-use)
    /// whether or not it had expired.
    async fn consume(&self, value: &str) -> Option<String>;
}

/// In-process [`OneTimeTokenService`] backed by a map — suitable for a single
/// instance. A distributed deployment supplies its own implementation over a
/// shared store (Redis/Postgres) so a link minted on one node redeems on
/// another.
pub struct InMemoryOneTimeTokenService {
    ttl_seconds: u64,
    tokens: Mutex<HashMap<String, (String, u64)>>,
}

impl InMemoryOneTimeTokenService {
    /// Builds the service with the default [`DEFAULT_OTT_TTL_SECONDS`] lifetime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ttl_seconds: DEFAULT_OTT_TTL_SECONDS,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// Overrides the token lifetime in seconds.
    #[must_use]
    pub fn ttl_seconds(mut self, seconds: u64) -> Self {
        self.ttl_seconds = seconds;
        self
    }
}

impl Default for InMemoryOneTimeTokenService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OneTimeTokenService for InMemoryOneTimeTokenService {
    async fn generate(&self, username: &str) -> OneTimeToken {
        // 32 bytes of OS entropy → 43-char URL-safe value, as elsewhere in the
        // crate (CSRF / OAuth2 state).
        let value = random_urlsafe(32);
        let expires_at = now_secs().saturating_add(self.ttl_seconds);
        self.tokens
            .lock()
            .await
            .insert(value.clone(), (username.to_string(), expires_at));
        OneTimeToken {
            value,
            username: username.to_string(),
            expires_at,
        }
    }

    async fn consume(&self, value: &str) -> Option<String> {
        // Single-use: remove on lookup so a replay (or an expired token) can
        // never be redeemed twice.
        let (username, expires_at) = self.tokens.lock().await.remove(value)?;
        if now_secs() > expires_at {
            return None;
        }
        Some(username)
    }
}

/// Delivers a freshly-minted [`OneTimeToken`] out of band (email / SMS magic
/// link) — Spring's `OneTimeTokenGenerationSuccessHandler`.
#[async_trait]
pub trait OneTimeTokenGenerationSuccessHandler: Send + Sync {
    /// Handles a newly-generated token (e.g. emails the magic link).
    async fn handle(&self, token: &OneTimeToken);
}

/// The default handler: records *that* a token was issued, never its value
/// (logging the value would defeat the point). Replace it with a real delivery
/// handler (e.g. over `firefly-notifications`) in production.
#[derive(Debug, Clone, Default)]
pub struct LoggingOttHandler;

#[async_trait]
impl OneTimeTokenGenerationSuccessHandler for LoggingOttHandler {
    async fn handle(&self, token: &OneTimeToken) {
        tracing::info!(
            username = %token.username,
            "one-time login token generated; deliver the magic link out of band \
             (no real delivery handler configured)"
        );
    }
}

/// Shared state for the one-time-token login routes.
pub struct OttLoginState {
    /// Mints and redeems tokens.
    pub service: Arc<dyn OneTimeTokenService>,
    /// Delivers a generated token's magic link.
    pub handler: Arc<dyn OneTimeTokenGenerationSuccessHandler>,
    /// Where to redirect after a successful login (default `"/"`).
    pub success_redirect: String,
}

impl OttLoginState {
    /// Builds the state from a service and a delivery handler, redirecting to
    /// `"/"` on success.
    #[must_use]
    pub fn new(
        service: Arc<dyn OneTimeTokenService>,
        handler: Arc<dyn OneTimeTokenGenerationSuccessHandler>,
    ) -> Self {
        Self {
            service,
            handler,
            success_redirect: "/".to_string(),
        }
    }

    /// Overrides the post-login redirect target.
    #[must_use]
    pub fn success_redirect(mut self, target: impl Into<String>) -> Self {
        self.success_redirect = target.into();
        self
    }
}

#[derive(Deserialize)]
struct GenerateBody {
    username: String,
}

#[derive(Deserialize)]
struct ConsumeParams {
    token: String,
}

/// `POST /ott/generate` — mint a token and hand it to the delivery handler.
/// The response is deliberately generic so it neither leaks the token nor
/// reveals whether the account exists (Spring's behaviour).
async fn handle_generate(
    State(state): State<Arc<OttLoginState>>,
    Form(body): Form<GenerateBody>,
) -> Response {
    let token = state.service.generate(&body.username).await;
    state.handler.handle(&token).await;
    (
        StatusCode::OK,
        "If the account exists, a one-time login link has been sent.",
    )
        .into_response()
}

/// `GET /login/ott?token=…` — redeem a token (single-use, expiry-checked),
/// establish the session security context, rotate the session id, and redirect.
async fn handle_login(
    State(state): State<Arc<OttLoginState>>,
    Extension(session): Extension<Session>,
    Query(params): Query<ConsumeParams>,
) -> Response {
    let Some(username) = state.service.consume(&params.token).await else {
        return crate::problem::unauthorized("Invalid or expired one-time token");
    };

    let auth = Authentication {
        principal: username.clone(),
        username,
        ..Default::default()
    };

    // Anti-fixation: rotate the session id on authentication, then store the
    // security context where `SessionAuthenticationLayer` restores it.
    session.rotate_id().await;
    let serialized = serde_json::to_string(&auth).expect("Authentication serializes to JSON");
    let _ = session
        .set_attribute(SESSION_KEY_SECURITY_CONTEXT, serialized)
        .await;

    redirect(&state.success_redirect)
}

/// Builds the one-time-token login routes (`POST /ott/generate`,
/// `GET /login/ott`). Mount behind a [`firefly_session::SessionLayer`] so a
/// [`Session`] is available; pair with a
/// [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer) to restore
/// the established context on subsequent requests.
pub fn ott_login_routes(state: Arc<OttLoginState>) -> Router {
    Router::new()
        .route("/ott/generate", axum::routing::post(handle_generate))
        .route("/login/ott", axum::routing::get(handle_login))
        .with_state(state)
}

/// 302 redirect to `location`.
fn redirect(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(axum::body::Body::empty())
        .expect("static redirect response must build")
}

/// The current wall-clock time in epoch seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_session::SessionInner;
    use tower::ServiceExt;

    fn svc() -> InMemoryOneTimeTokenService {
        InMemoryOneTimeTokenService::new()
    }

    #[tokio::test]
    async fn generate_then_consume_authenticates_user() {
        let s = svc();
        let token = s.generate("alice").await;
        assert_eq!(token.username, "alice");
        assert_eq!(token.value.len(), 43);
        assert_eq!(s.consume(&token.value).await.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn token_is_single_use() {
        let s = svc();
        let token = s.generate("bob").await;
        assert_eq!(s.consume(&token.value).await.as_deref(), Some("bob"));
        // A replay fails.
        assert_eq!(s.consume(&token.value).await, None);
    }

    #[tokio::test]
    async fn unknown_token_is_rejected() {
        let s = svc();
        assert_eq!(s.consume("not-a-real-token").await, None);
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let s = svc().ttl_seconds(0); // expires immediately (now > expires_at)
        let token = s.generate("carol").await;
        // expires_at == now; a token is valid only while now <= expires_at, and
        // by the time we consume, the second has advanced — but to be robust we
        // assert the boundary by minting an already-past token directly.
        s.tokens
            .lock()
            .await
            .insert("past".into(), ("carol".into(), now_secs().saturating_sub(10)));
        assert_eq!(s.consume("past").await, None);
        // The 0-ttl token is also gone after one consume regardless.
        let _ = s.consume(&token.value).await;
    }

    fn router(captured: Arc<Mutex<Option<OneTimeToken>>>) -> (Router, Arc<dyn OneTimeTokenService>) {
        struct Capturing(Arc<Mutex<Option<OneTimeToken>>>);
        #[async_trait]
        impl OneTimeTokenGenerationSuccessHandler for Capturing {
            async fn handle(&self, token: &OneTimeToken) {
                *self.0.lock().await = Some(token.clone());
            }
        }
        let service: Arc<dyn OneTimeTokenService> = Arc::new(svc());
        let state = Arc::new(OttLoginState::new(
            service.clone(),
            Arc::new(Capturing(captured)),
        ));
        (ott_login_routes(state), service)
    }

    #[tokio::test]
    async fn generate_endpoint_delivers_token_without_leaking_it() {
        let captured = Arc::new(Mutex::new(None));
        let (app, _) = router(captured.clone());

        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/ott/generate")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from("username=dave"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // The response body must not contain the token value.
        let token = captured.lock().await.clone().expect("handler captured a token");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains(&token.value));
        assert_eq!(token.username, "dave");
    }

    #[tokio::test]
    async fn login_endpoint_consumes_token_and_sets_session_context() {
        let captured = Arc::new(Mutex::new(None));
        let (app, service) = router(captured);
        let token = service.generate("erin").await;

        let session = Session::new(SessionInner::new("sid"));
        let mut req = http::Request::builder()
            .uri(format!("/login/ott?token={}", token.value))
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut().insert(session.clone());

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers()[header::LOCATION], "/");

        // The security context is now stored on the session.
        let ctx = session
            .attribute::<String>(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .expect("security context stored");
        let auth: Authentication = serde_json::from_str(&ctx).unwrap();
        assert_eq!(auth.principal, "erin");

        // A replay of the same link fails (single-use), even with a session.
        let session2 = Session::new(SessionInner::new("sid2"));
        let mut req2 = http::Request::builder()
            .uri(format!("/login/ott?token={}", token.value))
            .body(axum::body::Body::empty())
            .unwrap();
        req2.extensions_mut().insert(session2);
        let resp2 = app.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::UNAUTHORIZED);
    }
}
