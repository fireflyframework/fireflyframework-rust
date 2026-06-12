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

/// The method set Starlette expands `["*"]` into at construction
/// (`ALL_METHODS`), echoed verbatim on preflight — so a wildcard method
/// list never emits a literal `*` (invalid with credentials). Mirrors
/// `starlette.middleware.cors.ALL_METHODS`.
const ALL_METHODS: [&str; 7] = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

/// The CORS-safelisted request headers Starlette always merges into the
/// preflight allow-list (`SAFELISTED_HEADERS`), so an explicit
/// `allowed_headers` list still accepts `Accept`, `Accept-Language`,
/// `Content-Language`, and `Content-Type` (sent on ordinary JSON POSTs).
const SAFELISTED_HEADERS: [&str; 4] = [
    "Accept",
    "Accept-Language",
    "Content-Language",
    "Content-Type",
];

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

    /// The method list echoed in `Access-Control-Allow-Methods` — a
    /// wildcard `["*"]` expands to [`ALL_METHODS`], exactly as Starlette
    /// does at construction (so `*` is never emitted verbatim).
    fn effective_methods(&self) -> Vec<String> {
        if self.allow_all_methods() {
            ALL_METHODS.iter().map(ToString::to_string).collect()
        } else {
            self.allowed_methods.clone()
        }
    }

    /// The sorted union of the configured
    /// [`allowed_headers`](Self::allowed_headers) and the CORS-safelisted
    /// headers ([`SAFELISTED_HEADERS`]) — Starlette's
    /// `sorted(SAFELISTED_HEADERS | set(allow_headers))`, used to both
    /// validate requested headers and build `Access-Control-Allow-Headers`.
    fn effective_allowed_headers(&self) -> Vec<String> {
        let mut union: Vec<String> = SAFELISTED_HEADERS.iter().map(ToString::to_string).collect();
        for header in &self.allowed_headers {
            if !union.iter().any(|h| h.eq_ignore_ascii_case(header)) {
                union.push(header.clone());
            }
        }
        union.sort();
        union
    }

    fn header_allowed(&self, header: &str) -> bool {
        self.effective_allowed_headers()
            .iter()
            .any(|a| a.eq_ignore_ascii_case(header))
    }

    fn headers_allowed(&self, requested: &str) -> bool {
        if self.allow_all_headers() {
            return true;
        }
        requested.split(',').all(|h| {
            let h = h.trim();
            h.is_empty() || self.header_allowed(h)
        })
    }

    /// Starlette's `preflight_explicit_allow_origin`: the per-request
    /// origin is echoed (and `Vary: Origin` emitted) whenever origins are
    /// not a bare wildcard, or credentials are allowed.
    fn preflight_explicit_allow_origin(&self) -> bool {
        !self.allow_all_origins() || self.allow_credentials
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

fn plain_text(status: StatusCode, body: String) -> Response {
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
/// `CORSConfig`): the preflight CORS headers (`Vary`, `Allow-Methods`,
/// `Max-Age`, and — once validated — `Allow-Origin`/`Allow-Headers`) are
/// built up regardless of outcome; a disallowed origin/method/header
/// produces a `400` whose body is `"Disallowed CORS "` + the joined list
/// of offenders, carrying those same headers; success produces a `200`.
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

    // Seed the preflight headers exactly like Starlette's
    // `self.preflight_headers`, attached to BOTH the 200 and the 400.
    let mut res = plain_text(StatusCode::OK, "OK".to_string());
    let headers = res.headers_mut();
    if config.preflight_explicit_allow_origin() {
        // The origin value is filled in below if it is allowed.
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    } else {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    if let Ok(value) = HeaderValue::from_str(&config.effective_methods().join(", ")) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
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

    let mut failures: Vec<&str> = Vec::new();

    if config.origin_allowed(&origin) {
        if config.preflight_explicit_allow_origin() {
            // The "else" case (bare wildcard) already emitted `*` above.
            if let Ok(value) = HeaderValue::from_str(&origin) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
            }
        }
    } else {
        failures.push("origin");
    }

    if !config.method_allowed(&requested_method) {
        failures.push("method");
    }

    // When all headers are allowed, mirror the requested headers back;
    // otherwise validate each against the safelist ∪ allowed list.
    if config.allow_all_headers() {
        if let Some(requested) = requested_headers.as_deref() {
            if let Ok(value) = HeaderValue::from_str(requested) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
            }
        }
    } else {
        if let Some(requested) = requested_headers.as_deref() {
            if !config.headers_allowed(requested) {
                failures.push("headers");
            }
        }
        // Echo the safelist ∪ configured allow-list (Starlette emits its
        // precomputed `Access-Control-Allow-Headers` from
        // `preflight_headers`: `sorted(SAFELISTED_HEADERS | allow_headers)`).
        let allow_headers = config.effective_allowed_headers();
        if !allow_headers.is_empty() {
            if let Ok(value) = HeaderValue::from_str(&allow_headers.join(", ")) {
                headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
            }
        }
    }

    if !failures.is_empty() {
        let reason = format!("Disallowed CORS {}", failures.join(", "));
        *res.status_mut() = StatusCode::BAD_REQUEST;
        *res.body_mut() = Body::from(reason);
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
