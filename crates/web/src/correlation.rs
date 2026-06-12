//! Correlation-id propagation middleware — the Rust port of the Go
//! module's `correlation.go` (`CorrelationMiddleware`), extended to the
//! full pyfly correlation surface (`CorrelationFilter` +
//! `TransactionIdFilter`): `X-Correlation-Id`, `X-Request-Id`,
//! `X-Tenant-Id`, `X-Transaction-Id`, and the W3C `traceparent` /
//! `tracestate` pair.
//!
//! Wire behavior, header-for-header identical to pyfly:
//!
//! * `X-Correlation-Id` — echoed when supplied, generated when absent
//!   (unchanged from the Go-parity layer).
//! * `X-Request-Id` — one identifier per HTTP call; minted (UUID v4)
//!   when absent, always echoed.
//! * `X-Tenant-Id` — multi-tenant scope; **never generated server-side**,
//!   echoed only when supplied.
//! * `X-Transaction-Id` — propagated or minted (UUID v4), always echoed.
//! * `traceparent` / `tracestate` — echoed unchanged when present.
//!
//! The full set is stored in the request extensions as a
//! [`CorrelationContext`] and bound to a task-local scope readable via
//! [`current_correlation_context`], the Rust analog of pyfly's
//! `contextvars`-backed `current_correlation_context()`.

use std::any::Any;
use std::convert::Infallible;
use std::panic::{resume_unwind, AssertUnwindSafe};
use std::sync::LazyLock;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use firefly_kernel::{new_correlation_id, with_correlation_id, HEADER_CORRELATION_ID};
use futures::future::BoxFuture;
use futures::FutureExt;
use http::{HeaderMap, HeaderName, HeaderValue, Request};
use tower::{Layer, Service};

/// The `X-Request-Id` header name — one identifier per HTTP call,
/// minted when absent and echoed on the response.
pub const HEADER_REQUEST_ID: &str = "X-Request-Id";

/// The `X-Tenant-Id` header name — multi-tenant scope, propagated but
/// never generated server-side.
pub const HEADER_TENANT_ID: &str = "X-Tenant-Id";

/// The `X-Transaction-Id` header name — propagated or minted, always
/// echoed on the response.
pub const HEADER_TRANSACTION_ID: &str = "X-Transaction-Id";

/// The W3C Trace Context `traceparent` header name, echoed unchanged.
pub const HEADER_TRACEPARENT: &str = "traceparent";

/// The W3C Trace Context `tracestate` header name, echoed unchanged.
pub const HEADER_TRACESTATE: &str = "tracestate";

/// The `X-Correlation-Id` header as a typed [`HeaderName`], derived from
/// the kernel constant so there is a single source of truth.
pub(crate) static CORRELATION_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()).expect("valid header name")
});

static REQUEST_ID_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_REQUEST_ID.as_bytes()).expect("valid header name")
});
static TENANT_ID_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_TENANT_ID.as_bytes()).expect("valid header name")
});
static TRANSACTION_ID_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_TRANSACTION_ID.as_bytes()).expect("valid header name")
});
static TRACEPARENT_HEADER: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static(HEADER_TRACEPARENT));
static TRACESTATE_HEADER: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static(HEADER_TRACESTATE));

/// A panic payload that unwound through [`CorrelationService`], wrapped
/// together with the request's correlation id. The Go middleware sets
/// `X-Correlation-Id` on the shared response-header map *before*
/// invoking next, so the recovered 500 written by the outer recover
/// middleware still carries the id; Rust has no shared response while
/// unwinding, so the id travels inside the panic payload instead and
/// the outer [`crate::ProblemLayer`] attaches it to the recovered 500.
pub(crate) struct CorrelationPanic {
    /// The correlation id in effect for the panicking request.
    pub(crate) id: String,
    /// The original panic payload.
    pub(crate) payload: Box<dyn Any + Send>,
}

/// The correlation id of the current request, stored in the request
/// extensions by [`CorrelationLayer`] so handlers can extract it with
/// `axum::Extension<CorrelationId>`. Handlers running under the layer
/// can equivalently call [`firefly_kernel::correlation_id`], which reads
/// the task-local scope the layer installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationId(pub String);

/// The full correlation surface of the current request — the Rust
/// analog of the contextvars pyfly's `CorrelationFilter` binds. Stored
/// in the request extensions (extract with
/// `axum::Extension<CorrelationContext>`) and in a task-local scope
/// readable via [`current_correlation_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationContext {
    /// `X-Correlation-Id` — service-hop correlation, echoed or minted.
    pub correlation_id: String,
    /// `X-Request-Id` — one id per HTTP call, echoed or minted (UUID v4).
    pub request_id: String,
    /// `X-Tenant-Id` — propagated only; `None` when the caller sent none.
    pub tenant_id: Option<String>,
    /// `X-Transaction-Id` — propagated or minted (UUID v4).
    pub transaction_id: String,
    /// W3C `traceparent`, echoed unchanged when present.
    pub traceparent: Option<String>,
    /// W3C `tracestate`, echoed unchanged when present.
    pub tracestate: Option<String>,
}

tokio::task_local! {
    /// Task-local slot mirroring pyfly's correlation contextvars.
    static CORRELATION_CONTEXT: CorrelationContext;
}

/// Runs `fut` with `ctx` as the ambient [`CorrelationContext`] — what
/// [`CorrelationLayer`] does for every request. Exposed so non-HTTP
/// entry points (message consumers, schedulers) can install the same
/// scope.
pub async fn with_correlation_context<F: std::future::Future>(
    ctx: CorrelationContext,
    fut: F,
) -> F::Output {
    CORRELATION_CONTEXT.scope(ctx, fut).await
}

/// Reads the ambient [`CorrelationContext`], returning `None` outside a
/// [`with_correlation_context`] scope — the Rust analog of pyfly's
/// `current_correlation_context()`.
pub fn current_correlation_context() -> Option<CorrelationContext> {
    CORRELATION_CONTEXT.try_with(Clone::clone).ok()
}

fn header_string(headers: &HeaderMap, name: &HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Extracts the correlation surface from the incoming request —
/// generating fresh ids where pyfly does — stores it in the request
/// extensions as [`CorrelationId`] + [`CorrelationContext`], runs the
/// inner service inside [`firefly_kernel::with_correlation_id`] and
/// [`with_correlation_context`] task-local scopes, and echoes the
/// headers back on the response. `X-Correlation-Id` behavior (header
/// name, echo semantics, panic survival) is byte-identical to the
/// original Go-parity layer; the extra pyfly headers ride alongside.
#[derive(Debug, Clone, Copy, Default)]
pub struct CorrelationLayer;

impl CorrelationLayer {
    /// Returns the layer. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for CorrelationLayer {
    type Service = CorrelationService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CorrelationService { inner }
    }
}

/// The tower service produced by [`CorrelationLayer`].
#[derive(Debug, Clone)]
pub struct CorrelationService<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for CorrelationService<S>
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

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let headers = req.headers();
        let id = header_string(headers, &CORRELATION_HEADER).unwrap_or_else(new_correlation_id);
        let ctx = CorrelationContext {
            correlation_id: id.clone(),
            request_id: header_string(headers, &REQUEST_ID_HEADER).unwrap_or_else(new_uuid),
            tenant_id: header_string(headers, &TENANT_ID_HEADER),
            transaction_id: header_string(headers, &TRANSACTION_ID_HEADER).unwrap_or_else(new_uuid),
            traceparent: header_string(headers, &TRACEPARENT_HEADER),
            tracestate: header_string(headers, &TRACESTATE_HEADER),
        };
        req.extensions_mut().insert(CorrelationId(id.clone()));
        req.extensions_mut().insert(ctx.clone());

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let scope_id = id.clone();
            let scope_ctx = ctx.clone();
            let result = AssertUnwindSafe(async move {
                with_correlation_id(
                    scope_id,
                    with_correlation_context(scope_ctx, inner.call(req)),
                )
                .await
            })
            .catch_unwind()
            .await;
            match result {
                Ok(res) => {
                    let mut res = res?;
                    // Echo the surface back; a header explicitly set by
                    // the handler wins, matching the Go middleware (which
                    // sets the header before invoking the next handler).
                    if let Ok(value) = HeaderValue::from_str(&id) {
                        res.headers_mut()
                            .entry(&*CORRELATION_HEADER)
                            .or_insert(value);
                    }
                    if let Ok(value) = HeaderValue::from_str(&ctx.request_id) {
                        res.headers_mut()
                            .entry(&*REQUEST_ID_HEADER)
                            .or_insert(value);
                    }
                    if let Ok(value) = HeaderValue::from_str(&ctx.transaction_id) {
                        res.headers_mut()
                            .entry(&*TRANSACTION_ID_HEADER)
                            .or_insert(value);
                    }
                    if let Some(tenant) = &ctx.tenant_id {
                        if let Ok(value) = HeaderValue::from_str(tenant) {
                            res.headers_mut().entry(&*TENANT_ID_HEADER).or_insert(value);
                        }
                    }
                    if let Some(tp) = &ctx.traceparent {
                        if let Ok(value) = HeaderValue::from_str(tp) {
                            res.headers_mut()
                                .entry(&*TRACEPARENT_HEADER)
                                .or_insert(value);
                        }
                    }
                    if let Some(ts) = &ctx.tracestate {
                        if let Ok(value) = HeaderValue::from_str(ts) {
                            res.headers_mut()
                                .entry(&*TRACESTATE_HEADER)
                                .or_insert(value);
                        }
                    }
                    Ok(res)
                }
                // A panicking handler must still produce a 500 that
                // carries the correlation id (Go stages the header on
                // the shared ResponseWriter before next runs, so the
                // recovered 500 keeps it). Re-raise the panic wrapped
                // with the id; the outer ProblemLayer unwraps it.
                Err(payload) => resume_unwind(Box::new(CorrelationPanic { id, payload })),
            }
        })
    }
}
