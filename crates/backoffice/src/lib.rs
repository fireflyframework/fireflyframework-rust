//! # firefly-backoffice
//!
//! [`Application`] + **back-office context middleware** — the port of
//! the Go `backoffice` module (Java original: `firefly-backoffice`,
//! .NET: `FireflyFramework.BackOffice`).
//!
//! [`BackOffice::new`] composes [`firefly_starter_application`] with a
//! guard middleware that requires every request to carry the canonical
//! operator headers:
//!
//! | Header                  | Purpose                                               |
//! |-------------------------|-------------------------------------------------------|
//! | `X-BackOffice-Branch`   | Branch / tenant identifier the operator is scoped to  |
//! | `X-BackOffice-Operator` | The operator's stable user id                         |
//!
//! Both must be present; the middleware emits a 400
//! `application/problem+json` response when either is missing.
//! Successful requests have the values stored on the request — as a
//! [`BackOfficeContext`] extension *and* a tokio task-local scope — and
//! exposed via [`branch()`] / [`operator()`], the Rust spelling of Go's
//! `backoffice.Branch(ctx)` / `backoffice.Operator(ctx)`.
//!
//! [`BackOffice`] dereferences to [`Application`] (which in turn
//! dereferences to [`Core`]) — the Rust analog of Go's struct embedding
//! — so every application field (`plugins`) and core field (`bus`,
//! `cache`, `broker`, `health`, …) is reachable directly on the
//! back-office value. The starter name defaults to
//! `"starter-backoffice"`.
//!
//! [`BackOffice::apply_middleware_chain`] is Go's `MiddlewareChain()`:
//! the core chain (problem renderer, correlation, idempotency) composed
//! with the back-office guard as the innermost layer — apply it once
//! and every handler gets problem rendering, correlation, idempotency,
//! AND the back-office guard.
//!
//! ## Quick start
//!
//! ```
//! use axum::{routing::get, Router};
//! use firefly_backoffice::{BackOffice, CoreConfig};
//!
//! let bo = BackOffice::new(CoreConfig {
//!     app_name: "loan-bo".into(),
//!     ..CoreConfig::default()
//! });
//!
//! let router = Router::new().route(
//!     "/admin/loans",
//!     get(|| async {
//!         let branch = firefly_backoffice::branch().unwrap_or_default();
//!         let operator = firefly_backoffice::operator().unwrap_or_default();
//!         format!("op {operator} @ branch {branch} listing loans")
//!     }),
//! );
//!
//! // Problem + correlation + idempotency + back-office guard, in the
//! // canonical order.
//! let app = bo.apply_middleware_chain(router);
//! # let _ = app;
//! ```
//!
//! A request without both headers receives:
//!
//! ```text
//! 400 Bad Request
//! Content-Type: application/problem+json
//!
//! {"detail":"missing back-office headers","status":400,
//!  "title":"Bad Request",
//!  "type":"https://fireflyframework.org/problems/bad-request"}
//! ```

#![warn(missing_docs)]

use std::convert::Infallible;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use axum::Router;
use firefly_kernel::ProblemDetail;
use firefly_web::problem_response;
use http::Request;
use tower::{Layer, Service};

pub use firefly_starter_application::{Application, BoxError, Plugin, PluginError, Registry};
pub use firefly_starter_core::{Core, CoreConfig};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = firefly_starter_application::VERSION;

/// The canonical back-office branch identifier header — Go's
/// `backoffice.HeaderBranch`.
pub const HEADER_BRANCH: &str = "X-BackOffice-Branch";

/// The canonical back-office operator identifier header — Go's
/// `backoffice.HeaderOperator`.
pub const HEADER_OPERATOR: &str = "X-BackOffice-Operator";

/// The branch + operator pair extracted from the request headers by
/// [`BackOfficeLayer`] — the Rust spelling of the two `context.Context`
/// values the Go middleware stores.
///
/// The layer makes it available two ways, mirroring the dual surface of
/// the correlation middleware:
///
/// * as a request extension, extractable with
///   `axum::Extension<BackOfficeContext>`;
/// * as a tokio task-local scope, readable anywhere below the layer via
///   [`branch()`] / [`operator()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackOfficeContext {
    /// Branch / tenant identifier the operator is scoped to
    /// (`X-BackOffice-Branch`).
    pub branch: String,
    /// The operator's stable user id (`X-BackOffice-Operator`).
    pub operator: String,
}

impl BackOfficeContext {
    /// Builds a context from the branch and operator identifiers.
    pub fn new(branch: impl Into<String>, operator: impl Into<String>) -> Self {
        Self {
            branch: branch.into(),
            operator: operator.into(),
        }
    }
}

tokio::task_local! {
    /// Task-local storage slot for the back-office context — the Rust
    /// analog of the Go module's private `branchKey` / `operatorKey`
    /// context keys.
    static BACK_OFFICE: BackOfficeContext;
}

/// Runs `fut` with the given back-office context in scope — the Rust
/// analog of the Go middleware's `context.WithValue` pair. Scopes nest:
/// an inner scope shadows the outer one, exactly like a child
/// `context.Context`.
pub async fn with_back_office<F: Future>(ctx: BackOfficeContext, fut: F) -> F::Output {
    BACK_OFFICE.scope(ctx, fut).await
}

/// Runs the synchronous closure `f` with the given back-office context
/// in scope. Useful from blocking code and plain `#[test]` functions.
pub fn with_back_office_sync<F: FnOnce() -> R, R>(ctx: BackOfficeContext, f: F) -> R {
    BACK_OFFICE.sync_scope(ctx, f)
}

/// Extracts the back-office branch from the current task-local scope —
/// Go's `backoffice.Branch(ctx)`. Returns `None` when no scope is
/// active or the branch is empty, matching Go's `ok && v != ""`.
pub fn branch() -> Option<String> {
    BACK_OFFICE
        .try_with(|ctx| ctx.branch.clone())
        .ok()
        .filter(|v| !v.is_empty())
}

/// Extracts the back-office operator from the current task-local scope
/// — Go's `backoffice.Operator(ctx)`. Returns `None` when no scope is
/// active or the operator is empty, matching Go's `ok && v != ""`.
pub fn operator() -> Option<String> {
    BACK_OFFICE
        .try_with(|ctx| ctx.operator.clone())
        .ok()
        .filter(|v| !v.is_empty())
}

/// Back-office context guard as a composable tower layer — the Rust
/// analog of the Go module's `Middleware`. Reads the
/// [`HEADER_BRANCH`] / [`HEADER_OPERATOR`] headers and stores them on
/// the request; a missing (or empty) header short-circuits with a 400
/// `application/problem+json` response and never reaches the inner
/// service.
#[derive(Debug, Clone, Copy, Default)]
pub struct BackOfficeLayer;

impl BackOfficeLayer {
    /// Returns the layer. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for BackOfficeLayer {
    type Service = BackOfficeService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BackOfficeService { inner }
    }
}

/// The tower service produced by [`BackOfficeLayer`].
#[derive(Debug, Clone)]
pub struct BackOfficeService<S> {
    inner: S,
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S> Service<Request<Body>> for BackOfficeService<S>
where
    S: Service<Request<Body>, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        let branch = header_value(&req, HEADER_BRANCH);
        let operator = header_value(&req, HEADER_OPERATOR);
        let (Some(branch), Some(operator)) = (branch, operator) else {
            // Go: web.WriteProblem(w, kernel.ProblemBadRequest("missing
            // back-office headers")) — the inner service is never called.
            return Box::pin(std::future::ready(Ok(problem_response(
                &ProblemDetail::bad_request("missing back-office headers"),
            ))));
        };

        let ctx = BackOfficeContext { branch, operator };
        req.extensions_mut().insert(ctx.clone());

        // Take the ready service and leave a clone in its place — the
        // standard tower pattern for 'static futures.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move { BACK_OFFICE.scope(ctx, inner.call(req)).await })
    }
}

/// Returns the header value as an owned string, treating absent,
/// non-UTF-8, and empty values as missing — Go's `r.Header.Get(name)`
/// followed by the `== ""` check.
fn header_value(req: &Request<Body>, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

/// [`Application`] with back-office context middleware enabled by
/// default — the Rust spelling of the Go `backoffice.BackOffice` struct
/// (which embeds `*starterapplication.Application`).
pub struct BackOffice {
    /// The wired application starter — also reachable through deref,
    /// mirroring Go's embedded-field promotion.
    pub app: Application,
}

impl BackOffice {
    /// Wires the back-office starter — Go's `backoffice.New(cfg)`.
    ///
    /// Identical to [`Application::new`] except the
    /// `"starter-application"` starter name becomes
    /// `"starter-backoffice"`; any other starter name configured
    /// explicitly is preserved (exactly Go's
    /// `if app.StarterName == "starter-application"` guard).
    pub fn new(cfg: CoreConfig) -> Self {
        let mut app = Application::new(cfg);
        if app.starter_name == "starter-application" {
            app.starter_name = "starter-backoffice".to_string();
        }
        BackOffice { app }
    }

    /// Wraps `router` in the core middleware chain composed with the
    /// back-office guard as the innermost layer — Go's
    /// `MiddlewareChain()`.
    ///
    /// The execution order is `Problem → Correlation → Idempotency →
    /// BackOffice → router`, so a rejected request still renders as a
    /// problem and still carries a correlation id. The core-only chain
    /// remains available as `apply_middleware` through deref, exactly
    /// like Go's promoted `Middleware()`.
    pub fn apply_middleware_chain(&self, router: Router) -> Router {
        self.app
            .apply_middleware(router.layer(BackOfficeLayer::new()))
    }
}

impl Deref for BackOffice {
    type Target = Application;

    fn deref(&self) -> &Application {
        &self.app
    }
}

impl DerefMut for BackOffice {
    fn deref_mut(&mut self) -> &mut Application {
        &mut self.app
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Extension;
    use firefly_kernel::{HEADER_CORRELATION_ID, PROBLEM_CONTENT_TYPE, TYPE_BAD_REQUEST};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    fn back_office() -> BackOffice {
        BackOffice::new(CoreConfig {
            app_name: "bo".into(),
            ..CoreConfig::default()
        })
    }

    fn ok_router() -> Router {
        Router::new().route("/x", get(|| async { StatusCode::OK }))
    }

    fn request(headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::get("/x");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn body_bytes(res: Response) -> Vec<u8> {
        res.into_body().collect().await.unwrap().to_bytes().to_vec()
    }

    // ---- ports of the Go test suite -----------------------------------------

    /// Go: TestBackOfficeRejectsMissingHeaders.
    #[tokio::test]
    async fn back_office_rejects_missing_headers() {
        let bo = back_office();
        let app = bo.apply_middleware_chain(ok_router());
        let res = app.oneshot(request(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    /// Go: TestBackOfficePropagatesContext.
    #[tokio::test]
    async fn back_office_propagates_context() {
        let bo = back_office();
        let seen: Arc<Mutex<(Option<String>, Option<String>)>> = Arc::default();
        let record = Arc::clone(&seen);
        let app = bo.apply_middleware_chain(Router::new().route(
            "/x",
            get(move || {
                let record = Arc::clone(&record);
                async move {
                    *record.lock().unwrap() = (branch(), operator());
                    StatusCode::OK
                }
            }),
        ));

        let res = app
            .oneshot(request(&[
                (HEADER_BRANCH, "branch-1"),
                (HEADER_OPERATOR, "op-7"),
            ]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let (seen_branch, seen_operator) = seen.lock().unwrap().clone();
        assert_eq!(seen_branch.as_deref(), Some("branch-1"));
        assert_eq!(seen_operator.as_deref(), Some("op-7"));
    }

    // ---- rejection behavior --------------------------------------------------

    /// Either header missing rejects — Go's `branch == "" || operator == ""`.
    #[tokio::test]
    async fn rejects_when_either_header_missing() {
        for headers in [
            vec![(HEADER_BRANCH, "branch-1")],
            vec![(HEADER_OPERATOR, "op-7")],
        ] {
            let app = back_office().apply_middleware_chain(ok_router());
            let res = app.oneshot(request(&headers)).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::BAD_REQUEST,
                "headers: {headers:?}"
            );
        }
    }

    /// An empty header value counts as missing, exactly like Go where
    /// `r.Header.Get` yields `""` for both cases.
    #[tokio::test]
    async fn rejects_empty_header_values() {
        let app = back_office().apply_middleware_chain(ok_router());
        let res = app
            .oneshot(request(&[(HEADER_BRANCH, ""), (HEADER_OPERATOR, "op-7")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    /// The rejection renders the canonical RFC 7807 envelope, byte-for-
    /// byte what the Go port's `web.WriteProblem` emits.
    #[tokio::test]
    async fn rejection_renders_problem_json() {
        let app = back_office().apply_middleware_chain(ok_router());
        let res = app.oneshot(request(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            PROBLEM_CONTENT_TYPE
        );
        let body = body_bytes(res).await;
        assert_eq!(
            body,
            format!(
                "{{\"detail\":\"missing back-office headers\",\"status\":400,\"title\":\"Bad Request\",\"type\":\"{TYPE_BAD_REQUEST}\"}}\n"
            )
            .into_bytes()
        );
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["type"], TYPE_BAD_REQUEST);
        assert_eq!(parsed["title"], "Bad Request");
        assert_eq!(parsed["status"], 400);
        assert_eq!(parsed["detail"], "missing back-office headers");
    }

    /// The guard short-circuits before the handler, like Go's middleware
    /// returning without calling `next.ServeHTTP`.
    #[tokio::test]
    async fn rejection_never_reaches_handler() {
        let hits = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&hits);
        let app = back_office().apply_middleware_chain(Router::new().route(
            "/x",
            get(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        ));
        let res = app.oneshot(request(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "handler must not run");
    }

    // ---- chain ordering -------------------------------------------------------

    /// The back-office guard sits inside the core chain, so even a 400
    /// rejection carries the correlation id — Go's
    /// `core(Middleware(next))` composition.
    #[tokio::test]
    async fn rejection_carries_correlation_id() {
        let app = back_office().apply_middleware_chain(ok_router());
        let res = app
            .oneshot(request(&[(HEADER_CORRELATION_ID, "abc-123")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(res.headers().get(HEADER_CORRELATION_ID).unwrap(), "abc-123");
    }

    /// A successful request flows through the full chain: correlation id
    /// echoed AND back-office context in scope.
    #[tokio::test]
    async fn success_flows_through_full_chain() {
        let app = back_office().apply_middleware_chain(Router::new().route(
            "/x",
            get(|| async {
                assert_eq!(branch().as_deref(), Some("b1"));
                assert_eq!(operator().as_deref(), Some("o1"));
                StatusCode::OK
            }),
        ));
        let res = app
            .oneshot(request(&[
                (HEADER_BRANCH, "b1"),
                (HEADER_OPERATOR, "o1"),
                (HEADER_CORRELATION_ID, "xyz-9"),
            ]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get(HEADER_CORRELATION_ID).unwrap(), "xyz-9");
    }

    /// The standalone layer composes onto any router without the core
    /// chain — Go's exported `Middleware` function.
    #[tokio::test]
    async fn standalone_layer_guards_router() {
        let app = ok_router().layer(BackOfficeLayer::new());

        let res = app.clone().oneshot(request(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .oneshot(request(&[(HEADER_BRANCH, "b"), (HEADER_OPERATOR, "o")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// The context is also stored as a request extension, extractable
    /// with `axum::Extension<BackOfficeContext>`.
    #[tokio::test]
    async fn context_available_as_request_extension() {
        let app = back_office().apply_middleware_chain(Router::new().route(
            "/x",
            get(|Extension(ctx): Extension<BackOfficeContext>| async move {
                format!("{}/{}", ctx.branch, ctx.operator)
            }),
        ));
        let res = app
            .oneshot(request(&[
                (HEADER_BRANCH, "branch-1"),
                (HEADER_OPERATOR, "op-7"),
            ]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_bytes(res).await, b"branch-1/op-7");
    }

    // ---- accessors --------------------------------------------------------------

    /// Outside any scope the accessors return None — Go's `ok == false`
    /// on a bare context.
    #[test]
    fn accessors_outside_scope_return_none() {
        assert_eq!(branch(), None);
        assert_eq!(operator(), None);
    }

    /// Empty values read back as None — Go's `ok && v != ""`.
    #[test]
    fn accessors_filter_empty_values() {
        with_back_office_sync(BackOfficeContext::new("", ""), || {
            assert_eq!(branch(), None);
            assert_eq!(operator(), None);
        });
    }

    /// The async scope helper mirrors Go's `context.WithValue` pair, and
    /// scopes nest like child contexts.
    #[tokio::test]
    async fn with_back_office_scopes_and_nests() {
        with_back_office(BackOfficeContext::new("b-outer", "o-outer"), async {
            assert_eq!(branch().as_deref(), Some("b-outer"));
            assert_eq!(operator().as_deref(), Some("o-outer"));
            with_back_office(BackOfficeContext::new("b-inner", "o-inner"), async {
                assert_eq!(branch().as_deref(), Some("b-inner"));
                assert_eq!(operator().as_deref(), Some("o-inner"));
            })
            .await;
            assert_eq!(branch().as_deref(), Some("b-outer"));
        })
        .await;
        assert_eq!(branch(), None);
    }

    // ---- starter wiring -----------------------------------------------------------

    /// New() renames the default starter to "starter-backoffice", with
    /// every other default falling back to the canonical names.
    #[test]
    fn defaults_fall_back_to_canonical_names() {
        let bo = BackOffice::new(CoreConfig::default());
        assert_eq!(bo.app_name, "firefly-app");
        assert_eq!(bo.starter_name, "starter-backoffice");
        assert_eq!(bo.log.service, "firefly-app");
    }

    /// An explicitly configured starter name survives New(), exactly
    /// like Go's `if app.StarterName == "starter-application"` guard.
    #[test]
    fn explicit_starter_name_preserved() {
        let bo = BackOffice::new(CoreConfig {
            starter_name: "starter-custom".into(),
            ..CoreConfig::default()
        });
        assert_eq!(bo.starter_name, "starter-custom");
    }

    /// An explicit "starter-application" is indistinguishable from the
    /// application default and becomes "starter-backoffice" — the exact
    /// Go semantics.
    #[test]
    fn explicit_starter_application_renamed() {
        let bo = BackOffice::new(CoreConfig {
            starter_name: "starter-application".into(),
            ..CoreConfig::default()
        });
        assert_eq!(bo.starter_name, "starter-backoffice");
    }

    /// The banner identifies the back-office tier, like Go's startup
    /// banner driven by `StarterName`.
    #[test]
    fn banner_identifies_backoffice_tier() {
        let bo = back_office();
        let banner = bo.banner();
        assert!(banner.contains("starter-backoffice"), "banner: {banner}");
        assert!(banner.contains("bo"), "banner: {banner}");
    }

    /// Deref reaches the embedded application and core, mirroring Go's
    /// two-level embedded-field promotion (reads and writes).
    #[tokio::test]
    async fn deref_reaches_application_and_core() {
        let mut bo = back_office();
        assert!(bo.plugins.names().is_empty()); // Application field via Deref
        assert_eq!(bo.cache.name(), "memory"); // Core field via double Deref
        assert_eq!(bo.new_application().name(), "bo"); // Core method via Deref
        bo.plugins.start_all().await.expect("start_all");
        bo.plugins.stop_all().await.expect("stop_all");
        bo.starter_name = "renamed".into(); // Core field via DerefMut
        assert_eq!(bo.app.core.starter_name, "renamed");
    }

    /// The core-only chain stays reachable through deref — Go's promoted
    /// `Middleware()` next to `MiddlewareChain()`.
    #[tokio::test]
    async fn core_only_chain_skips_back_office_guard() {
        let bo = back_office();
        let app = bo.apply_middleware(ok_router());
        let res = app.oneshot(request(&[])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "no back-office guard");
    }

    #[test]
    fn version_matches_workspace() {
        assert_eq!(VERSION, firefly_starter_application::VERSION);
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BackOffice>();
        assert_send_sync::<BackOfficeContext>();
        assert_send_sync::<BackOfficeLayer>();
    }
}
