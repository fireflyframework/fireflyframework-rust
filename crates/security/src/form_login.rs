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

//! Form login — the Rust analog of Spring Security's `formLogin()`
//! (`UsernamePasswordAuthenticationFilter`).
//!
//! [`form_login_routes`] mounts a `POST /login` endpoint that takes a
//! url-encoded `username` + `password`, authenticates them through the Tier 1
//! [`AuthenticationManager`](crate::AuthenticationManager) spine, and — on
//! success — rotates the session id (anti-fixation), persists the
//! [`Authentication`](crate::Authentication) through a
//! [`SecurityContextRepository`](crate::SecurityContextRepository) (so
//! [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer) restores it
//! on later requests), and redirects to the success URL. A failed login
//! redirects to the failure URL. Both URLs are configurable, and the success/
//! failure rendering can be swapped via [`FormLoginSuccessHandler`] /
//! [`FormLoginFailureHandler`].

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Form, State};
use axum::response::Response;
use axum::{Extension, Router};
use http::{header, StatusCode};
use serde::Deserialize;

use firefly_session::Session;

use crate::authentication::{Authentication, SecurityError};
use crate::authentication_manager::{AuthenticationManager, AuthenticationRequest};
use crate::security_context::{HttpSessionSecurityContextRepository, SecurityContextRepository};

/// Renders the response after a **successful** form login — Spring's
/// `AuthenticationSuccessHandler`.
#[async_trait]
pub trait FormLoginSuccessHandler: Send + Sync {
    /// Builds the success response (default: a 302 redirect).
    async fn on_success(&self, auth: &Authentication) -> Response;
}

/// Renders the response after a **failed** form login — Spring's
/// `AuthenticationFailureHandler`.
#[async_trait]
pub trait FormLoginFailureHandler: Send + Sync {
    /// Builds the failure response (default: a 302 redirect).
    async fn on_failure(&self, error: &SecurityError) -> Response;
}

/// Default success handler: 302 redirect to a fixed URL.
struct RedirectSuccess(String);
#[async_trait]
impl FormLoginSuccessHandler for RedirectSuccess {
    async fn on_success(&self, _auth: &Authentication) -> Response {
        redirect(&self.0)
    }
}

/// Default failure handler: 302 redirect to a fixed URL.
struct RedirectFailure(String);
#[async_trait]
impl FormLoginFailureHandler for RedirectFailure {
    async fn on_failure(&self, _error: &SecurityError) -> Response {
        redirect(&self.0)
    }
}

/// Shared state for the form-login route.
pub struct FormLoginState {
    manager: Arc<dyn AuthenticationManager>,
    repository: Arc<dyn SecurityContextRepository>,
    success: Arc<dyn FormLoginSuccessHandler>,
    failure: Arc<dyn FormLoginFailureHandler>,
}

impl FormLoginState {
    /// Builds the state over the Tier 1 [`AuthenticationManager`], persisting
    /// the context with the default
    /// [`HttpSessionSecurityContextRepository`], redirecting to `"/"` on
    /// success and `"/login?error"` on failure (Spring's defaults).
    #[must_use]
    pub fn new(manager: Arc<dyn AuthenticationManager>) -> Self {
        Self {
            manager,
            repository: Arc::new(HttpSessionSecurityContextRepository::new()),
            success: Arc::new(RedirectSuccess("/".to_string())),
            failure: Arc::new(RedirectFailure("/login?error".to_string())),
        }
    }

    /// Sets the post-login success redirect target.
    #[must_use]
    pub fn success_url(mut self, url: impl Into<String>) -> Self {
        self.success = Arc::new(RedirectSuccess(url.into()));
        self
    }

    /// Sets the failure redirect target.
    #[must_use]
    pub fn failure_url(mut self, url: impl Into<String>) -> Self {
        self.failure = Arc::new(RedirectFailure(url.into()));
        self
    }

    /// Overrides the [`SecurityContextRepository`] used to persist the context.
    #[must_use]
    pub fn repository(mut self, repository: Arc<dyn SecurityContextRepository>) -> Self {
        self.repository = repository;
        self
    }

    /// Overrides the success handler.
    #[must_use]
    pub fn success_handler(mut self, handler: Arc<dyn FormLoginSuccessHandler>) -> Self {
        self.success = handler;
        self
    }

    /// Overrides the failure handler.
    #[must_use]
    pub fn failure_handler(mut self, handler: Arc<dyn FormLoginFailureHandler>) -> Self {
        self.failure = handler;
        self
    }
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

/// `POST /login` — authenticate username/password, establish the session
/// security context (rotating the id), and hand off to the success/failure
/// handler.
async fn handle_login(
    State(state): State<Arc<FormLoginState>>,
    Extension(session): Extension<Session>,
    Form(form): Form<LoginForm>,
) -> Response {
    match state
        .manager
        .authenticate(AuthenticationRequest::username_password(
            form.username,
            form.password,
        ))
        .await
    {
        Ok(auth) => {
            // Anti-fixation: rotate the session id on authentication, then
            // persist the context where SessionAuthenticationLayer restores it.
            session.rotate_id().await;
            state.repository.save(&session, &auth).await;
            state.success.on_success(&auth).await
        }
        Err(error) => state.failure.on_failure(&error).await,
    }
}

/// Builds the form-login route (`POST /login`). Mount behind a
/// [`firefly_session::SessionLayer`]; pair with a
/// [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer) to restore
/// the established context on subsequent requests.
pub fn form_login_routes(state: Arc<FormLoginState>) -> Router {
    Router::new()
        .route("/login", axum::routing::post(handle_login))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authentication_manager::ProviderManager;
    use crate::oauth2::SESSION_KEY_SECURITY_CONTEXT;
    use crate::password::{BcryptPasswordEncoder, PasswordEncoder};
    use crate::userdetails::{DaoAuthenticationProvider, InMemoryUserDetailsService, UserDetails};
    use firefly_session::SessionInner;
    use tower::ServiceExt;

    fn manager() -> Arc<dyn AuthenticationManager> {
        let hash = BcryptPasswordEncoder::with_rounds(4).hash("pw").unwrap();
        let uds = Arc::new(
            InMemoryUserDetailsService::new().with_user(UserDetails::new(
                "alice",
                hash,
                vec!["USER".into()],
            )),
        );
        let provider = Arc::new(DaoAuthenticationProvider::new(
            uds,
            Arc::new(BcryptPasswordEncoder::with_rounds(4)),
        ));
        Arc::new(ProviderManager::new(vec![provider]))
    }

    async fn post_login(body: &str) -> (Response, Session) {
        let state = Arc::new(FormLoginState::new(manager()).success_url("/home"));
        let app = form_login_routes(state);
        let session = Session::new(SessionInner::new("sid"));
        let mut req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        req.extensions_mut().insert(session.clone());
        let resp = app.oneshot(req).await.unwrap();
        (resp, session)
    }

    #[tokio::test]
    async fn valid_login_sets_context_and_rotates_session() {
        let session_id_before;
        let (resp, session) = {
            // Build the session first so we can read its id before/after.
            let state = Arc::new(FormLoginState::new(manager()).success_url("/home"));
            let app = form_login_routes(state);
            let session = Session::new(SessionInner::new("sid"));
            session_id_before = session.id().await;
            let mut req = http::Request::builder()
                .method(http::Method::POST)
                .uri("/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(axum::body::Body::from("username=alice&password=pw"))
                .unwrap();
            req.extensions_mut().insert(session.clone());
            (app.oneshot(req).await.unwrap(), session)
        };

        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers()[header::LOCATION], "/home");
        // Session id rotated (anti-fixation).
        assert_ne!(session.id().await, session_id_before);
        // Security context persisted.
        let ctx = session
            .attribute::<String>(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .expect("context stored");
        let auth: Authentication = serde_json::from_str(&ctx).unwrap();
        assert_eq!(auth.principal, "alice");
    }

    #[tokio::test]
    async fn invalid_login_redirects_to_failure_without_context() {
        let (resp, session) = post_login("username=alice&password=wrong").await;
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert_eq!(resp.headers()[header::LOCATION], "/login?error");
        assert!(session
            .attribute::<String>(SESSION_KEY_SECURITY_CONTEXT)
            .await
            .is_none());
    }
}
