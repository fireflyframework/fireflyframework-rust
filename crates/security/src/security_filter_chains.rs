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

//! Multiple security filter chains — the Rust analog of Spring Security's
//! several `SecurityFilterChain` beans behind a `FilterChainProxy`.
//!
//! A real app often needs different authorization rules for different URL
//! spaces — a stateless, deny-by-default `/api/**` and a more permissive web
//! surface, say. [`SecurityFilterChains`] holds an *ordered* list of
//! ([`RequestMatcher`], [`FilterChain`](crate::FilterChain)) pairs; for each
//! request the **first** chain whose matcher matches handles it (and *only*
//! that chain runs) — Spring's first-match-wins `FilterChainProxy`. A request
//! that matches **no** chain passes through untouched (no authorization
//! applied), so declare a catch-all [`any`](SecurityFilterChains::any) chain
//! last when you want a fail-closed tail.
//!
//! This dispatches the *authorization* [`FilterChain`](crate::FilterChain) per
//! request. Authentication layers ([`BearerLayer`](crate::BearerLayer),
//! [`SessionAuthenticationLayer`](crate::SessionAuthenticationLayer)) compose
//! around it as usual; for fully distinct *authentication* per URL space, the
//! idiomatic complement is an axum `Router::nest` with per-router layers.
//!
//! ```rust,no_run
//! use firefly_security::{
//!     AnyRequestMatcher, FilterChain, PathRequestMatcher, SecurityFilterChains,
//! };
//!
//! let security = SecurityFilterChains::new()
//!     // /api/** — locked down, deny-by-default.
//!     .chain(
//!         PathRequestMatcher::new("/api"),
//!         FilterChain::new().require_pattern("/api/**", &["API"]),
//!     )
//!     // everything else — public.
//!     .any(FilterChain::new().any_request_permit())
//!     .layer();
//! // app.layer(security) ...
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use http::Method;
use tower::{Layer, Service, ServiceExt};

use crate::authentication::SecurityError;
use crate::filter_chain::{prefix_matches, FilterChain, FilterChainLayer, FilterChainService};

/// Decides whether a security filter chain applies to a request — Spring's
/// `RequestMatcher`.
pub trait RequestMatcher: Send + Sync {
    /// Whether this matcher selects `request`.
    fn matches(&self, request: &Request) -> bool;
}

/// Matches every request — Spring's `AnyRequestMatcher`. Use it for a
/// catch-all (last) chain.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnyRequestMatcher;

impl RequestMatcher for AnyRequestMatcher {
    fn matches(&self, _request: &Request) -> bool {
        true
    }
}

/// Matches by path prefix (path-segment aware, like Spring's
/// `AntPathRequestMatcher`) and, optionally, an HTTP method. `/api` matches
/// `/api` and `/api/...` but not `/apixyz`.
#[derive(Debug, Clone)]
pub struct PathRequestMatcher {
    method: Option<Method>,
    prefix: String,
}

impl PathRequestMatcher {
    /// Matches any method under the path `prefix`.
    #[must_use]
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            method: None,
            prefix: prefix.into(),
        }
    }

    /// Matches only `method` requests under the path `prefix`.
    #[must_use]
    pub fn method(method: Method, prefix: impl Into<String>) -> Self {
        Self {
            method: Some(method),
            prefix: prefix.into(),
        }
    }
}

impl RequestMatcher for PathRequestMatcher {
    fn matches(&self, request: &Request) -> bool {
        if let Some(m) = &self.method {
            if request.method() != m {
                return false;
            }
        }
        prefix_matches(request.uri().path(), &self.prefix)
    }
}

/// An ordered set of ([`RequestMatcher`], [`FilterChain`](crate::FilterChain))
/// pairs — Spring's list of `SecurityFilterChain`s behind a `FilterChainProxy`.
/// The first matching chain handles each request.
#[derive(Default)]
pub struct SecurityFilterChains {
    chains: Vec<(Arc<dyn RequestMatcher>, FilterChain)>,
}

impl SecurityFilterChains {
    /// An empty proxy (passes every request through until a chain is added).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a chain guarded by `matcher`. Order matters: earlier chains win.
    #[must_use]
    pub fn chain(mut self, matcher: impl RequestMatcher + 'static, chain: FilterChain) -> Self {
        self.chains.push((Arc::new(matcher), chain));
        self
    }

    /// Appends a catch-all chain (matches every request) — Spring's
    /// `securityMatcher` omitted. Declare it last.
    #[must_use]
    pub fn any(self, chain: FilterChain) -> Self {
        self.chain(AnyRequestMatcher, chain)
    }

    /// Compiles every chain into a dispatching tower [`Layer`].
    ///
    /// # Panics
    ///
    /// Panics if any chain has an invalid glob pattern. Use
    /// [`try_layer`](Self::try_layer) to surface that as a recoverable error.
    #[must_use]
    pub fn layer(self) -> SecurityFilterChainsLayer {
        self.try_layer()
            .expect("firefly/security: invalid glob pattern in a SecurityFilterChains chain")
    }

    /// Compiles every chain into a dispatching tower [`Layer`], returning a
    /// recoverable [`SecurityError`] if any chain has an invalid glob pattern.
    pub fn try_layer(self) -> Result<SecurityFilterChainsLayer, SecurityError> {
        let mut compiled = Vec::with_capacity(self.chains.len());
        for (matcher, chain) in self.chains {
            compiled.push((matcher, chain.try_layer()?));
        }
        Ok(SecurityFilterChainsLayer {
            chains: Arc::new(compiled),
        })
    }
}

/// The tower layer produced by [`SecurityFilterChains::layer`].
#[derive(Clone)]
pub struct SecurityFilterChainsLayer {
    chains: Arc<Vec<(Arc<dyn RequestMatcher>, FilterChainLayer)>>,
}

impl<S> Layer<S> for SecurityFilterChainsLayer
where
    S: Clone,
{
    type Service = SecurityFilterChainsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        // Pre-apply each chain's authorization layer to its own clone of the
        // inner service, so dispatch is a cheap matcher scan at request time.
        let chains = self
            .chains
            .iter()
            .map(|(matcher, layer)| (Arc::clone(matcher), layer.layer(inner.clone())))
            .collect();
        SecurityFilterChainsService { inner, chains }
    }
}

/// The tower service produced by [`SecurityFilterChainsLayer`]. Selects the
/// first chain whose [`RequestMatcher`] matches; if none match, passes the
/// request through to the inner service unmodified.
#[derive(Clone)]
pub struct SecurityFilterChainsService<S> {
    inner: S,
    chains: Vec<(Arc<dyn RequestMatcher>, FilterChainService<S>)>,
}

impl<S> Service<Request> for SecurityFilterChainsService<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        // First-match-wins (Spring's FilterChainProxy): scan synchronously.
        let selected = self.chains.iter().position(|(m, _)| m.matches(&req));
        match selected {
            Some(i) => {
                // Call a fresh clone of the chosen chain service, but honor
                // tower's readiness contract first: `poll_ready` above only
                // readied `self.inner` (used on the no-match branch), so drive
                // this clone ready (its `poll_ready` readies its own wrapped
                // inner) before `call` — correct even for a backpressure-bearing
                // inner service, not only always-ready ones.
                let mut svc = self.chains[i].1.clone();
                Box::pin(async move { svc.ready().await?.call(req).await })
            }
            None => {
                let clone = self.inner.clone();
                let mut inner = std::mem::replace(&mut self.inner, clone);
                Box::pin(async move { inner.call(req).await })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn status(proxy: SecurityFilterChains, method: &str, path: &str) -> http::StatusCode {
        let inner = tower::service_fn(|_req: Request| async {
            Ok::<Response, Infallible>(Response::new(axum::body::Body::empty()))
        });
        let svc = proxy.layer().layer(inner);
        let req = Request::builder()
            .method(method)
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap();
        svc.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn first_matching_chain_handles_the_request() {
        // /api/** routes to a deny-all chain; everything else to a permit-all
        // chain — proving the matcher selects which chain's rules apply.
        let proxy = || {
            SecurityFilterChains::new()
                .chain(
                    PathRequestMatcher::new("/api"),
                    FilterChain::new().any_request_deny(),
                )
                .any(FilterChain::new().any_request_permit())
        };
        assert_eq!(
            status(proxy(), "GET", "/api/users").await,
            http::StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(proxy(), "GET", "/web/home").await,
            http::StatusCode::OK
        );
    }

    #[tokio::test]
    async fn earlier_chain_wins_over_a_later_overlapping_one() {
        // Both chains match /api; the first (deny) decides.
        let proxy = SecurityFilterChains::new()
            .chain(
                PathRequestMatcher::new("/api"),
                FilterChain::new().any_request_deny(),
            )
            .any(FilterChain::new().any_request_permit());
        assert_eq!(
            status(proxy, "GET", "/api/x").await,
            http::StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn unmatched_request_passes_through() {
        // Only an /api chain is declared; an unmatched path is served by the
        // inner service (no authorization applied) — Spring's FilterChainProxy.
        let proxy = SecurityFilterChains::new().chain(
            PathRequestMatcher::new("/api"),
            FilterChain::new().any_request_deny(),
        );
        assert_eq!(status(proxy, "GET", "/other").await, http::StatusCode::OK);
        // ...and the /api chain still denies.
        assert_eq!(
            status(proxy_again(), "GET", "/api/x").await,
            http::StatusCode::FORBIDDEN
        );
    }

    fn proxy_again() -> SecurityFilterChains {
        SecurityFilterChains::new().chain(
            PathRequestMatcher::new("/api"),
            FilterChain::new().any_request_deny(),
        )
    }

    #[tokio::test]
    async fn method_scoped_matcher_selects_by_verb() {
        let proxy = || {
            SecurityFilterChains::new()
                .chain(
                    PathRequestMatcher::method(Method::POST, "/data"),
                    FilterChain::new().any_request_deny(),
                )
                .any(FilterChain::new().any_request_permit())
        };
        // POST /data → deny chain; GET /data → falls through to permit chain.
        assert_eq!(
            status(proxy(), "POST", "/data").await,
            http::StatusCode::FORBIDDEN
        );
        assert_eq!(status(proxy(), "GET", "/data").await, http::StatusCode::OK);
    }

    #[test]
    fn try_layer_surfaces_invalid_glob_as_error() {
        let bad = SecurityFilterChains::new()
            .any(FilterChain::new().require_pattern("/admin/[", &["ADMIN"]))
            .try_layer();
        assert!(bad.is_err());
    }

    // An inner service that requires the tower readiness handshake: `call`
    // panics unless `poll_ready` was driven on the same instance first (like
    // tower's Buffer/ConcurrencyLimit, which reserve a permit in poll_ready).
    #[derive(Clone)]
    struct ReadinessGated {
        ready: bool,
    }

    impl Service<Request> for ReadinessGated {
        type Response = Response;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.ready = true;
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _req: Request) -> Self::Future {
            assert!(
                self.ready,
                "call() invoked before poll_ready() — readiness contract violated"
            );
            self.ready = false;
            Box::pin(async { Ok(Response::new(axum::body::Body::empty())) })
        }
    }

    #[tokio::test]
    async fn matched_chain_drives_inner_to_readiness_before_calling() {
        // Regression: the matched-chain dispatch path must drive the chosen
        // chain service ready before calling it. With a readiness-gated inner,
        // a missing handshake panics in `call`.
        let proxy = SecurityFilterChains::new()
            .chain(
                PathRequestMatcher::new("/api"),
                FilterChain::new().any_request_permit(),
            )
            .layer();
        let svc = proxy.layer(ReadinessGated { ready: false });
        let req = Request::builder()
            .method("GET")
            .uri("/api/users")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            svc.oneshot(req).await.unwrap().status(),
            http::StatusCode::OK
        );
    }
}
