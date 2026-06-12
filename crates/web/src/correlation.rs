//! Correlation-id propagation middleware — the Rust port of the Go
//! module's `correlation.go` (`CorrelationMiddleware`).

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
use http::{HeaderName, HeaderValue, Request};
use tower::{Layer, Service};

/// The `X-Correlation-Id` header as a typed [`HeaderName`], derived from
/// the kernel constant so there is a single source of truth.
pub(crate) static CORRELATION_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()).expect("valid header name")
});

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

/// Extracts `X-Correlation-Id` from the incoming request — generating a
/// fresh id when absent — stores it in the request extensions as
/// [`CorrelationId`], runs the inner service inside a
/// [`firefly_kernel::with_correlation_id`] task-local scope, and echoes
/// the id back on the response. The Rust analog of the Go port's
/// `CorrelationMiddleware`; the header name and echo semantics are
/// identical across runtimes. The echo also survives a panicking
/// handler: the id rides along with the unwinding panic so the 500
/// recovered by the outer [`crate::ProblemLayer`] still carries
/// `X-Correlation-Id`, just as Go's recovered 500 does.
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
        let id = req
            .headers()
            .get(&*CORRELATION_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(new_correlation_id);
        req.extensions_mut().insert(CorrelationId(id.clone()));

        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let scope_id = id.clone();
            let result =
                AssertUnwindSafe(
                    async move { with_correlation_id(scope_id, inner.call(req)).await },
                )
                .catch_unwind()
                .await;
            match result {
                Ok(res) => {
                    let mut res = res?;
                    // Echo the id back; a header explicitly set by the
                    // handler wins, matching the Go middleware (which
                    // sets the header before invoking the next handler).
                    if let Ok(value) = HeaderValue::from_str(&id) {
                        res.headers_mut()
                            .entry(&*CORRELATION_HEADER)
                            .or_insert(value);
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
