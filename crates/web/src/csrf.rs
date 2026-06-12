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
use crate::problem::problem_response;

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
}

impl CsrfLayer {
    /// Returns the layer with pyfly's default exclude patterns.
    pub fn new() -> Self {
        Self {
            exclude_patterns: Arc::new(default_exclude_patterns()),
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
        }
    }
}

/// The tower service produced by [`CsrfLayer`].
#[derive(Debug, Clone)]
pub struct CsrfService<S> {
    inner: S,
    exclude_patterns: Arc<Vec<String>>,
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
/// `HttpOnly`), `SameSite=Lax`, `Secure`, path `/` — matching pyfly's
/// `_set_csrf_cookie` attribute-for-attribute.
fn set_csrf_cookie(res: &mut Response, token: &str) {
    let cookie = format!("{CSRF_COOKIE_NAME}={token}; Path=/; SameSite=Lax; Secure");
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

        // Safe methods — pass through and set/refresh the CSRF cookie.
        if matches!(
            method,
            Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
        ) {
            return Box::pin(async move {
                let mut res = inner.call(req).await?;
                set_csrf_cookie(&mut res, &generate_csrf_token());
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
            set_csrf_cookie(&mut res, &generate_csrf_token());
            Ok(res)
        })
    }
}
