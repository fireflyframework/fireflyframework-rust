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

//! HTTP Basic authentication — the Rust analog of Spring Security's
//! `httpBasic()` (`BasicAuthenticationFilter`).
//!
//! [`HttpBasicLayer`] reads `Authorization: Basic <base64(user:password)>`,
//! authenticates the credentials through the Tier 1
//! [`AuthenticationManager`](crate::AuthenticationManager) spine, and — on
//! success — stores the resulting [`Authentication`](crate::Authentication) on
//! the request extensions and scopes it as the ambient context (so
//! `#[pre_authorize]` / `FilterChain` / handlers see it), exactly as
//! [`BearerLayer`](crate::BearerLayer) does.
//!
//! * A **present, valid** header authenticates.
//! * A **present, invalid/malformed** header is rejected with `401` +
//!   `WWW-Authenticate: Basic` (via a
//!   [`BasicAuthenticationEntryPoint`](crate::BasicAuthenticationEntryPoint)).
//! * An **absent** header passes through untouched (Spring's
//!   `BasicAuthenticationFilter` behaviour) — a following session/bearer layer
//!   or the `FilterChain`'s deny-by-default then governs the request.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use http::header::AUTHORIZATION;
use http::HeaderMap;
use tower::{Layer, Service};

use crate::authentication_manager::{AuthenticationManager, AuthenticationRequest};
use crate::exception::{AuthenticationEntryPoint, BasicAuthenticationEntryPoint};

/// Tower [`Layer`] applying HTTP Basic authentication — Spring's `httpBasic()`.
#[derive(Clone)]
pub struct HttpBasicLayer {
    manager: Arc<dyn AuthenticationManager>,
    entry_point: Arc<dyn AuthenticationEntryPoint>,
}

impl HttpBasicLayer {
    /// Builds the layer over an [`AuthenticationManager`] (the Tier 1 spine),
    /// challenging with the default realm `"Realm"`.
    #[must_use]
    pub fn new(manager: Arc<dyn AuthenticationManager>) -> Self {
        Self {
            manager,
            entry_point: Arc::new(BasicAuthenticationEntryPoint::default()),
        }
    }

    /// Sets the Basic `realm` advertised in the `WWW-Authenticate` challenge.
    #[must_use]
    pub fn realm(mut self, realm: impl Into<String>) -> Self {
        self.entry_point = Arc::new(BasicAuthenticationEntryPoint::new(realm));
        self
    }

    /// Overrides the [`AuthenticationEntryPoint`] used to render the `401`.
    #[must_use]
    pub fn entry_point(mut self, entry_point: Arc<dyn AuthenticationEntryPoint>) -> Self {
        self.entry_point = entry_point;
        self
    }
}

impl std::fmt::Debug for HttpBasicLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpBasicLayer").finish_non_exhaustive()
    }
}

impl<S> Layer<S> for HttpBasicLayer {
    type Service = HttpBasicService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpBasicService {
            inner,
            manager: Arc::clone(&self.manager),
            entry_point: Arc::clone(&self.entry_point),
        }
    }
}

/// The tower service produced by [`HttpBasicLayer`].
#[derive(Clone)]
pub struct HttpBasicService<S> {
    inner: S,
    manager: Arc<dyn AuthenticationManager>,
    entry_point: Arc<dyn AuthenticationEntryPoint>,
}

impl<S> Service<Request> for HttpBasicService<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let manager = Arc::clone(&self.manager);
        let entry_point = Arc::clone(&self.entry_point);
        let creds = parse_basic(req.headers());

        Box::pin(async move {
            match creds {
                // No `Basic` header — pass through (Spring continues the chain).
                None => inner.call(req).await,
                // Present but undecodable / no colon — reject with a challenge.
                Some(Err(())) => Ok(entry_point.commence(&req, "Malformed Basic credentials")),
                Some(Ok((username, password))) => {
                    match manager
                        .authenticate(AuthenticationRequest::username_password(username, password))
                        .await
                    {
                        Ok(auth) => {
                            req.extensions_mut().insert(auth.clone());
                            // Scope the authentication for downstream method
                            // security / handlers, as BearerLayer does.
                            crate::with_authentication_scope(auth, inner.call(req)).await
                        }
                        Err(_) => Ok(entry_point.commence(&req, "Bad credentials")),
                    }
                }
            }
        })
    }
}

/// Parses an `Authorization: Basic <base64(user:password)>` header.
///
/// * `None` — no header, or a non-`Basic` scheme (pass through).
/// * `Some(Err(()))` — a `Basic` header that is not valid base64 / UTF-8, or
///   carries no `:` separator (malformed).
/// * `Some(Ok((user, password)))` — decoded credentials.
fn parse_basic(headers: &HeaderMap) -> Option<Result<(String, String), ()>> {
    let raw = headers.get(AUTHORIZATION)?.to_str().ok()?;
    // The scheme token is case-insensitive (RFC 7617).
    if !raw
        .get(..6)
        .is_some_and(|p| p.eq_ignore_ascii_case("Basic "))
    {
        return None;
    }
    let encoded = raw[6..].trim();
    let Ok(bytes) = STANDARD.decode(encoded) else {
        return Some(Err(()));
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Some(Err(()));
    };
    match text.split_once(':') {
        Some((user, password)) => Some(Ok((user.to_string(), password.to_string()))),
        None => Some(Err(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authentication_manager::ProviderManager;
    use crate::password::{BcryptPasswordEncoder, PasswordEncoder};
    use crate::userdetails::{DaoAuthenticationProvider, InMemoryUserDetailsService, UserDetails};
    use std::sync::Mutex;
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

    fn basic(user: &str, pass: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
    }

    async fn run(auth_header: Option<&str>) -> (Response, Option<String>) {
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let probe = seen.clone();
        let inner = tower::service_fn(move |_req: Request| {
            let probe = probe.clone();
            async move {
                *probe.lock().unwrap() = crate::current_authentication().map(|a| a.principal);
                Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
            }
        });
        let svc = HttpBasicLayer::new(manager()).realm("test").layer(inner);
        let mut builder = Request::builder().uri("/x");
        if let Some(h) = auth_header {
            builder = builder.header(AUTHORIZATION, h);
        }
        let resp = svc
            .oneshot(builder.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let principal = seen.lock().unwrap().clone();
        (resp, principal)
    }

    #[tokio::test]
    async fn valid_credentials_authenticate_and_scope() {
        let (resp, principal) = run(Some(&basic("alice", "pw"))).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(principal.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn invalid_credentials_get_a_basic_challenge() {
        let (resp, principal) = run(Some(&basic("alice", "wrong"))).await;
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
        let challenge = resp
            .headers()
            .get(http::header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(challenge.starts_with("Basic realm=\"test\""), "{challenge}");
        // The inner handler never ran.
        assert_eq!(principal, None);
    }

    #[tokio::test]
    async fn absent_header_passes_through() {
        let (resp, principal) = run(None).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(principal, None);
    }

    #[tokio::test]
    async fn malformed_header_is_challenged() {
        let (resp, _) = run(Some("Basic !!!not-base64!!!")).await;
        assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
        // A non-Basic scheme passes through, though.
        let (resp2, _) = run(Some("Bearer xyz")).await;
        assert_eq!(resp2.status(), http::StatusCode::OK);
    }
}
