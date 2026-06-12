//! `GET /actuator/httpexchanges` — Spring Boot's
//! `InMemoryHttpExchangeRepository` parity: a bounded, newest-first ring
//! buffer of recent request/response exchanges populated by the
//! [`HttpExchangesLayer`] tower middleware (pyfly's
//! `HttpExchangeRecorderFilter`).
//!
//! Only a safe header subset is captured; sensitive headers
//! (`authorization`, `cookie`, `set-cookie`, `proxy-authorization`) are
//! masked to `"******"`.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use chrono::{SecondsFormat, Utc};
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use tower::{Layer, Service};

/// Request headers captured (pyfly's `_CAPTURE_REQUEST_HEADERS`).
const CAPTURE_REQUEST_HEADERS: [&str; 4] = ["host", "user-agent", "accept", "content-type"];
/// Response headers captured (pyfly's `_CAPTURE_RESPONSE_HEADERS`).
const CAPTURE_RESPONSE_HEADERS: [&str; 2] = ["content-type", "content-length"];
/// Headers always masked (pyfly's `_SENSITIVE_HEADERS`).
const SENSITIVE_HEADERS: [&str; 4] = [
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];

/// Default ring-buffer capacity, matching Spring Boot's
/// `InMemoryHttpExchangeRepository`.
pub const DEFAULT_EXCHANGE_CAPACITY: usize = 100;

/// The request half of a recorded exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRequest {
    /// HTTP method, e.g. `GET`.
    pub method: String,
    /// The request URI as received.
    pub uri: String,
    /// Captured header subset, Spring shape: name → `[values]`.
    pub headers: BTreeMap<String, Vec<String>>,
}

/// The response half of a recorded exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeResponse {
    /// HTTP status code.
    pub status: u16,
    /// Captured header subset, Spring shape: name → `[values]`.
    pub headers: BTreeMap<String, Vec<String>>,
}

/// One recorded HTTP exchange — the elements of the
/// `{"exchanges": […]}` array served on `GET /actuator/httpexchanges`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpExchange {
    /// RFC 3339 UTC instant at which the exchange completed.
    pub timestamp: String,
    /// The request half.
    pub request: ExchangeRequest,
    /// The response half.
    pub response: ExchangeResponse,
    /// ISO-8601 duration of the exchange, e.g. `PT0.012S` (Spring's
    /// `timeTaken`).
    #[serde(rename = "timeTaken")]
    pub time_taken: String,
}

/// Bounded, newest-first store of recent HTTP exchanges — pyfly's
/// `HttpExchangeRecorder` / Spring's `InMemoryHttpExchangeRepository`.
pub struct HttpExchangeRecorder {
    capacity: usize,
    buf: Mutex<VecDeque<HttpExchange>>,
}

impl Default for HttpExchangeRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpExchangeRecorder {
    /// Returns a recorder holding the last
    /// [`DEFAULT_EXCHANGE_CAPACITY`] exchanges.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EXCHANGE_CAPACITY)
    }

    /// Returns a recorder holding the last `capacity` exchanges.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            buf: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
        }
    }

    /// Appends an exchange, evicting the oldest when full.
    pub fn record(&self, exchange: HttpExchange) {
        let mut buf = self.buf.lock().expect("http exchange lock poisoned");
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(exchange);
    }

    /// The recorded exchanges, most recent first (Spring Boot's order).
    pub fn recent(&self) -> Vec<HttpExchange> {
        self.buf
            .lock()
            .expect("http exchange lock poisoned")
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    /// Removes every recorded exchange.
    pub fn clear(&self) {
        self.buf
            .lock()
            .expect("http exchange lock poisoned")
            .clear();
    }

    /// Number of recorded exchanges.
    pub fn len(&self) -> usize {
        self.buf.lock().expect("http exchange lock poisoned").len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Captures the safe subset of `headers` (Spring shape: name →
/// `[values]`), masking sensitive ones.
fn capture_headers(headers: &HeaderMap, keep: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        let lname = name.as_str().to_ascii_lowercase();
        if SENSITIVE_HEADERS.contains(&lname.as_str()) {
            out.insert(lname, vec!["******".to_string()]);
        } else if keep.contains(&lname.as_str()) {
            out.entry(lname)
                .or_default()
                .push(String::from_utf8_lossy(value.as_bytes()).into_owned());
        }
    }
    out
}

/// Tower middleware recording each request/response into a shared
/// [`HttpExchangeRecorder`] — pyfly's `HttpExchangeRecorderFilter`.
/// Apply it to the application router (the actuator router itself does
/// not record); paths matching an exclude prefix (default:
/// `/actuator/prometheus`) are skipped.
#[derive(Clone)]
pub struct HttpExchangesLayer {
    recorder: Arc<HttpExchangeRecorder>,
    exclude_prefixes: Arc<Vec<String>>,
}

impl HttpExchangesLayer {
    /// Wraps `recorder` with the default exclude prefixes
    /// (`/actuator/prometheus`).
    pub fn new(recorder: Arc<HttpExchangeRecorder>) -> Self {
        Self {
            recorder,
            exclude_prefixes: Arc::new(vec!["/actuator/prometheus".to_string()]),
        }
    }

    /// Replaces the path prefixes excluded from recording.
    #[must_use]
    pub fn with_exclude_prefixes(mut self, prefixes: Vec<String>) -> Self {
        self.exclude_prefixes = Arc::new(prefixes);
        self
    }
}

impl<S> Layer<S> for HttpExchangesLayer {
    type Service = HttpExchangesService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpExchangesService {
            inner,
            recorder: Arc::clone(&self.recorder),
            exclude_prefixes: Arc::clone(&self.exclude_prefixes),
        }
    }
}

/// The [`Service`] produced by [`HttpExchangesLayer`].
#[derive(Clone)]
pub struct HttpExchangesService<S> {
    inner: S,
    recorder: Arc<HttpExchangeRecorder>,
    exclude_prefixes: Arc<Vec<String>>,
}

impl<S, ReqB, ResB> Service<http::Request<ReqB>> for HttpExchangesService<S>
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
        if self.exclude_prefixes.iter().any(|p| path.starts_with(p)) {
            return Box::pin(self.inner.call(req));
        }

        let request = ExchangeRequest {
            method: req.method().to_string(),
            uri: req.uri().to_string(),
            headers: capture_headers(req.headers(), &CAPTURE_REQUEST_HEADERS),
        };
        let recorder = Arc::clone(&self.recorder);
        let start = Instant::now();
        let fut = self.inner.call(req);

        Box::pin(async move {
            let result = fut.await;
            let response = match &result {
                Ok(resp) => ExchangeResponse {
                    status: resp.status().as_u16(),
                    headers: capture_headers(resp.headers(), &CAPTURE_RESPONSE_HEADERS),
                },
                // pyfly's filter records a 500 when the chain raised.
                Err(_) => ExchangeResponse {
                    status: 500,
                    headers: BTreeMap::new(),
                },
            };
            recorder.record(HttpExchange {
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                request,
                response,
                time_taken: format!("PT{:.3}S", start.elapsed().as_secs_f64()),
            });
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::header::{HeaderName, HeaderValue};

    fn exchange(n: u16) -> HttpExchange {
        HttpExchange {
            timestamp: "2026-06-12T00:00:00Z".into(),
            request: ExchangeRequest {
                method: "GET".into(),
                uri: format!("/x/{n}"),
                headers: BTreeMap::new(),
            },
            response: ExchangeResponse {
                status: n,
                headers: BTreeMap::new(),
            },
            time_taken: "PT0.001S".into(),
        }
    }

    #[test]
    fn recorder_is_bounded_and_newest_first() {
        let recorder = HttpExchangeRecorder::with_capacity(2);
        recorder.record(exchange(1));
        recorder.record(exchange(2));
        recorder.record(exchange(3));
        let recent = recorder.recent();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].response.status, 3, "newest first");
        assert_eq!(recent[1].response.status, 2);
        recorder.clear();
        assert!(recorder.is_empty());
    }

    #[test]
    fn default_capacity_is_100() {
        let recorder = HttpExchangeRecorder::new();
        for n in 0..150 {
            recorder.record(exchange(n));
        }
        assert_eq!(recorder.len(), 100);
        assert_eq!(recorder.recent()[0].response.status, 149);
    }

    #[test]
    fn capture_masks_sensitive_and_keeps_subset() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static("test-agent"),
        );
        headers.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("dropped"),
        );
        let captured = capture_headers(&headers, &CAPTURE_REQUEST_HEADERS);
        assert_eq!(captured["authorization"], vec!["******"]);
        assert_eq!(captured["user-agent"], vec!["test-agent"]);
        assert!(!captured.contains_key("x-custom"));
    }

    #[test]
    fn exchange_serializes_with_time_taken_key() {
        let value = serde_json::to_value(exchange(200)).unwrap();
        assert!(value.get("timeTaken").is_some());
        assert!(value.get("time_taken").is_none());
        assert_eq!(value["request"]["method"], "GET");
        assert_eq!(value["response"]["status"], 200);
    }
}
