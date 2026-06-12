//! Micrometer-parity HTTP server metrics middleware — the Rust port of
//! pyfly's `MetricsFilter` (Spring Boot's `WebMvcMetricsFilter`).
//!
//! The layer times every request and reports one [`RequestMetric`] per
//! response to a pluggable [`RequestObserver`], carrying the exact
//! Micrometer tag set:
//!
//! * `method`  — HTTP method (`GET`, `POST`, …);
//! * `uri`     — the **templated** route (`/users/{id}`, from axum's
//!   `MatchedPath`) — never the raw path, which would explode label
//!   cardinality; unmatched 404s report `NOT_FOUND`, 3xx report
//!   `REDIRECTION`;
//! * `status`  — numeric HTTP status (`200`);
//! * `outcome` — [`Outcome`] (`SUCCESS` / `CLIENT_ERROR` / …);
//! * `exception` — `"panic"` for a panicking handler, absent otherwise
//!   (pyfly reports the raised exception class; Rust handlers signal
//!   errors through status codes, so only panics map to the tag).
//!
//! It also maintains Micrometer's time-windowed maximum per tag set
//! ([`RollingMax`], two windows of `step` seconds), reported as
//! [`RequestMetric::rolling_max_seconds`] for the companion
//! `http_server_requests_seconds_max` gauge.
//!
//! The observer is a **local trait on purpose** — `firefly-web` does not
//! depend on `firefly-actuator`; `firefly-starter-core` bridges the two
//! by implementing [`RequestObserver`] over the actuator
//! `MetricRegistry`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::panic::{resume_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::extract::MatchedPath;
use axum::response::Response;
use futures::future::BoxFuture;
use futures::FutureExt;
use http::Request;
use tower::{Layer, Service};

use crate::globs::matches_any;

/// Micrometer meter name in Prometheus exposition form (base unit
/// seconds) — `http.server.requests` as Spring Boot publishes it.
pub const HTTP_SERVER_REQUESTS_METRIC: &str = "http_server_requests_seconds";

/// The companion time-windowed maximum gauge name.
pub const HTTP_SERVER_REQUESTS_MAX_METRIC: &str = "http_server_requests_seconds_max";

/// Micrometer's `outcome` tag value, derived from the HTTP status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// 1xx.
    Informational,
    /// 2xx.
    Success,
    /// 3xx.
    Redirection,
    /// 4xx.
    ClientError,
    /// 5xx.
    ServerError,
    /// Anything else.
    Unknown,
}

impl Outcome {
    /// Maps an HTTP status code to its Micrometer `outcome` tag value.
    pub fn from_status(status: u16) -> Self {
        match status {
            100..=199 => Outcome::Informational,
            200..=299 => Outcome::Success,
            300..=399 => Outcome::Redirection,
            400..=499 => Outcome::ClientError,
            500..=599 => Outcome::ServerError,
            _ => Outcome::Unknown,
        }
    }

    /// The Micrometer tag spelling (`SUCCESS`, `CLIENT_ERROR`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Informational => "INFORMATIONAL",
            Outcome::Success => "SUCCESS",
            Outcome::Redirection => "REDIRECTION",
            Outcome::ClientError => "CLIENT_ERROR",
            Outcome::ServerError => "SERVER_ERROR",
            Outcome::Unknown => "UNKNOWN",
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One observed HTTP request, delivered to the [`RequestObserver`]
/// after the response (or panic) completes.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestMetric {
    /// HTTP method (`GET`, `POST`, …).
    pub method: String,
    /// Low-cardinality `uri` tag: the matched route template, or
    /// `NOT_FOUND` / `REDIRECTION` / the raw path per Micrometer rules.
    pub uri: String,
    /// Numeric HTTP status.
    pub status: u16,
    /// Micrometer `outcome` tag.
    pub outcome: Outcome,
    /// `Some("panic")` when the handler panicked, `None` otherwise —
    /// the Rust analog of pyfly's exception-class tag.
    pub exception: Option<String>,
    /// Wall-clock handling time in seconds.
    pub duration_seconds: f64,
    /// Micrometer's two-window rolling maximum for this tag set, in
    /// seconds — feed it to the `…_seconds_max` gauge.
    pub rolling_max_seconds: f64,
}

/// Sink for [`RequestMetric`] observations. `firefly-starter-core`
/// bridges this to the actuator `MetricRegistry`; tests use an
/// in-memory recorder.
pub trait RequestObserver: Send + Sync {
    /// Records one completed request.
    fn record(&self, metric: &RequestMetric);
}

/// Two-window rolling maximum, mirroring Micrometer's time-window max
/// (pyfly's `_RollingMax`). Reports
/// `max(current_window, previous_window)` so a single slow request
/// decays out of the metric after at most `2 * step` seconds instead of
/// pinning the gauge forever.
#[derive(Debug, Clone)]
pub struct RollingMax {
    step: f64,
    idx: Option<i64>,
    cur: f64,
    prev: f64,
}

impl RollingMax {
    /// A rolling max with the given window step in seconds (Micrometer
    /// default: 60).
    pub fn new(step_seconds: f64) -> Self {
        Self {
            step: step_seconds,
            idx: None,
            cur: 0.0,
            prev: 0.0,
        }
    }

    /// Records `value` observed at `now_seconds` (any monotonic clock)
    /// and returns the windowed maximum.
    pub fn record(&mut self, value: f64, now_seconds: f64) -> f64 {
        let idx = (now_seconds / self.step) as i64;
        match self.idx {
            None => self.idx = Some(idx),
            Some(current) if idx != current => {
                // Carry the immediately-preceding window forward; older
                // windows expire.
                self.prev = if idx == current + 1 { self.cur } else { 0.0 };
                self.cur = 0.0;
                self.idx = Some(idx);
            }
            _ => {}
        }
        if value > self.cur {
            self.cur = value;
        }
        self.cur.max(self.prev)
    }
}

impl Default for RollingMax {
    fn default() -> Self {
        Self::new(60.0)
    }
}

type LabelKey = (String, String, u16, Outcome, Option<String>);

#[derive(Debug)]
struct MaxState {
    step: f64,
    epoch: Instant,
    by_key: Mutex<HashMap<LabelKey, RollingMax>>,
}

fn default_exclude_patterns() -> Vec<String> {
    ["/actuator/prometheus", "/admin/api/sse/*"]
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// HTTP server metrics middleware. Add **via `Router::layer`** so axum
/// has already matched the route when the service runs and the
/// `MatchedPath` extension carries the route template.
///
/// Default exclusions mirror pyfly: the Prometheus scrape endpoint
/// itself and the admin dashboard's long-lived SSE streams.
#[derive(Clone)]
pub struct MetricsLayer {
    observer: Arc<dyn RequestObserver>,
    exclude_patterns: Arc<Vec<String>>,
    max_state: Arc<MaxState>,
}

impl std::fmt::Debug for MetricsLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsLayer")
            .field("exclude_patterns", &self.exclude_patterns)
            .finish_non_exhaustive()
    }
}

impl MetricsLayer {
    /// Builds the layer reporting into `observer`, with the Micrometer
    /// default 60-second max window and pyfly's default exclusions.
    pub fn new(observer: Arc<dyn RequestObserver>) -> Self {
        Self {
            observer,
            exclude_patterns: Arc::new(default_exclude_patterns()),
            max_state: Arc::new(MaxState {
                step: 60.0,
                epoch: Instant::now(),
                by_key: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Replaces the rolling-max window step (seconds).
    pub fn with_step(mut self, step_seconds: f64) -> Self {
        self.max_state = Arc::new(MaxState {
            step: step_seconds,
            epoch: self.max_state.epoch,
            by_key: Mutex::new(HashMap::new()),
        });
        self
    }

    /// Replaces the exclude patterns (shell-style globs; matching paths
    /// are not instrumented).
    pub fn with_exclude_patterns<I, P>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        self.exclude_patterns = Arc::new(patterns.into_iter().map(Into::into).collect());
        self
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            observer: Arc::clone(&self.observer),
            exclude_patterns: Arc::clone(&self.exclude_patterns),
            max_state: Arc::clone(&self.max_state),
        }
    }
}

/// The tower service produced by [`MetricsLayer`].
#[derive(Clone)]
pub struct MetricsService<S> {
    inner: S,
    observer: Arc<dyn RequestObserver>,
    exclude_patterns: Arc<Vec<String>>,
    max_state: Arc<MaxState>,
}

impl<S: std::fmt::Debug> std::fmt::Debug for MetricsService<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsService")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

/// Normalizes an axum route template (`/users/:id`, `/assets/*path`)
/// into the Micrometer spelling (`/users/{id}`, `/assets/{path}`) so
/// the `uri` tag matches what Spring Boot, pyfly, and the other ports
/// emit — existing Grafana dashboards key on it.
fn normalize_template(template: &str) -> String {
    template
        .split('/')
        .map(|segment| {
            if let Some(name) = segment.strip_prefix(':') {
                format!("{{{name}}}")
            } else if let Some(name) = segment.strip_prefix('*') {
                format!("{{{name}}}")
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Computes the low-cardinality `uri` tag with Micrometer semantics.
fn uri_tag(matched: Option<&str>, raw_path: &str, status: u16) -> String {
    if let Some(template) = matched {
        return normalize_template(template);
    }
    if status == 404 {
        return "NOT_FOUND".to_string();
    }
    if (300..400).contains(&status) {
        return "REDIRECTION".to_string();
    }
    // A handler matched but the template is unknown — fall back to the
    // raw path rather than dropping the observation entirely.
    raw_path.to_string()
}

fn record(
    observer: &Arc<dyn RequestObserver>,
    max_state: &Arc<MaxState>,
    method: String,
    uri: String,
    status: u16,
    exception: Option<String>,
    duration_seconds: f64,
) {
    let outcome = Outcome::from_status(status);
    let now = max_state.epoch.elapsed().as_secs_f64();
    let key: LabelKey = (
        method.clone(),
        uri.clone(),
        status,
        outcome,
        exception.clone(),
    );
    let rolling_max = {
        let mut by_key = max_state
            .by_key
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        by_key
            .entry(key)
            .or_insert_with(|| RollingMax::new(max_state.step))
            .record(duration_seconds, now)
    };
    observer.record(&RequestMetric {
        method,
        uri,
        status,
        outcome,
        exception,
        duration_seconds,
        rolling_max_seconds: rolling_max,
    });
}

impl<S> Service<Request<Body>> for MetricsService<S>
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

        let raw_path = req.uri().path().to_string();
        if matches_any(&self.exclude_patterns, &raw_path) {
            return Box::pin(async move { inner.call(req).await });
        }

        let method = req.method().to_string();
        let matched = req
            .extensions()
            .get::<MatchedPath>()
            .map(|m| m.as_str().to_string());
        let observer = Arc::clone(&self.observer);
        let max_state = Arc::clone(&self.max_state);

        Box::pin(async move {
            let start = Instant::now();
            let result = AssertUnwindSafe(inner.call(req)).catch_unwind().await;
            let duration = start.elapsed().as_secs_f64();
            match result {
                Ok(res) => {
                    let res = res?;
                    let status = res.status().as_u16();
                    let uri = uri_tag(matched.as_deref(), &raw_path, status);
                    record(&observer, &max_state, method, uri, status, None, duration);
                    Ok(res)
                }
                Err(payload) => {
                    // A panicking handler is recorded as a 500 with the
                    // exception tag, then re-raised for the outer
                    // ProblemLayer — mirroring pyfly's `except … raise`.
                    let uri = uri_tag(matched.as_deref(), &raw_path, 500);
                    record(
                        &observer,
                        &max_state,
                        method,
                        uri,
                        500,
                        Some("panic".to_string()),
                        duration,
                    );
                    resume_unwind(payload)
                }
            }
        })
    }
}
