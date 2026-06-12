//! Correlation-id propagation middleware — the Rust port of the Go
//! module's `correlation.go` (`CorrelationMiddleware`).

use std::convert::Infallible;
use std::sync::LazyLock;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use firefly_kernel::{new_correlation_id, with_correlation_id, HEADER_CORRELATION_ID};
use futures::future::BoxFuture;
use http::{HeaderName, HeaderValue, Request};
use tower::{Layer, Service};

/// The `X-Correlation-Id` header as a typed [`HeaderName`], derived from
/// the kernel constant so there is a single source of truth.
static CORRELATION_HEADER: LazyLock<HeaderName> = LazyLock::new(|| {
    HeaderName::from_bytes(HEADER_CORRELATION_ID.as_bytes()).expect("valid header name")
});

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
/// identical across runtimes.
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
            let mut res = with_correlation_id(id.clone(), inner.call(req)).await?;
            // Echo the id back; a header explicitly set by the handler
            // wins, matching the Go middleware (which sets the header
            // before invoking the next handler).
            if let Ok(value) = HeaderValue::from_str(&id) {
                res.headers_mut()
                    .entry(&*CORRELATION_HEADER)
                    .or_insert(value);
            }
            Ok(res)
        })
    }
}
