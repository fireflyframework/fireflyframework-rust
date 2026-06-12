//! OWASP security response headers — the Rust port of pyfly's
//! `SecurityHeadersConfig` + `SecurityHeadersFilter`
//! (`pyfly.web.security_headers` / `SecurityHeadersMiddleware`).
//!
//! The layer pre-encodes the configured `(HeaderName, HeaderValue)` pairs
//! once at construction and appends them to every response, exactly like
//! the pyfly filter pre-encodes its raw header list.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use futures::future::BoxFuture;
use http::{HeaderName, HeaderValue, Request};
use serde::Deserialize;
use tower::{Layer, Service};

/// Configuration for the security response headers, mirroring pyfly's
/// `SecurityHeadersConfig` field-for-field. All headers are enabled by
/// default following OWASP recommendations; the two `Option` headers are
/// omitted unless configured (they are too application-specific for a
/// safe default).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SecurityHeadersConfig {
    /// `X-Content-Type-Options` value. Default: `nosniff`.
    pub x_content_type_options: String,
    /// `X-Frame-Options` value. Default: `DENY`.
    pub x_frame_options: String,
    /// `Strict-Transport-Security` value.
    /// Default: `max-age=31536000; includeSubDomains`.
    pub strict_transport_security: String,
    /// `X-XSS-Protection` value. Default: `0` (modern browsers: disable
    /// the legacy XSS auditor).
    pub x_xss_protection: String,
    /// `Referrer-Policy` value. Default: `strict-origin-when-cross-origin`.
    pub referrer_policy: String,
    /// `Content-Security-Policy` value; `None` (default) omits the header.
    pub content_security_policy: Option<String>,
    /// `Permissions-Policy` value; `None` (default) omits the header.
    pub permissions_policy: Option<String>,
}

impl Default for SecurityHeadersConfig {
    fn default() -> Self {
        Self {
            x_content_type_options: "nosniff".to_string(),
            x_frame_options: "DENY".to_string(),
            strict_transport_security: "max-age=31536000; includeSubDomains".to_string(),
            x_xss_protection: "0".to_string(),
            referrer_policy: "strict-origin-when-cross-origin".to_string(),
            content_security_policy: None,
            permissions_policy: None,
        }
    }
}

/// Adds the configured security headers to every response — the Rust
/// analog of pyfly's `SecurityHeadersFilter` (order
/// `HIGHEST_PRECEDENCE + 300`). Header pairs are encoded once here, not
/// per request.
#[derive(Debug, Clone, Default)]
pub struct SecurityHeadersLayer {
    pairs: Arc<Vec<(HeaderName, HeaderValue)>>,
}

impl SecurityHeadersLayer {
    /// Builds the layer from `config`, pre-encoding every header pair.
    /// Invalid header values (non-ASCII) are skipped rather than
    /// panicking, since they can only come from user configuration.
    pub fn new(config: SecurityHeadersConfig) -> Self {
        let mut raw: Vec<(&'static str, &str)> = vec![
            ("x-content-type-options", &config.x_content_type_options),
            ("x-frame-options", &config.x_frame_options),
            (
                "strict-transport-security",
                &config.strict_transport_security,
            ),
            ("x-xss-protection", &config.x_xss_protection),
            ("referrer-policy", &config.referrer_policy),
        ];
        if let Some(csp) = config.content_security_policy.as_deref() {
            raw.push(("content-security-policy", csp));
        }
        if let Some(pp) = config.permissions_policy.as_deref() {
            raw.push(("permissions-policy", pp));
        }
        let pairs = raw
            .into_iter()
            .filter_map(|(name, value)| {
                Some((
                    HeaderName::from_static(name),
                    HeaderValue::from_str(value).ok()?,
                ))
            })
            .collect();
        Self {
            pairs: Arc::new(pairs),
        }
    }
}

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService {
            inner,
            pairs: Arc::clone(&self.pairs),
        }
    }
}

/// The tower service produced by [`SecurityHeadersLayer`].
#[derive(Debug, Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
    pairs: Arc<Vec<(HeaderName, HeaderValue)>>,
}

impl<S> Service<Request<Body>> for SecurityHeadersService<S>
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
        let pairs = Arc::clone(&self.pairs);
        Box::pin(async move {
            let mut res = inner.call(req).await?;
            for (name, value) in pairs.iter() {
                res.headers_mut().insert(name.clone(), value.clone());
            }
            Ok(res)
        })
    }
}
