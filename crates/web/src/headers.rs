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
    /// Emit `Strict-Transport-Security` even over plain HTTP. Default `false`,
    /// matching Spring's `HstsHeaderWriter`, which writes HSTS only on secure
    /// requests (sending HSTS over HTTP is meaningless and a deployment-config
    /// smell). Set `true` to force the header on every response.
    pub hsts_include_insecure: bool,
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
            hsts_include_insecure: false,
        }
    }
}

/// Adds the configured security headers to every response — the Rust
/// analog of pyfly's `SecurityHeadersFilter` (order
/// `HIGHEST_PRECEDENCE + 300`). Header pairs are encoded once here, not
/// per request.
#[derive(Debug, Clone, Default)]
pub struct SecurityHeadersLayer {
    /// Always-on headers (everything except HSTS).
    pairs: Arc<Vec<(HeaderName, HeaderValue)>>,
    /// The HSTS header, gated on a secure request unless `hsts_include_insecure`.
    hsts: Option<(HeaderName, HeaderValue)>,
    hsts_include_insecure: bool,
}

impl SecurityHeadersLayer {
    /// Builds the layer from `config`, pre-encoding every header pair.
    /// Invalid header values (non-ASCII) are skipped rather than
    /// panicking, since they can only come from user configuration.
    ///
    /// `Strict-Transport-Security` is held separately: it is emitted only on a
    /// secure request (Spring's `HstsHeaderWriter` default) unless
    /// [`SecurityHeadersConfig::hsts_include_insecure`] is set.
    pub fn new(config: SecurityHeadersConfig) -> Self {
        let mut raw: Vec<(&'static str, &str)> = vec![
            ("x-content-type-options", &config.x_content_type_options),
            ("x-frame-options", &config.x_frame_options),
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
        let hsts = if config.strict_transport_security.is_empty() {
            None
        } else {
            HeaderValue::from_str(&config.strict_transport_security)
                .ok()
                .map(|v| (HeaderName::from_static("strict-transport-security"), v))
        };
        Self {
            pairs: Arc::new(pairs),
            hsts,
            hsts_include_insecure: config.hsts_include_insecure,
        }
    }
}

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService {
            inner,
            pairs: Arc::clone(&self.pairs),
            hsts: self.hsts.clone(),
            hsts_include_insecure: self.hsts_include_insecure,
        }
    }
}

/// The tower service produced by [`SecurityHeadersLayer`].
#[derive(Debug, Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
    pairs: Arc<Vec<(HeaderName, HeaderValue)>>,
    hsts: Option<(HeaderName, HeaderValue)>,
    hsts_include_insecure: bool,
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
        let hsts = self.hsts.clone();
        let send_hsts = self.hsts_include_insecure || request_is_secure(&req);
        Box::pin(async move {
            let mut res = inner.call(req).await?;
            for (name, value) in pairs.iter() {
                res.headers_mut().insert(name.clone(), value.clone());
            }
            if send_hsts {
                if let Some((name, value)) = &hsts {
                    res.headers_mut().insert(name.clone(), value.clone());
                }
            }
            Ok(res)
        })
    }
}

/// Marker inserted into request extensions by [`serve`](crate::serve) when the
/// framework terminates TLS in-process (so `request_is_secure` recognises a
/// direct HTTPS connection, whose origin-form request URI carries no scheme and
/// whose connection sends no `X-Forwarded-Proto`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SecureRequest;

/// Whether the request arrived over a secure (HTTPS) channel — recognised three
/// ways: the in-process-TLS [`SecureRequest`] marker, a TLS-terminating proxy's
/// `X-Forwarded-Proto: https`, or an absolute-form `https` request URI. Shared
/// with the CSRF layer's `Secure`-cookie gating.
pub(crate) fn request_is_secure(req: &Request<Body>) -> bool {
    // In-process TLS termination (firefly's own `serve`) marks the request.
    if req.extensions().get::<SecureRequest>().is_some() {
        return true;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn hsts_header(config: SecurityHeadersConfig, forwarded_proto: Option<&str>) -> bool {
        let inner = tower::service_fn(|_req: Request<Body>| async {
            Ok::<Response, Infallible>(Response::new(Body::empty()))
        });
        let svc = SecurityHeadersLayer::new(config).layer(inner);
        let mut builder = Request::builder().uri("/x");
        if let Some(proto) = forwarded_proto {
            builder = builder.header("x-forwarded-proto", proto);
        }
        let resp = svc
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        resp.headers().contains_key("strict-transport-security")
    }

    // H9: by default HSTS is emitted only on secure requests (Spring's
    // HstsHeaderWriter), not over plain HTTP.
    #[tokio::test]
    async fn hsts_is_secure_only_by_default() {
        assert!(
            !hsts_header(SecurityHeadersConfig::default(), None).await,
            "HSTS must NOT be sent over plain HTTP by default"
        );
        assert!(
            hsts_header(SecurityHeadersConfig::default(), Some("https")).await,
            "HSTS must be sent over HTTPS (X-Forwarded-Proto)"
        );
    }

    // Review fix: an in-process-TLS request (SecureRequest marker, no
    // X-Forwarded-Proto, no URI scheme) is recognised as secure, so HSTS is
    // emitted — previously it was silently dropped on direct-HTTPS deployments.
    #[tokio::test]
    async fn hsts_present_for_in_app_tls_marker() {
        let inner = tower::service_fn(|_req: Request<Body>| async {
            Ok::<Response, Infallible>(Response::new(Body::empty()))
        });
        let svc = SecurityHeadersLayer::new(SecurityHeadersConfig::default()).layer(inner);
        let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
        req.extensions_mut().insert(SecureRequest);
        let resp = svc.oneshot(req).await.unwrap();
        assert!(resp.headers().contains_key("strict-transport-security"));
    }

    // H9: opt-in to always emit HSTS, even over plain HTTP.
    #[tokio::test]
    async fn hsts_can_be_forced_on_insecure() {
        let config = SecurityHeadersConfig {
            hsts_include_insecure: true,
            ..Default::default()
        };
        assert!(
            hsts_header(config, None).await,
            "HSTS must be sent over HTTP when include_insecure is set"
        );
    }

    // The other security headers are always present, on HTTP and HTTPS alike.
    #[tokio::test]
    async fn non_hsts_headers_always_present() {
        let inner = tower::service_fn(|_req: Request<Body>| async {
            Ok::<Response, Infallible>(Response::new(Body::empty()))
        });
        let svc = SecurityHeadersLayer::new(SecurityHeadersConfig::default()).layer(inner);
        let resp = svc
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(resp.headers().contains_key("x-content-type-options"));
        assert!(resp.headers().contains_key("x-frame-options"));
        assert!(resp.headers().contains_key("referrer-policy"));
    }
}
