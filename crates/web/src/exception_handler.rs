// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Error-to-response mapping hooks — the Rust analog of pyfly's
//! `@controller_advice` + `@exception_handler` (Spring's
//! `@ExceptionHandler` / `ResponseEntityExceptionHandler`).
//!
//! pyfly catches a *raised exception by Python type* and runs the
//! handler registered for the nearest type in its MRO, with
//! controller-local handlers overriding global `@controller_advice`
//! ones. Rust has no runtime exception hierarchy, so the equivalent
//! matching key is the [`ProblemDetail`]'s `type` member (the stable
//! error code the framework already assigns — `firefly_kernel`'s
//! `TYPE_NOT_FOUND`, `TYPE_VALIDATION`, …) or its HTTP status code.
//!
//! An [`ExceptionHandlerRegistry`] maps those matchers to a transform
//! that rewrites the outgoing [`ProblemDetail`] (and therefore the
//! status, title, and body of the RFC 7807 response). Register
//! framework-wide rules on a *global* registry, then layer
//! controller-specific overrides on top with
//! [`ExceptionHandlerRegistry::with_overrides`]: local rules win, exactly
//! like pyfly's controller-local-overrides-global precedence.
//!
//! The default RFC 7807 path (a [`crate::WebError`] rendered straight
//! through [`crate::problem_response`]) is unchanged; this is an opt-in
//! customization hook a migrating user reaches for only when a specific
//! error should surface as a *custom* status or body.
//!
//! ## Quick start
//!
//! ```
//! use firefly_kernel::{FireflyError, ProblemDetail, TYPE_NOT_FOUND};
//! use firefly_web::{ExceptionHandlerRegistry, WebError};
//!
//! // Global rule: turn every "not found" into a teapot with a friendly title.
//! let registry = ExceptionHandlerRegistry::new()
//!     .on_type(TYPE_NOT_FOUND, |pd: &ProblemDetail| {
//!         let mut out = pd.clone();
//!         out.status = 418;
//!         out.title = "Gone fishing".into();
//!         out
//!     });
//!
//! let err = WebError::from(FireflyError::not_found("order 7"));
//! let mapped = registry.map(&err).expect("a handler matched");
//! assert_eq!(mapped.status, 418);
//! assert_eq!(mapped.title, "Gone fishing");
//!
//! // Render straight to an axum response (custom status + RFC 7807 body):
//! let _response = registry.handle(&err);
//! ```

use std::convert::Infallible;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::response::Response;
use firefly_kernel::{ProblemDetail, PROBLEM_CONTENT_TYPE};
use futures::future::BoxFuture;
use http::{header, HeaderValue, Request, StatusCode};
use tower::{Layer, Service};

use crate::problem::{problem_response, WebError};

/// A boxed, shareable transform from an inbound [`ProblemDetail`] to the
/// one that should actually be rendered.
type Transform = Arc<dyn Fn(&ProblemDetail) -> ProblemDetail + Send + Sync>;

/// What a registered handler matches on. A handler keyed by problem
/// `type` is the closest Rust analog of pyfly keying by exception class;
/// a status-code handler is a coarser catch-all (the analog of handling
/// a broad base exception).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// Matches a [`ProblemDetail::problem_type`] exactly.
    Type(String),
    /// Matches a [`ProblemDetail::status`] code exactly.
    Status(u16),
}

/// One registered mapping: a matcher plus its transform.
#[derive(Clone)]
struct Handler {
    matcher: Matcher,
    transform: Transform,
}

/// A registry of error-to-response mappings — the `@controller_advice`
/// equivalent. Build it fluently with [`ExceptionHandlerRegistry::on_type`]
/// / [`ExceptionHandlerRegistry::on_status`], then resolve an outgoing
/// error with [`ExceptionHandlerRegistry::map`] or render it directly
/// with [`ExceptionHandlerRegistry::handle`].
///
/// **Precedence.** A by-`type` handler is more specific than a
/// by-status handler and is always tried first. Within the same matcher
/// kind the most recently registered handler wins, so
/// [`ExceptionHandlerRegistry::with_overrides`] layers controller-local
/// rules on top of a global registry with local-overrides-global
/// semantics.
#[derive(Clone, Default)]
pub struct ExceptionHandlerRegistry {
    // Stored most-recent-first so resolution is a simple front-to-back
    // scan; `with_overrides` prepends the local handlers.
    handlers: Vec<Handler>,
}

impl std::fmt::Debug for ExceptionHandlerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExceptionHandlerRegistry")
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl ExceptionHandlerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a handler that maps every error whose problem `type`
    /// equals `problem_type` through `transform`.
    ///
    /// Most recently registered wins among same-`type` handlers.
    #[must_use]
    pub fn on_type<F>(mut self, problem_type: impl Into<String>, transform: F) -> Self
    where
        F: Fn(&ProblemDetail) -> ProblemDetail + Send + Sync + 'static,
    {
        self.handlers.insert(
            0,
            Handler {
                matcher: Matcher::Type(problem_type.into()),
                transform: Arc::new(transform),
            },
        );
        self
    }

    /// Registers a handler that maps every error carrying HTTP `status`
    /// through `transform`. A coarser catch-all than [`Self::on_type`];
    /// by-`type` handlers are always tried first.
    #[must_use]
    pub fn on_status<F>(mut self, status: u16, transform: F) -> Self
    where
        F: Fn(&ProblemDetail) -> ProblemDetail + Send + Sync + 'static,
    {
        self.handlers.insert(
            0,
            Handler {
                matcher: Matcher::Status(status),
                transform: Arc::new(transform),
            },
        );
        self
    }

    /// Returns a new registry with `local`'s handlers taking precedence
    /// over `self`'s — the Rust spelling of pyfly's
    /// controller-local-overrides-global merge.
    ///
    /// `self` is the global advice; `local` is the per-controller
    /// registry. Local handlers are resolved first; global handlers fill
    /// in any case the controller did not override.
    #[must_use]
    pub fn with_overrides(&self, local: &ExceptionHandlerRegistry) -> ExceptionHandlerRegistry {
        let mut handlers = local.handlers.clone();
        handlers.extend(self.handlers.iter().cloned());
        ExceptionHandlerRegistry { handlers }
    }

    /// Whether any handler is registered.
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// How many handlers are registered.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Resolves the most specific handler for `err` and returns the
    /// transformed [`ProblemDetail`], or `None` when no handler matches
    /// (the default RFC 7807 path applies).
    ///
    /// By-`type` handlers are tried before by-status handlers, so a
    /// precise match always wins over a status catch-all.
    pub fn map(&self, err: &WebError) -> Option<ProblemDetail> {
        let pd = &err.0;
        self.resolve(pd).map(|transform| transform(pd))
    }

    /// Resolves and renders `err` to an axum [`Response`]. When a handler
    /// matches, the customized [`ProblemDetail`] is rendered (custom
    /// status + RFC 7807 body); otherwise the error renders unchanged via
    /// [`problem_response`].
    pub fn handle(&self, err: &WebError) -> Response {
        match self.map(err) {
            Some(pd) => problem_response(&pd),
            None => problem_response(&err.0),
        }
    }

    /// Applies the registry to an **already-rendered** [`ProblemDetail`]
    /// (re-parsed from an outgoing `application/problem+json` body), returning
    /// the transformed detail or `None` when no handler matches.
    ///
    /// This is the entry point used by [`ExceptionAdviceLayer`] to
    /// post-process every error response transparently — the missing piece that
    /// makes a registered registry behave like a global `@ControllerAdvice`
    /// without each handler opting in.
    pub fn apply(&self, pd: &ProblemDetail) -> Option<ProblemDetail> {
        self.resolve(pd).map(|transform| transform(pd))
    }

    /// Finds the transform for `pd`: a by-`type` match first (most
    /// specific), then a by-status match.
    fn resolve(&self, pd: &ProblemDetail) -> Option<Transform> {
        self.handlers
            .iter()
            .find(|h| matches!(&h.matcher, Matcher::Type(t) if *t == pd.problem_type))
            .or_else(|| {
                self.handlers
                    .iter()
                    .find(|h| matches!(&h.matcher, Matcher::Status(s) if *s == pd.status))
            })
            .map(|h| Arc::clone(&h.transform))
    }
}

/// The transparent **global exception-advice** tower layer — Spring's
/// `@ControllerAdvice` applied to *every* response, not per handler.
///
/// It wraps the whole handler chain: when an inner layer or handler emits an
/// `application/problem+json` response (a per-handler [`WebError`], or a
/// panic-recovered 500 from [`ProblemLayer`](crate::ProblemLayer)), this layer
/// re-parses the body back into a [`ProblemDetail`], runs it through the
/// configured [`ExceptionHandlerRegistry`], and — when a handler matches —
/// re-renders the customized status + body while preserving the response's
/// existing headers (correlation id, security headers, CORS). Non-problem
/// responses and unmatched problems pass through untouched.
///
/// `FireflyApplication` installs this automatically when an
/// [`ExceptionHandlerRegistry`] bean is registered, so a service gets global
/// advice with zero wiring.
#[derive(Clone)]
pub struct ExceptionAdviceLayer {
    registry: Arc<ExceptionHandlerRegistry>,
}

impl ExceptionAdviceLayer {
    /// Builds the layer from a registry (cloned into a shared `Arc`).
    pub fn new(registry: ExceptionHandlerRegistry) -> Self {
        Self {
            registry: Arc::new(registry),
        }
    }
}

impl<S> Layer<S> for ExceptionAdviceLayer {
    type Service = ExceptionAdviceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ExceptionAdviceService {
            inner,
            registry: Arc::clone(&self.registry),
        }
    }
}

/// The tower service produced by [`ExceptionAdviceLayer`].
#[derive(Clone)]
pub struct ExceptionAdviceService<S> {
    inner: S,
    registry: Arc<ExceptionHandlerRegistry>,
}

impl<S> Service<Request<Body>> for ExceptionAdviceService<S>
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
        // Ready-clone dance: the future owns a ready inner service.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let registry = Arc::clone(&self.registry);
        Box::pin(async move {
            let res = inner.call(req).await?;

            // Fast path: only buffer + re-parse `application/problem+json`.
            let is_problem = res
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.starts_with(PROBLEM_CONTENT_TYPE))
                .unwrap_or(false);
            if !is_problem {
                return Ok(res);
            }

            let (mut parts, body) = res.into_parts();
            let bytes = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(bytes) => bytes,
                // The body was consumed and cannot be reconstructed; surface a
                // generic problem rather than a torn-down connection.
                Err(_) => {
                    return Ok(problem_response(&ProblemDetail::internal(
                        "exception advice could not read the response body",
                    )));
                }
            };

            // Re-parse the problem body. A non-problem JSON shape (or invalid
            // JSON) passes through with its original bytes.
            let pd = match serde_json::from_slice::<ProblemDetail>(&bytes) {
                Ok(pd) => pd,
                Err(_) => return Ok(Response::from_parts(parts, Body::from(bytes))),
            };

            match registry.apply(&pd) {
                // A handler matched: re-render the customized status + body,
                // keeping the headers the chain already set.
                Some(mapped) => {
                    let mut out = serde_json::to_vec(&mapped).unwrap_or_default();
                    out.push(b'\n');
                    parts.status = StatusCode::from_u16(mapped.status)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                    parts.headers.remove(header::CONTENT_LENGTH);
                    parts.headers.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(PROBLEM_CONTENT_TYPE),
                    );
                    Ok(Response::from_parts(parts, Body::from(out)))
                }
                // No handler matched: emit the original problem unchanged.
                None => Ok(Response::from_parts(parts, Body::from(bytes))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use firefly_kernel::{FireflyError, TYPE_NOT_FOUND, TYPE_VALIDATION};
    use http::StatusCode;
    use http_body_util::BodyExt;

    fn not_found() -> WebError {
        WebError::from(FireflyError::not_found("order 7 not found"))
    }

    #[test]
    fn empty_registry_matches_nothing() {
        let registry = ExceptionHandlerRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.map(&not_found()).is_none());
    }

    #[test]
    fn on_type_maps_matching_error() {
        let registry = ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| {
            let mut out = pd.clone();
            out.status = 418;
            out.title = "Teapot".into();
            out
        });
        let mapped = registry.map(&not_found()).expect("handler matched");
        assert_eq!(mapped.status, 418);
        assert_eq!(mapped.title, "Teapot");
        // The original detail is preserved by the transform.
        assert_eq!(mapped.detail, "order 7 not found");
    }

    #[test]
    fn unmatched_type_falls_through() {
        let registry = ExceptionHandlerRegistry::new().on_type(TYPE_VALIDATION, |pd| pd.clone());
        // A not-found error has a different `type`, so nothing matches.
        assert!(registry.map(&not_found()).is_none());
    }

    #[test]
    fn type_handler_beats_status_handler() {
        let registry = ExceptionHandlerRegistry::new()
            .on_status(404, |pd| {
                let mut out = pd.clone();
                out.title = "by-status".into();
                out
            })
            .on_type(TYPE_NOT_FOUND, |pd| {
                let mut out = pd.clone();
                out.title = "by-type".into();
                out
            });
        // Even though both could match a 404 not-found, the by-type
        // handler is more specific and wins.
        assert_eq!(registry.map(&not_found()).unwrap().title, "by-type");
    }

    #[test]
    fn status_handler_is_a_catch_all() {
        let registry = ExceptionHandlerRegistry::new().on_status(404, |pd| {
            let mut out = pd.clone();
            out.detail = "masked".into();
            out
        });
        assert_eq!(registry.map(&not_found()).unwrap().detail, "masked");
    }

    #[test]
    fn most_recent_same_type_handler_wins() {
        let registry = ExceptionHandlerRegistry::new()
            .on_type(TYPE_NOT_FOUND, |pd| {
                let mut out = pd.clone();
                out.title = "first".into();
                out
            })
            .on_type(TYPE_NOT_FOUND, |pd| {
                let mut out = pd.clone();
                out.title = "second".into();
                out
            });
        assert_eq!(registry.map(&not_found()).unwrap().title, "second");
    }

    #[test]
    fn local_overrides_global() {
        let global = ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| {
            let mut out = pd.clone();
            out.title = "global".into();
            out
        });
        let local = ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| {
            let mut out = pd.clone();
            out.title = "local".into();
            out
        });
        let merged = global.with_overrides(&local);
        // Controller-local rule wins over the global advice.
        assert_eq!(merged.map(&not_found()).unwrap().title, "local");
    }

    #[test]
    fn local_falls_back_to_global_for_unhandled_types() {
        let global = ExceptionHandlerRegistry::new().on_type(TYPE_VALIDATION, |pd| {
            let mut out = pd.clone();
            out.title = "global-validation".into();
            out
        });
        // Local only handles not-found; validation falls back to global.
        let local = ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| pd.clone());
        let merged = global.with_overrides(&local);
        let validation = WebError::from(FireflyError::validation("bad field"));
        assert_eq!(merged.map(&validation).unwrap().title, "global-validation");
    }

    #[tokio::test]
    async fn handle_renders_custom_problem_response() {
        let registry = ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| {
            let mut out = pd.clone();
            out.status = 418;
            out
        });
        let res = registry.handle(&not_found());
        assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(
            res.headers().get(http::header::CONTENT_TYPE).unwrap(),
            firefly_kernel::PROBLEM_CONTENT_TYPE
        );
        let body = collect(res.into_body()).await;
        assert!(body.contains("\"status\":418"));
    }

    #[tokio::test]
    async fn handle_passes_unmatched_errors_through_unchanged() {
        let registry = ExceptionHandlerRegistry::new().on_type(TYPE_VALIDATION, |pd| pd.clone());
        let res = registry.handle(&not_found());
        // No handler matched → default RFC 7807 path, original 404 status.
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    async fn collect(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn registry_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExceptionHandlerRegistry>();
    }

    // ---- ExceptionAdviceLayer (transparent global advice) -------------------

    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn teapot_registry() -> ExceptionHandlerRegistry {
        ExceptionHandlerRegistry::new().on_type(TYPE_NOT_FOUND, |pd| {
            let mut out = pd.clone();
            out.status = 418;
            out.title = "Teapot".into();
            out
        })
    }

    async fn problem_404_handler() -> Response {
        problem_response(&firefly_kernel::ProblemDetail::not_found(
            "order 7 not found",
        ))
    }

    #[tokio::test]
    async fn advice_layer_transforms_a_matching_problem_response() {
        let app = Router::new()
            .route("/x", get(problem_404_handler))
            .layer(ExceptionAdviceLayer::new(teapot_registry()));
        let res = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // The 404 problem was post-processed into the registry's 418 teapot.
        assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(
            res.headers().get(http::header::CONTENT_TYPE).unwrap(),
            firefly_kernel::PROBLEM_CONTENT_TYPE
        );
        let body = collect(res.into_body()).await;
        assert!(body.contains("\"status\":418"), "body: {body}");
        assert!(body.contains("Teapot"), "body: {body}");
        // The original detail survives the transform.
        assert!(body.contains("order 7 not found"), "body: {body}");
    }

    #[tokio::test]
    async fn advice_layer_passes_non_problem_responses_through_untouched() {
        async fn ok_handler() -> &'static str {
            "hello"
        }
        let app = Router::new()
            .route("/x", get(ok_handler))
            .layer(ExceptionAdviceLayer::new(teapot_registry()));
        let res = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(collect(res.into_body()).await, "hello");
    }

    #[tokio::test]
    async fn advice_layer_leaves_unmatched_problems_unchanged() {
        // The registry only handles validation; a not-found passes through 404.
        let registry = ExceptionHandlerRegistry::new().on_type(TYPE_VALIDATION, |pd| pd.clone());
        let app = Router::new()
            .route("/x", get(problem_404_handler))
            .layer(ExceptionAdviceLayer::new(registry));
        let res = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = collect(res.into_body()).await;
        assert!(body.contains("order 7 not found"), "body: {body}");
    }

    #[tokio::test]
    async fn advice_layer_preserves_existing_response_headers() {
        async fn handler() -> Response {
            let mut res = problem_response(&firefly_kernel::ProblemDetail::not_found("missing"));
            res.headers_mut()
                .insert("x-correlation-id", http::HeaderValue::from_static("abc123"));
            res
        }
        let app = Router::new()
            .route("/x", get(handler))
            .layer(ExceptionAdviceLayer::new(teapot_registry()));
        let res = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        // Re-rendered to 418, but the correlation header set upstream survives.
        assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(res.headers().get("x-correlation-id").unwrap(), "abc123");
    }
}
