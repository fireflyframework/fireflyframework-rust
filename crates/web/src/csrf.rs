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

//! Double-submit-cookie CSRF protection — the Rust port of pyfly's
//! `CsrfFilter` (`pyfly.web.adapters.starlette.filters.csrf_filter`) and
//! the `pyfly.security.csrf` token helpers.
//!
//! * **Safe methods** (GET, HEAD, OPTIONS, TRACE) pass through and the
//!   `XSRF-TOKEN` cookie is set (or refreshed) on the response so that
//!   JavaScript can read it.
//! * **Unsafe methods** compare the `XSRF-TOKEN` cookie against the
//!   `X-XSRF-TOKEN` request header using a timing-safe comparison
//!   (SHA-256 digests are compared instead of the raw strings, so the
//!   comparison time reveals nothing about the token). A missing or
//!   mismatched pair is answered `403` `application/problem+json`.
//! * **Bearer bypass** — requests carrying `Authorization: Bearer …`
//!   are stateless API clients and exempt from CSRF validation.
//! * A successful unsafe request **rotates** the token.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use firefly_kernel::ProblemDetail;
use futures::future::BoxFuture;
use http::{header, HeaderValue, Method, Request};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tower::{Layer, Service};

use crate::globs::matches_any;
use crate::headers::request_is_secure;
use crate::problem::problem_response;

/// Policy for the `Secure` attribute on the CSRF cookie.
///
/// Spring's `CookieCsrfTokenRepository` marks the cookie `Secure` only when the
/// request is itself secure; sending `Secure` over plain HTTP makes the browser
/// drop the cookie, so the double-submit pair can never be established (every
/// unsafe request then 403s). [`Auto`](CookieSecure::Auto) reproduces Spring's
/// request-driven behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CookieSecure {
    /// Mark `Secure` only when the request arrived over HTTPS (default).
    #[default]
    Auto,
    /// Always mark the cookie `Secure` (HTTPS-only deployments).
    Always,
    /// Never mark the cookie `Secure` (plain-HTTP dev only).
    Never,
}

impl CookieSecure {
    /// Resolves whether the cookie should carry `Secure` for `req`.
    fn applies(self, req: &Request<Body>) -> bool {
        match self {
            CookieSecure::Auto => request_is_secure(req),
            CookieSecure::Always => true,
            CookieSecure::Never => false,
        }
    }
}

/// Name of the cookie that carries the CSRF token — identical across
/// the Java, .NET, Go, and Python ports (Angular convention).
pub const CSRF_COOKIE_NAME: &str = "XSRF-TOKEN";

/// Name of the request header that carries the CSRF token.
pub const CSRF_HEADER_NAME: &str = "X-XSRF-TOKEN";

/// HTTP methods that do not require CSRF validation.
pub const CSRF_SAFE_METHODS: [&str; 4] = ["GET", "HEAD", "OPTIONS", "TRACE"];

/// Generates a cryptographically-secure CSRF token: 32 random bytes,
/// URL-safe base64 without padding — 43 characters, byte-compatible
/// with Python's `secrets.token_urlsafe(32)`.
pub fn generate_csrf_token() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Validates a CSRF token pair using a timing-safe comparison — the
/// Rust analog of Python's `secrets.compare_digest`. The SHA-256
/// digests of both values are compared, so equality testing takes the
/// same time regardless of where the first differing byte sits.
pub fn validate_csrf_token(cookie_token: &str, header_token: &str) -> bool {
    let a = Sha256::digest(cookie_token.as_bytes());
    let b = Sha256::digest(header_token.as_bytes());
    a == b
}

fn default_exclude_patterns() -> Vec<String> {
    ["/actuator/*", "/health", "/ready"]
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// Double-submit-cookie CSRF middleware — see the module docs for the
/// exact pyfly-parity semantics. Excluded paths (default: `/actuator/*`,
/// `/health`, `/ready`) bypass the filter entirely.
#[derive(Debug, Clone)]
pub struct CsrfLayer {
    exclude_patterns: Arc<Vec<String>>,
    cookie_secure: CookieSecure,
}

impl CsrfLayer {
    /// Returns the layer with pyfly's default exclude patterns and the
    /// request-driven [`CookieSecure::Auto`] cookie policy.
    pub fn new() -> Self {
        Self {
            exclude_patterns: Arc::new(default_exclude_patterns()),
            cookie_secure: CookieSecure::Auto,
        }
    }

    /// Replaces the exclude patterns (shell-style globs; a matching
    /// request path bypasses CSRF entirely).
    pub fn with_exclude_patterns<I, P>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        self.exclude_patterns = Arc::new(patterns.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the `Secure`-attribute policy for the CSRF cookie
    /// (default [`CookieSecure::Auto`]).
    pub fn cookie_secure(mut self, policy: CookieSecure) -> Self {
        self.cookie_secure = policy;
        self
    }
}

impl Default for CsrfLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for CsrfLayer {
    type Service = CsrfService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CsrfService {
            inner,
            exclude_patterns: Arc::clone(&self.exclude_patterns),
            cookie_secure: self.cookie_secure,
        }
    }
}

/// The tower service produced by [`CsrfLayer`].
#[derive(Debug, Clone)]
pub struct CsrfService<S> {
    inner: S,
    exclude_patterns: Arc<Vec<String>>,
    cookie_secure: CookieSecure,
}

/// Extracts the value of `name` from the request's `Cookie` header(s).
fn cookie_value(req: &Request<Body>, name: &str) -> Option<String> {
    for header_value in req.headers().get_all(header::COOKIE) {
        let raw = header_value.to_str().ok()?;
        for pair in raw.split(';') {
            let mut parts = pair.trim().splitn(2, '=');
            if parts.next() == Some(name) {
                return Some(parts.next().unwrap_or_default().to_string());
            }
        }
    }
    None
}

/// Appends the `XSRF-TOKEN` cookie — readable by JavaScript (no
/// `HttpOnly`), `SameSite=Lax`, path `/`, and `Secure` only when `secure`
/// (so the double-submit pair also works over plain-HTTP development; see
/// [`CookieSecure`]).
fn set_csrf_cookie(res: &mut Response, token: &str, secure: bool) {
    let mut cookie = format!("{CSRF_COOKIE_NAME}={token}; Path=/; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        res.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn forbidden(detail: &str) -> Response {
    problem_response(&ProblemDetail::forbidden(detail))
}

impl<S> Service<Request<Body>> for CsrfService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        // Excluded paths bypass the filter entirely.
        if matches_any(&self.exclude_patterns, req.uri().path()) {
            return Box::pin(async move { inner.call(req).await });
        }

        let method = req.method().clone();
        // Resolve the `Secure` cookie attribute from the request (before `req`
        // is moved into the response future).
        let secure = self.cookie_secure.applies(&req);

        // Safe methods — pass through and set/refresh the CSRF cookie.
        if matches!(
            method,
            Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
        ) {
            return Box::pin(async move {
                let mut res = inner.call(req).await?;
                set_csrf_cookie(&mut res, &generate_csrf_token(), secure);
                Ok(res)
            });
        }

        // Bearer bypass — JWT API clients don't need CSRF.
        let bearer = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer "));
        if bearer {
            return Box::pin(async move { inner.call(req).await });
        }

        // Unsafe methods — validate the double-submit cookie.
        let cookie_token = cookie_value(&req, CSRF_COOKIE_NAME).filter(|t| !t.is_empty());
        let header_token = req
            .headers()
            .get(CSRF_HEADER_NAME)
            .and_then(|v| v.to_str().ok())
            .filter(|t| !t.is_empty())
            .map(ToOwned::to_owned);

        Box::pin(async move {
            let (Some(cookie_token), Some(header_token)) = (cookie_token, header_token) else {
                return Ok(forbidden("CSRF token missing"));
            };
            if !validate_csrf_token(&cookie_token, &header_token) {
                return Ok(forbidden("CSRF token invalid"));
            }
            // Valid — proceed and rotate the token.
            let mut res = inner.call(req).await?;
            set_csrf_cookie(&mut res, &generate_csrf_token(), secure);
            Ok(res)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn set_cookie_for(layer: CsrfLayer, forwarded_proto: Option<&str>) -> String {
        let inner = tower::service_fn(|_r: Request<Body>| async {
            Ok::<Response, Infallible>(Response::new(Body::empty()))
        });
        let svc = layer.layer(inner);
        let mut b = Request::builder().method(Method::GET).uri("/page");
        if let Some(p) = forwarded_proto {
            b = b.header("x-forwarded-proto", p);
        }
        let resp = svc.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        resp.headers()
            .get(header::SET_COOKIE)
            .expect("XSRF cookie set on safe request")
            .to_str()
            .unwrap()
            .to_string()
    }

    // H4: Auto policy follows the request scheme — no `Secure` over plain HTTP
    // (so the double-submit pair works in dev), `Secure` over HTTPS.
    #[tokio::test]
    async fn cookie_secure_auto_follows_request_scheme() {
        let http = set_cookie_for(CsrfLayer::new(), None).await;
        assert!(http.contains("XSRF-TOKEN="), "{http}");
        assert!(!http.contains("Secure"), "HTTP cookie must not be Secure: {http}");

        let https = set_cookie_for(CsrfLayer::new(), Some("https")).await;
        assert!(https.contains("Secure"), "HTTPS cookie must be Secure: {https}");
    }

    // H4: Always/Never override the request scheme.
    #[tokio::test]
    async fn cookie_secure_always_and_never_override() {
        let always = set_cookie_for(CsrfLayer::new().cookie_secure(CookieSecure::Always), None).await;
        assert!(always.contains("Secure"), "{always}");

        let never =
            set_cookie_for(CsrfLayer::new().cookie_secure(CookieSecure::Never), Some("https")).await;
        assert!(!never.contains("Secure"), "{never}");
    }
}
