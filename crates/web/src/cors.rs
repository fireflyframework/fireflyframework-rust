//! Cross-Origin Resource Sharing — the Rust port of pyfly's
//! `CORSConfig` + config-driven CORS middleware (`pyfly.web.cors`,
//! itself mirroring Spring's `CorsConfiguration`).
//!
//! The layer short-circuits preflight `OPTIONS` requests (an `Origin`
//! plus `Access-Control-Request-Method` header) and decorates simple
//! cross-origin responses, with the same semantics as the Starlette
//! `CORSMiddleware` pyfly delegates to:
//!
//! * wildcard origins without credentials reflect `*`;
//! * wildcard origins **with** credentials echo the request origin;
//! * explicit origins echo the origin and add `Vary: Origin`;
//! * a disallowed preflight is answered `400` with a plain-text reason.

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use futures::future::BoxFuture;
use http::{header, HeaderValue, Method, Request, StatusCode};
use serde::Deserialize;
use tower::{Layer, Service};

/// Spring's `CorsConfiguration.applyPermitDefaultValues()` method set,
/// applied by pyfly's `CORSConfig.from_config` when CORS is enabled but
/// no methods are configured explicitly.
pub const PERMIT_DEFAULT_METHODS: [&str; 3] = ["GET", "HEAD", "POST"];

fn default_origins() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_methods() -> Vec<String> {
    vec!["GET".to_string()]
}

fn default_headers() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_max_age() -> u64 {
    600
}

/// Configuration for Cross-Origin Resource Sharing, mirroring pyfly's
/// frozen `CORSConfig` dataclass (and Spring Boot's `CorsConfiguration`)
/// field-for-field, including defaults.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CorsConfig {
    /// Origins allowed to make cross-origin requests. Default: `["*"]`.
    #[serde(alias = "allowed-origins")]
    pub allowed_origins: Vec<String>,
    /// Methods allowed on cross-origin requests. Default: `["GET"]` —
    /// see [`PERMIT_DEFAULT_METHODS`] and [`CorsConfig::permit_defaults`]
    /// for the Spring permit-default set used by config binding.
    #[serde(alias = "allowed-methods")]
    pub allowed_methods: Vec<String>,
    /// Request headers allowed on preflight. Default: `["*"]` (echo the
    /// requested headers back).
    #[serde(alias = "allowed-headers")]
    pub allowed_headers: Vec<String>,
    /// Whether `Access-Control-Allow-Credentials: true` is sent.
    /// Default: `false`.
    #[serde(alias = "allow-credentials")]
    pub allow_credentials: bool,
    /// Response headers exposed to the browser. Default: empty.
    #[serde(alias = "exposed-headers")]
    pub exposed_headers: Vec<String>,
    /// Preflight cache lifetime in seconds. Default: `600`.
    #[serde(alias = "max-age")]
    pub max_age: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: default_origins(),
            allowed_methods: default_methods(),
            allowed_headers: default_headers(),
            allow_credentials: false,
            exposed_headers: Vec::new(),
            max_age: default_max_age(),
        }
    }
}

impl CorsConfig {
    /// Returns a config with Spring's permit-default method set
    /// (`GET`, `HEAD`, `POST`) instead of the bare `["GET"]` default —
    /// what pyfly's `CORSConfig.from_config` applies when CORS is
    /// enabled without an explicit method list.
    pub fn permit_defaults() -> Self {
        Self {
            allowed_methods: PERMIT_DEFAULT_METHODS
                .iter()
                .map(ToString::to_string)
                .collect(),
            ..Self::default()
        }
    }

    fn allow_all_origins(&self) -> bool {
        self.allowed_origins.iter().any(|o| o == "*")
    }

    fn allow_all_methods(&self) -> bool {
        self.allowed_methods.iter().any(|m| m == "*")
    }

    fn allow_all_headers(&self) -> bool {
        self.allowed_headers.iter().any(|h| h == "*")
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.allow_all_origins() || self.allowed_origins.iter().any(|o| o == origin)
    }

    fn method_allowed(&self, method: &str) -> bool {
        self.allow_all_methods()
            || self
                .allowed_methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method))
    }

    fn headers_allowed(&self, requested: &str) -> bool {
        if self.allow_all_headers() {
            return true;
        }
        requested.split(',').all(|h| {
            let h = h.trim();
            h.is_empty()
                || self
                    .allowed_headers
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(h))
        })
    }
}

/// CORS middleware — short-circuits preflight requests and decorates
/// cross-origin responses according to a [`CorsConfig`]. Requests
/// without an `Origin` header pass through untouched, as do responses
/// for origins not in the allow-list (the browser then blocks them).
#[derive(Debug, Clone)]
pub struct CorsLayer {
    config: Arc<CorsConfig>,
}

impl CorsLayer {
    /// Builds the layer from `config`.
    pub fn new(config: CorsConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl Default for CorsLayer {
    fn default() -> Self {
        Self::new(CorsConfig::default())
    }
}

impl<S> Layer<S> for CorsLayer {
    type Service = CorsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CorsService {
            inner,
            config: Arc::clone(&self.config),
        }
    }
}

/// The tower service produced by [`CorsLayer`].
#[derive(Debug, Clone)]
pub struct CorsService<S> {
    inner: S,
    config: Arc<CorsConfig>,
}

fn plain_text(status: StatusCode, body: &'static str) -> Response {
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    res
}

/// Builds the preflight short-circuit response, mirroring Starlette's
/// `CORSMiddleware.preflight_response` (which pyfly wires from
/// `CORSConfig`): a disallowed origin/method/header produces a 400 with
/// the offending part named; success produces a 200 with the
/// `Access-Control-Allow-*` set.
fn preflight_response(config: &CorsConfig, req: &Request<Body>) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let requested_method = req
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let requested_headers = req
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    let mut failures = Vec::new();
    if !config.origin_allowed(&origin) {
        failures.push("origin");
    }
    if !config.method_allowed(&requested_method) {
        failures.push("method");
    }
    if let Some(requested) = requested_headers.as_deref() {
        if !config.headers_allowed(requested) {
            failures.push("headers");
        }
    }
    if !failures.is_empty() {
        let reason: &'static str = match failures.as_slice() {
            ["origin"] => "Disallowed CORS origin",
            ["method"] => "Disallowed CORS method",
            ["headers"] => "Disallowed CORS headers",
            _ => "Disallowed CORS origin, method, headers",
        };
        return plain_text(StatusCode::BAD_REQUEST, reason);
    }

    let mut res = plain_text(StatusCode::OK, "OK");
    let headers = res.headers_mut();
    let allow_origin = if config.allow_all_origins() && !config.allow_credentials {
        HeaderValue::from_static("*")
    } else {
        HeaderValue::from_str(&origin).unwrap_or_else(|_| HeaderValue::from_static("*"))
    };
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
    if !config.allow_all_origins() {
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    if let Ok(value) = HeaderValue::from_str(&config.allowed_methods.join(", ")) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
    }
    let allow_headers = if config.allow_all_headers() {
        requested_headers.unwrap_or_default()
    } else {
        config.allowed_headers.join(", ")
    };
    if !allow_headers.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&allow_headers) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(&config.max_age.to_string()) {
        headers.insert(header::ACCESS_CONTROL_MAX_AGE, value);
    }
    if config.allow_credentials {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
    res
}

/// Decorates a simple (non-preflight) cross-origin response.
fn decorate_simple(config: &CorsConfig, origin: &str, res: &mut Response) {
    if !config.origin_allowed(origin) {
        return;
    }
    let headers = res.headers_mut();
    if config.allow_all_origins() && !config.allow_credentials {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    } else if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.append(header::VARY, HeaderValue::from_static("Origin"));
    }
    if config.allow_credentials {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
    if !config.exposed_headers.is_empty() {
        if let Ok(value) = HeaderValue::from_str(&config.exposed_headers.join(", ")) {
            headers.insert(header::ACCESS_CONTROL_EXPOSE_HEADERS, value);
        }
    }
}

impl<S> Service<Request<Body>> for CorsService<S>
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
        let config = Arc::clone(&self.config);
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(ToOwned::to_owned);
        let is_preflight = req.method() == Method::OPTIONS
            && origin.is_some()
            && req
                .headers()
                .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);

        if is_preflight {
            let res = preflight_response(&config, &req);
            return Box::pin(async move { Ok(res) });
        }

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let mut res = inner.call(req).await?;
            if let Some(origin) = origin {
                decorate_simple(&config, &origin, &mut res);
            }
            Ok(res)
        })
    }
}
