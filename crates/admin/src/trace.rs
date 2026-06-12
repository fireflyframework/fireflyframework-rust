//! HTTP request-trace ring buffer + tower layer — the Rust rendering of
//! pyfly's `TraceCollectorFilter`.
//!
//! [`TraceLayer`] wraps the application router, timing each request and
//! appending a [`TraceEntry`] to a shared, fixed-size [`TraceBuffer`]
//! (capacity 500). Requests whose path starts with `/admin` or `/actuator`
//! are skipped, exactly as pyfly excludes `/admin/*` and `/actuator/*`. The
//! buffer feeds `GET /admin/api/traces` and the traces SSE stream.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::{Layer, Service};

/// Default ring-buffer capacity, matching pyfly's `TraceCollectorFilter`.
pub const DEFAULT_TRACE_CAPACITY: usize = 500;

/// Path prefixes excluded from tracing (pyfly's `exclude_patterns`).
const EXCLUDE_PREFIXES: [&str; 2] = ["/admin", "/actuator"];

/// One captured HTTP request/response trace — the elements of the
/// `{"traces": […]}` array served on `GET /admin/api/traces`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceEntry {
    /// RFC 3339 UTC instant at which the request completed.
    pub timestamp: String,
    /// HTTP method, e.g. `GET`.
    pub method: String,
    /// Request path (without the query string).
    pub path: String,
    /// Raw query string (empty when absent).
    pub query_string: String,
    /// Response status code.
    pub status: u16,
    /// Wall-clock duration in milliseconds, rounded to 2 decimals.
    pub duration_ms: f64,
    /// Remote client address, when known.
    pub client_host: Option<String>,
    /// Request `Content-Type`, when present.
    pub content_type: Option<String>,
    /// Request `User-Agent`, truncated to 100 chars (pyfly parity).
    pub user_agent: String,
    /// Response `Content-Length`, when present and numeric.
    pub content_length: Option<i64>,
}

/// Bounded, oldest-first store of recent HTTP traces — pyfly's
/// `deque(maxlen=...)`.
pub struct TraceBuffer {
    capacity: usize,
    buf: Mutex<VecDeque<TraceEntry>>,
}

impl Default for TraceBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceBuffer {
    /// Returns a buffer holding the last [`DEFAULT_TRACE_CAPACITY`] traces.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TRACE_CAPACITY)
    }

    /// Returns a buffer holding the last `capacity` traces (minimum 1).
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            buf: Mutex::new(VecDeque::with_capacity(capacity)),
        }
    }

    /// Appends a trace, evicting the oldest when full.
    pub fn record(&self, entry: TraceEntry) {
        let mut buf = self.buf.lock().expect("trace buffer lock poisoned");
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// The recorded traces in insertion order, oldest first (pyfly's
    /// `get_traces`).
    pub fn entries(&self) -> Vec<TraceEntry> {
        self.buf
            .lock()
            .expect("trace buffer lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Number of recorded traces.
    pub fn len(&self) -> usize {
        self.buf.lock().expect("trace buffer lock poisoned").len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes every recorded trace.
    pub fn clear(&self) {
        self.buf.lock().expect("trace buffer lock poisoned").clear();
    }

    /// The `{"traces": […], "total": N}` body for `GET /admin/api/traces`,
    /// newest first and capped at `limit` (pyfly's `TracesProvider`).
    pub fn traces_json(&self, limit: usize) -> Value {
        let all = self.entries();
        let total = all.len();
        let recent: Vec<TraceEntry> = all.into_iter().rev().take(limit).collect();
        serde_json::json!({ "traces": recent, "total": total })
    }
}

/// Whether the given path is excluded from tracing (an `/admin*` or
/// `/actuator*` path) — pyfly's `should_not_filter`.
pub(crate) fn is_excluded(path: &str) -> bool {
    EXCLUDE_PREFIXES.iter().any(|p| path.starts_with(p))
}

/// Tower layer recording each non-excluded request into a shared
/// [`TraceBuffer`] — pyfly's `TraceCollectorFilter`. Apply it to the
/// **application** router (not the admin router, whose paths are excluded
/// anyway).
#[derive(Clone)]
pub struct TraceLayer {
    buffer: Arc<TraceBuffer>,
}

impl TraceLayer {
    /// Wraps `buffer`, recording every request whose path is not under
    /// `/admin` or `/actuator`.
    pub fn new(buffer: Arc<TraceBuffer>) -> Self {
        Self { buffer }
    }
}

impl<S> Layer<S> for TraceLayer {
    type Service = TraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TraceService {
            inner,
            buffer: Arc::clone(&self.buffer),
        }
    }
}

/// The [`Service`] produced by [`TraceLayer`].
#[derive(Clone)]
pub struct TraceService<S> {
    inner: S,
    buffer: Arc<TraceBuffer>,
}

impl<S, ReqB, ResB> Service<http::Request<ReqB>> for TraceService<S>
where
    S: Service<http::Request<ReqB>, Response = http::Response<ResB>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqB>) -> Self::Future {
        let path = req.uri().path().to_string();
        if is_excluded(&path) {
            return Box::pin(self.inner.call(req));
        }

        let method = req.method().to_string();
        let query_string = req.uri().query().unwrap_or("").to_string();
        let headers = req.headers();
        let content_type = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut user_agent = headers
            .get(http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if user_agent.len() > 100 {
            user_agent.truncate(100);
        }

        let buffer = Arc::clone(&self.buffer);
        let start = Instant::now();
        let fut = self.inner.call(req);

        Box::pin(async move {
            let result = fut.await;
            let (status, content_length) = match &result {
                Ok(resp) => {
                    let cl = resp
                        .headers()
                        .get(http::header::CONTENT_LENGTH)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<i64>().ok());
                    (resp.status().as_u16(), cl)
                }
                Err(_) => (500, None),
            };
            let duration_ms = (start.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0;
            buffer.record(TraceEntry {
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                method,
                path,
                query_string,
                status,
                duration_ms,
                client_host: None,
                content_type,
                user_agent,
                content_length,
            });
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, status: u16) -> TraceEntry {
        TraceEntry {
            timestamp: "2026-06-12T00:00:00Z".into(),
            method: "GET".into(),
            path: path.into(),
            query_string: String::new(),
            status,
            duration_ms: 1.0,
            client_host: None,
            content_type: None,
            user_agent: String::new(),
            content_length: None,
        }
    }

    // pyfly: test_excludes_admin_paths / test_excludes_actuator_paths / test_includes_app_paths
    #[test]
    fn exclusion_matches_pyfly() {
        assert!(is_excluded("/admin/api/overview"));
        assert!(is_excluded("/actuator/health"));
        assert!(!is_excluded("/api/users"));
    }

    // pyfly: test_max_traces_ring_buffer
    #[test]
    fn ring_buffer_is_bounded() {
        let buf = TraceBuffer::with_capacity(2);
        for n in 0..5 {
            buf.record(entry("/api/test", 200 + n));
        }
        assert_eq!(buf.len(), 2);
        let entries = buf.entries();
        assert_eq!(entries[0].status, 203, "oldest kept is the 4th record");
        assert_eq!(entries[1].status, 204);
    }

    // pyfly: test_captures_trace
    #[test]
    fn captures_method_path_status() {
        let buf = TraceBuffer::new();
        buf.record(entry("/api/users", 200));
        let entries = buf.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].method, "GET");
        assert_eq!(entries[0].path, "/api/users");
        assert_eq!(entries[0].status, 200);
    }

    #[test]
    fn traces_json_is_newest_first_and_capped() {
        let buf = TraceBuffer::new();
        for n in 0..5 {
            buf.record(entry(&format!("/api/{n}"), 200));
        }
        let body = buf.traces_json(2);
        assert_eq!(body["total"], 5);
        assert_eq!(body["traces"].as_array().unwrap().len(), 2);
        assert_eq!(body["traces"][0]["path"], "/api/4", "newest first");
    }

    #[test]
    fn clear_empties_buffer() {
        let buf = TraceBuffer::new();
        buf.record(entry("/api/x", 200));
        assert!(!buf.is_empty());
        buf.clear();
        assert!(buf.is_empty());
    }
}
