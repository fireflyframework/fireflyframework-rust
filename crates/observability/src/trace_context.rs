//! W3C Trace Context propagation — the Rust port of pyfly's
//! `pyfly.observability.propagation` + the `traceparent`/`tracestate`
//! contextvars from `pyfly.observability.correlation`.
//!
//! pyfly delegates parsing to the OpenTelemetry propagator; this port
//! implements the W3C wire format natively so no OTel SDK is required:
//!
//! * [`TraceParent`] / [`TraceState`] — parse + format the `traceparent`
//!   and `tracestate` headers per <https://www.w3.org/TR/trace-context/>.
//! * Task-locals ([`current_traceparent`], [`current_tracestate`],
//!   [`with_trace_context`]) — the Rust analog of pyfly's contextvars.
//!   The kernel's task-local carries the correlation id; the trace-context
//!   pair lives here because it is an observability concern.
//! * [`TraceContextLayer`] — a [`tower`] layer that extracts the inbound
//!   headers, stores the parsed context in the request extensions and
//!   scopes the task-locals around the inner service (pyfly's
//!   `TracingFilter` + `extract_context`).
//! * [`inject_headers`] / [`inject_reqwest`] — outbound propagation
//!   (pyfly's `inject_headers`), so downstream services receive an
//!   unbroken chain.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use http::header::{HeaderMap, HeaderValue};
use http::Request;
use tower::{Layer, Service};

/// The W3C `traceparent` header name.
pub const TRACEPARENT_HEADER: &str = "traceparent";
/// The W3C `tracestate` header name.
pub const TRACESTATE_HEADER: &str = "tracestate";

/// Error returned when a `traceparent`/`tracestate` header is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceContextError(String);

impl fmt::Display for TraceContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid trace context: {}", self.0)
    }
}

impl std::error::Error for TraceContextError {}

fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A parsed W3C `traceparent` header
/// (`{version}-{trace_id}-{parent_id}-{flags}`).
///
/// ```
/// use firefly_observability::TraceParent;
///
/// let tp = TraceParent::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01").unwrap();
/// assert_eq!(tp.trace_id, "0af7651916cd43dd8448eb211c80319c");
/// assert!(tp.sampled());
/// assert_eq!(
///     tp.to_string(),
///     "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceParent {
    /// Version field (currently `00`).
    pub version: u8,
    /// 32 lowercase hex characters; never all-zero.
    pub trace_id: String,
    /// 16 lowercase hex characters; never all-zero.
    pub parent_id: String,
    /// Trace flags (bit 0 = sampled).
    pub flags: u8,
}

impl TraceParent {
    /// Parses a `traceparent` header value, enforcing the W3C rules:
    /// lowercase hex, version ≠ `ff`, non-zero trace/parent ids. Versions
    /// above `00` may carry extra `-`-separated fields, which are ignored
    /// (forward compatibility, as the spec requires).
    pub fn parse(value: &str) -> Result<Self, TraceContextError> {
        let value = value.trim();
        let parts: Vec<&str> = value.split('-').collect();
        if parts.len() < 4 {
            return Err(TraceContextError(format!(
                "traceparent must have 4 fields, got {}",
                parts.len()
            )));
        }
        let (version_s, trace_id, parent_id, flags_s) = (parts[0], parts[1], parts[2], parts[3]);
        if version_s.len() != 2 || !is_lower_hex(version_s) {
            return Err(TraceContextError("malformed version".into()));
        }
        let version =
            u8::from_str_radix(version_s, 16).map_err(|e| TraceContextError(e.to_string()))?;
        if version == 0xff {
            return Err(TraceContextError("version ff is forbidden".into()));
        }
        if version == 0 && parts.len() != 4 {
            return Err(TraceContextError(
                "version 00 must have exactly 4 fields".into(),
            ));
        }
        if trace_id.len() != 32 || !is_lower_hex(trace_id) {
            return Err(TraceContextError("malformed trace-id".into()));
        }
        if trace_id.bytes().all(|b| b == b'0') {
            return Err(TraceContextError("all-zero trace-id".into()));
        }
        if parent_id.len() != 16 || !is_lower_hex(parent_id) {
            return Err(TraceContextError("malformed parent-id".into()));
        }
        if parent_id.bytes().all(|b| b == b'0') {
            return Err(TraceContextError("all-zero parent-id".into()));
        }
        if flags_s.len() != 2 || !is_lower_hex(flags_s) {
            return Err(TraceContextError("malformed trace-flags".into()));
        }
        let flags =
            u8::from_str_radix(flags_s, 16).map_err(|e| TraceContextError(e.to_string()))?;
        Ok(Self {
            version,
            trace_id: trace_id.to_string(),
            parent_id: parent_id.to_string(),
            flags,
        })
    }

    /// Whether the sampled flag (bit 0) is set.
    pub fn sampled(&self) -> bool {
        self.flags & 0x01 == 0x01
    }
}

impl fmt::Display for TraceParent {
    /// Formats back to the canonical wire form
    /// (`00-{trace_id}-{parent_id}-{flags:02x}`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}-{}-{}-{:02x}",
            self.version, self.trace_id, self.parent_id, self.flags
        )
    }
}

impl std::str::FromStr for TraceParent {
    type Err = TraceContextError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// A parsed W3C `tracestate` header — an ordered list of `key=value`
/// vendor entries.
///
/// ```
/// use firefly_observability::TraceState;
///
/// let ts = TraceState::parse("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7").unwrap();
/// assert_eq!(ts.get("rojo"), Some("00f067aa0ba902b7"));
/// assert_eq!(ts.to_string(), "congo=t61rcWkgMzE,rojo=00f067aa0ba902b7");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceState {
    entries: Vec<(String, String)>,
}

impl TraceState {
    /// Maximum number of list members the spec allows.
    pub const MAX_ENTRIES: usize = 32;

    /// Parses a `tracestate` header value. Empty list members are skipped
    /// (the spec permits them); a member without `=` or more than 32
    /// members is an error, in which case the whole header should be
    /// discarded (the W3C rule, also OTel's behaviour).
    pub fn parse(value: &str) -> Result<Self, TraceContextError> {
        let mut entries = Vec::new();
        for member in value.split(',') {
            let member = member.trim();
            if member.is_empty() {
                continue;
            }
            let (key, val) = member.split_once('=').ok_or_else(|| {
                TraceContextError(format!("tracestate member {member:?} has no '='"))
            })?;
            if key.is_empty() {
                return Err(TraceContextError("tracestate member has empty key".into()));
            }
            entries.push((key.to_string(), val.to_string()));
        }
        if entries.len() > Self::MAX_ENTRIES {
            return Err(TraceContextError(format!(
                "tracestate has {} members (max {})",
                entries.len(),
                Self::MAX_ENTRIES
            )));
        }
        Ok(Self { entries })
    }

    /// The value for `key`, if present.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The entries in wire order.
    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

impl fmt::Display for TraceState {
    /// Formats back to the wire form (`k1=v1,k2=v2`), preserving order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let body: Vec<String> = self
            .entries
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        write!(f, "{}", body.join(","))
    }
}

impl std::str::FromStr for TraceState {
    type Err = TraceContextError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

// ---------------------------------------------------------------------------
// Task-local scope (pyfly's contextvars)
// ---------------------------------------------------------------------------

tokio::task_local! {
    static TRACEPARENT: Option<String>;
    static TRACESTATE: Option<String>;
}

/// Runs `fut` with the given `traceparent`/`tracestate` values in
/// task-local scope — the Rust analog of pyfly's
/// `set_traceparent`/`set_tracestate` contextvars. Scopes nest; the values
/// propagate through every `.await` inside `fut`.
pub async fn with_trace_context<F: Future>(
    traceparent: Option<String>,
    tracestate: Option<String>,
    fut: F,
) -> F::Output {
    TRACEPARENT
        .scope(traceparent, TRACESTATE.scope(tracestate, fut))
        .await
}

/// The `traceparent` header value in the current task-local scope —
/// pyfly's `get_traceparent()`. `None` outside a scope or when the
/// inbound request carried no (valid) header.
pub fn current_traceparent() -> Option<String> {
    TRACEPARENT.try_with(Clone::clone).ok().flatten()
}

/// The `tracestate` header value in the current task-local scope —
/// pyfly's `get_tracestate()`.
pub fn current_tracestate() -> Option<String> {
    TRACESTATE.try_with(Clone::clone).ok().flatten()
}

// ---------------------------------------------------------------------------
// Tower extract layer (pyfly's TracingFilter / extract_context)
// ---------------------------------------------------------------------------

/// A [`tower`] layer that extracts the W3C trace context from inbound
/// request headers — the Rust analog of pyfly's `TracingFilter` +
/// `extract_context`.
///
/// For every request it:
/// 1. parses `traceparent` (and `tracestate`, which is only meaningful
///    alongside a valid `traceparent`),
/// 2. inserts the parsed [`TraceParent`] / [`TraceState`] into the request
///    extensions, and
/// 3. scopes the raw header values into the task-locals read by
///    [`current_traceparent`] / [`current_tracestate`] and re-injected by
///    [`inject_headers`], so the chain stays unbroken across hops.
///
/// Malformed headers are dropped (the W3C "restart the trace" rule).
#[derive(Debug, Clone, Copy, Default)]
pub struct TraceContextLayer;

impl TraceContextLayer {
    /// Creates the layer.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for TraceContextLayer {
    type Service = TraceContextService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TraceContextService { inner }
    }
}

/// The service produced by [`TraceContextLayer`].
#[derive(Debug, Clone)]
pub struct TraceContextService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for TraceContextService<S>
where
    S: Service<Request<B>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let parsed = req
            .headers()
            .get(TRACEPARENT_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| TraceParent::parse(v).ok());
        // tracestate is meaningless without a valid traceparent (W3C).
        let (traceparent, tracestate) = match parsed {
            Some(tp) => {
                let state = req
                    .headers()
                    .get(TRACESTATE_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| TraceState::parse(v).ok())
                    .filter(|ts| !ts.is_empty());
                let raw_tp = tp.to_string();
                let raw_ts = state.as_ref().map(ToString::to_string);
                req.extensions_mut().insert(tp);
                if let Some(ts) = state {
                    req.extensions_mut().insert(ts);
                }
                (Some(raw_tp), raw_ts)
            }
            None => (None, None),
        };
        let fut = self.inner.call(req);
        Box::pin(with_trace_context(traceparent, tracestate, fut))
    }
}

// ---------------------------------------------------------------------------
// Outbound injection (pyfly's inject_headers)
// ---------------------------------------------------------------------------

/// Injects the current task-local trace context into `headers` in place —
/// pyfly's `inject_headers`. A no-op when no context is in scope.
pub fn inject_headers(headers: &mut HeaderMap) {
    if let Some(tp) = current_traceparent() {
        if let Ok(value) = HeaderValue::from_str(&tp) {
            headers.insert(TRACEPARENT_HEADER, value);
        }
    }
    if let Some(ts) = current_tracestate() {
        if let Ok(value) = HeaderValue::from_str(&ts) {
            headers.insert(TRACESTATE_HEADER, value);
        }
    }
}

/// Returns `builder` with the current task-local trace context added as
/// `traceparent`/`tracestate` headers — the reqwest flavour of
/// [`inject_headers`] for outbound HTTP calls.
pub fn inject_reqwest(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let mut headers = HeaderMap::new();
    inject_headers(&mut headers);
    if headers.is_empty() {
        builder
    } else {
        builder.headers(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    #[test]
    fn parse_and_format_round_trip() {
        let tp = TraceParent::parse(EXAMPLE).unwrap();
        assert_eq!(tp.version, 0);
        assert_eq!(tp.trace_id, "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(tp.parent_id, "b7ad6b7169203331");
        assert_eq!(tp.flags, 1);
        assert!(tp.sampled());
        assert_eq!(tp.to_string(), EXAMPLE);
    }

    #[test]
    fn parse_rejects_malformed_traceparent() {
        for bad in [
            "",
            "00",
            "00-abc-def-01",
            // all-zero trace id
            "00-00000000000000000000000000000000-b7ad6b7169203331-01",
            // all-zero parent id
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01",
            // version ff forbidden
            "ff-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            // uppercase hex forbidden
            "00-0AF7651916CD43DD8448EB211C80319C-B7AD6B7169203331-01",
            // version 00 with trailing field
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-extra",
            // bad flag length
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-1",
        ] {
            assert!(TraceParent::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn parse_accepts_future_version_with_extra_fields() {
        let tp = TraceParent::parse(
            "cc-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01-what-the-future-holds",
        )
        .unwrap();
        assert_eq!(tp.version, 0xcc);
        assert_eq!(tp.trace_id, "0af7651916cd43dd8448eb211c80319c");
    }

    #[test]
    fn not_sampled_flag() {
        let tp =
            TraceParent::parse("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00").unwrap();
        assert!(!tp.sampled());
    }

    #[test]
    fn tracestate_round_trip_preserves_order() {
        let ts = TraceState::parse("congo=t61rcWkgMzE,rojo=00f067aa0ba902b7").unwrap();
        assert_eq!(ts.len(), 2);
        assert_eq!(ts.get("congo"), Some("t61rcWkgMzE"));
        assert_eq!(ts.get("missing"), None);
        assert_eq!(ts.to_string(), "congo=t61rcWkgMzE,rojo=00f067aa0ba902b7");
    }

    #[test]
    fn tracestate_skips_empty_members_and_rejects_invalid() {
        let ts = TraceState::parse("a=1, ,b=2,").unwrap();
        assert_eq!(ts.len(), 2);
        assert!(TraceState::parse("no-equals-sign").is_err());
        assert!(TraceState::parse("=v").is_err());
        let too_many: Vec<String> = (0..33).map(|i| format!("k{i}=v")).collect();
        assert!(TraceState::parse(&too_many.join(",")).is_err());
        assert!(TraceState::parse("").unwrap().is_empty());
    }
}
