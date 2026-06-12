//! RFC 7807 `application/problem+json` rendering and the panic-catching
//! [`ProblemLayer`] — the Rust port of the Go module's `problem.go`
//! (`WriteProblem`, `WriteError`, `ProblemMiddleware`, `ErrorHandler`).

use std::any::Any;
use std::convert::Infallible;
use std::error::Error as StdError;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use firefly_kernel::{as_problem, FireflyError, ProblemDetail, PROBLEM_CONTENT_TYPE};
use futures::future::BoxFuture;
use futures::FutureExt;
use http::{header, HeaderValue, Request, StatusCode};
use tower::{Layer, Service};

/// Renders `pd` as an `application/problem+json` response carrying the
/// problem's status code (`500` when unset) — the Rust analog of the Go
/// port's `WriteProblem(w, pd)`.
///
/// The body is byte-for-byte what the Go port emits: compact JSON with
/// lexicographically ordered keys, empty standard members omitted, and
/// the trailing newline produced by Go's `json.Encoder`.
pub fn problem_response(pd: &ProblemDetail) -> Response {
    let status = StatusCode::from_u16(pd.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut body = serde_json::to_vec(pd).unwrap_or_default();
    body.push(b'\n');
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
    );
    res
}

/// Converts `err` to a [`ProblemDetail`] (via [`firefly_kernel::as_problem`])
/// and renders it with [`problem_response`] — the Rust analog of the Go
/// port's `WriteError(w, err)`. A [`FireflyError`] anywhere in the source
/// chain renders with its own code/status; any other error becomes a
/// generic 500 Internal.
pub fn error_response(err: &(dyn StdError + 'static)) -> Response {
    problem_response(&as_problem(err))
}

/// The web-tier error wrapper that lets axum handlers return framework
/// errors with `?` — the Rust analog of the Go port's typed
/// `ErrorHandler` / `AsHandler()` adapter. Any [`FireflyError`] (or
/// pre-built [`ProblemDetail`]) converts into a `WebError`, and axum
/// renders it as an RFC 7807 response via [`IntoResponse`].
#[derive(Debug, Clone, PartialEq)]
pub struct WebError(pub ProblemDetail);

/// The canonical handler result: `Ok` renders normally, `Err` renders as
/// `application/problem+json`. Pair with `?` on any [`FireflyResult`]
/// expression inside the handler.
///
/// [`FireflyResult`]: firefly_kernel::FireflyResult
pub type WebResult<T> = Result<T, WebError>;

impl From<FireflyError> for WebError {
    fn from(err: FireflyError) -> Self {
        Self(err.to_problem())
    }
}

impl From<ProblemDetail> for WebError {
    fn from(pd: ProblemDetail) -> Self {
        Self(pd)
    }
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = if self.0.detail.is_empty() {
            &self.0.title
        } else {
            &self.0.detail
        };
        write!(f, "{}: {}", self.0.problem_type, what)
    }
}

impl StdError for WebError {}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        problem_response(&self.0)
    }
}

/// Panic recovery + automatic RFC 7807 rendering as a composable tower
/// layer — the Rust analog of the Go port's `ProblemMiddleware`. Apply
/// at the outermost layer of the handler chain: a panicking handler
/// produces a 500 `application/problem+json` response with
/// [`firefly_kernel::TYPE_INTERNAL`] instead of tearing down the
/// connection.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProblemLayer;

impl ProblemLayer {
    /// Returns the layer. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for ProblemLayer {
    type Service = ProblemService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ProblemService { inner }
    }
}

/// The tower service produced by [`ProblemLayer`]. Catches panics from
/// the wrapped service — both during request dispatch and at any await
/// point of the response future — and renders them as 500 problems.
#[derive(Debug, Clone)]
pub struct ProblemService<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for ProblemService<S>
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
        // Take the ready service and leave a clone in its place — the
        // standard tower pattern for 'static futures.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let fut = match std::panic::catch_unwind(AssertUnwindSafe(|| inner.call(req))) {
                Ok(fut) => fut,
                Err(payload) => return Ok(panic_response(payload.as_ref())),
            };
            match AssertUnwindSafe(fut).catch_unwind().await {
                Ok(res) => res,
                Err(payload) => Ok(panic_response(payload.as_ref())),
            }
        })
    }
}

/// Renders a panic payload as a 500 internal problem. Mirrors the Go
/// port's `recoverMiddleware`: where Go uses the panic value's message
/// when it is an `error`, Rust uses the payload string of
/// `panic!("...")`; any other payload becomes the literal `"panic"`.
fn panic_response(payload: &(dyn Any + Send)) -> Response {
    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_owned()
    };
    problem_response(&ProblemDetail::internal(msg))
}
