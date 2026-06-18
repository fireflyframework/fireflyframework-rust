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

//! CSRF token utilities — double-submit cookie pattern (pyfly:
//! `pyfly.security.csrf` + `CsrfFilter`).
//!
//! Provides cryptographically secure token generation, timing-safe
//! validation, and [`CsrfLayer`], a tower middleware implementing the
//! [double-submit cookie] strategy:
//!
//! * **Safe methods** (GET, HEAD, OPTIONS, TRACE) — pass through; the
//!   `XSRF-TOKEN` cookie is set (or rotated) on the response so that
//!   JavaScript can read it.
//! * **Unsafe methods** — the `XSRF-TOKEN` cookie is compared against
//!   the `X-XSRF-TOKEN` header with a timing-safe comparison; a
//!   mismatch or missing value yields `403` with the pyfly JSON body.
//! * **Bearer bypass** — requests carrying `Authorization: Bearer …`
//!   are stateless API calls and are exempt.
//!
//! [double-submit cookie]:
//!     https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html#double-submit-cookie

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use http::{header, HeaderValue, Method, StatusCode};
use rand::RngCore;
use tower::{Layer, Service};

/// Name of the cookie that carries the CSRF token.
pub const CSRF_COOKIE_NAME: &str = "XSRF-TOKEN";

/// Name of the request header that carries the CSRF token.
pub const CSRF_HEADER_NAME: &str = "X-XSRF-TOKEN";

/// HTTP methods that do not require CSRF validation.
pub const SAFE_METHODS: [&str; 4] = ["GET", "HEAD", "OPTIONS", "TRACE"];

/// Reports whether `method` is CSRF-safe (GET/HEAD/OPTIONS/TRACE).
pub fn is_safe_method(method: &Method) -> bool {
    SAFE_METHODS.iter().any(|m| *m == method.as_str())
}

/// Generates `n` cryptographically secure random bytes encoded as
/// URL-safe unpadded base64 — the Rust twin of Python's
/// `secrets.token_urlsafe(n)`.
pub(crate) fn random_urlsafe(nbytes: usize) -> String {
    let mut bytes = vec![0u8; nbytes];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generates a cryptographically secure CSRF token: a URL-safe
/// base64-encoded random string (43 characters), exactly pyfly's
/// `secrets.token_urlsafe(32)`.
pub fn generate_csrf_token() -> String {
    random_urlsafe(32)
}

/// Compares two byte strings in constant time (for equal lengths).
/// Length mismatches return `false` immediately — the same length
/// leak Python's `secrets.compare_digest` exhibits for `str` inputs.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Validates a CSRF token pair using a timing-safe comparison: `true`
/// iff the `XSRF-TOKEN` cookie value equals the `X-XSRF-TOKEN` header
/// value.
pub fn validate_csrf_token(cookie_token: &str, header_token: &str) -> bool {
    constant_time_eq(cookie_token.as_bytes(), header_token.as_bytes())
}

/// Extracts a cookie value from the request's `Cookie` headers.
fn cookie_value(req: &Request, name: &str) -> Option<String> {
    for header in req.headers().get_all(header::COOKIE) {
        let Ok(raw) = header.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// The pyfly `CsrfFilter` 403 body: `{"error": "..."}` as
/// `application/json`.
fn csrf_forbidden(message: &str) -> Response {
    let body = serde_json::json!({ "error": message }).to_string();
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("static csrf response must build")
}

/// Policy for the `Secure` attribute on the CSRF cookie. Spring's
/// `CookieCsrfTokenRepository` only marks the cookie `Secure` on a secure
/// request; a `Secure` cookie over plain HTTP is silently dropped, breaking the
/// double-submit pair. [`Auto`](CookieSecure::Auto) reproduces that behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CookieSecure {
    /// Mark `Secure` only when the request arrived over HTTPS (default).
    #[default]
    Auto,
    /// Always mark the cookie `Secure`.
    Always,
    /// Never mark the cookie `Secure` (plain-HTTP dev only).
    Never,
}

impl CookieSecure {
    fn applies(self, req: &Request) -> bool {
        match self {
            CookieSecure::Auto => request_is_secure(req),
            CookieSecure::Always => true,
            CookieSecure::Never => false,
        }
    }
}

/// Whether the request arrived over HTTPS — directly or via a TLS-terminating
/// proxy that set `X-Forwarded-Proto: https`.
fn request_is_secure(req: &Request) -> bool {
    if let Some(proto) = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
    {
        if proto
            .split(',')
            .next()
            .map(|s| s.trim().eq_ignore_ascii_case("https"))
            .unwrap_or(false)
        {
            return true;
        }
    }
    req.uri().scheme_str() == Some("https")
}

/// Sets (or rotates) the CSRF cookie on `resp` — readable by JS
/// (`HttpOnly` off), `SameSite=Lax`, path `/`, and `Secure` only when
/// `secure` (so the double-submit pair also works over plain-HTTP dev).
fn set_csrf_cookie(resp: &mut Response, token: &str, secure: bool) {
    let mut cookie = format!("{CSRF_COOKIE_NAME}={token}; Path=/; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().append(header::SET_COOKIE, value);
    }
}

/// `CsrfLayer` applies double-submit cookie CSRF protection to every
/// request — the Rust analog of pyfly's `CsrfFilter`.
///
/// ```rust
/// use axum::{routing::post, Router};
/// use firefly_security::CsrfLayer;
///
/// let app: Router = Router::new()
///     .route("/orders", post(|| async { "created" }))
///     .layer(CsrfLayer::new());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CsrfLayer {
    cookie_secure: CookieSecure,
}

impl CsrfLayer {
    /// Returns the CSRF protection layer with the request-driven
    /// [`CookieSecure::Auto`] cookie policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the `Secure`-attribute policy for the CSRF cookie
    /// (default [`CookieSecure::Auto`]).
    pub fn cookie_secure(mut self, policy: CookieSecure) -> Self {
        self.cookie_secure = policy;
        self
    }
}

impl<S> Layer<S> for CsrfLayer {
    type Service = CsrfService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CsrfService {
            inner,
            cookie_secure: self.cookie_secure,
        }
    }
}

/// The tower service produced by [`CsrfLayer`].
#[derive(Clone)]
pub struct CsrfService<S> {
    inner: S,
    cookie_secure: CookieSecure,
}

impl<S> Service<Request> for CsrfService<S>
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

    fn call(&mut self, req: Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let cookie_secure = self.cookie_secure;

        Box::pin(async move {
            let secure = cookie_secure.applies(&req);
            // Safe methods — pass through and set/refresh the cookie.
            if is_safe_method(req.method()) {
                let mut resp = inner.call(req).await?;
                set_csrf_cookie(&mut resp, &generate_csrf_token(), secure);
                return Ok(resp);
            }

            // Bearer bypass — JWT API clients don't need CSRF.
            let bearer = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("Bearer "));
            if bearer {
                return inner.call(req).await;
            }

            // Unsafe methods — validate the double-submit pair.
            let cookie_token = cookie_value(&req, CSRF_COOKIE_NAME);
            let header_token = req
                .headers()
                .get(CSRF_HEADER_NAME)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let (Some(cookie_token), Some(header_token)) = (cookie_token, header_token) else {
                return Ok(csrf_forbidden("CSRF token missing"));
            };
            if !validate_csrf_token(&cookie_token, &header_token) {
                return Ok(csrf_forbidden("CSRF token invalid"));
            }

            // Valid — proceed and rotate the token.
            let mut resp = inner.call(req).await?;
            set_csrf_cookie(&mut resp, &generate_csrf_token(), secure);
            Ok(resp)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ported from pyfly: TestCsrfTokenUtilities
    #[test]
    fn generate_csrf_token_is_43_chars_urlsafe() {
        let token = generate_csrf_token();
        assert_eq!(token.len(), 43);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn validate_csrf_token_matching() {
        let token = generate_csrf_token();
        assert!(validate_csrf_token(&token, &token));
    }

    #[test]
    fn validate_csrf_token_mismatch() {
        let a = generate_csrf_token();
        let b = generate_csrf_token();
        assert!(!validate_csrf_token(&a, &b));
        assert!(!validate_csrf_token(&a, ""));
        assert!(!validate_csrf_token(&a, &a[..42]));
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn safe_methods_match_pyfly() {
        for m in [Method::GET, Method::HEAD, Method::OPTIONS, Method::TRACE] {
            assert!(is_safe_method(&m), "{m}");
        }
        for m in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            assert!(!is_safe_method(&m), "{m}");
        }
    }

    #[test]
    fn cookie_value_parses_multiple_pairs() {
        let req = Request::builder()
            .uri("/x")
            .header(header::COOKIE, "a=1; XSRF-TOKEN=tok; b=2")
            .body(Body::empty())
            .unwrap();
        assert_eq!(cookie_value(&req, CSRF_COOKIE_NAME).as_deref(), Some("tok"));
        assert_eq!(cookie_value(&req, "missing"), None);
    }

    // H4: the Secure attribute follows the request scheme under the default
    // Auto policy, and can be forced or disabled.
    #[tokio::test]
    async fn cookie_secure_follows_scheme_and_policy() {
        use tower::ServiceExt;

        async fn cookie(layer: CsrfLayer, proto: Option<&str>) -> String {
            let inner = tower::service_fn(|_r: Request| async {
                Ok::<Response, Infallible>(Response::new(Body::empty()))
            });
            let svc = layer.layer(inner);
            let mut b = Request::builder().method(Method::GET).uri("/p");
            if let Some(p) = proto {
                b = b.header("x-forwarded-proto", p);
            }
            let resp = svc.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
            resp.headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }

        assert!(!cookie(CsrfLayer::new(), None).await.contains("Secure"));
        assert!(cookie(CsrfLayer::new(), Some("https")).await.contains("Secure"));
        assert!(cookie(CsrfLayer::new().cookie_secure(CookieSecure::Always), None)
            .await
            .contains("Secure"));
        assert!(!cookie(CsrfLayer::new().cookie_secure(CookieSecure::Never), Some("https"))
            .await
            .contains("Secure"));
    }
}
