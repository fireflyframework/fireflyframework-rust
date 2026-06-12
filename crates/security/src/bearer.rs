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

//! Bearer-token extraction middleware — the Rust analog of the Go
//! port's `BearerMiddleware`.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use tower::{Layer, Service};

use crate::authentication::{Authentication, SecurityError, Verifier};
use crate::problem;

/// `UnauthorizedHandler` customizes how rejections are rendered — the
/// Rust analog of Go's `UnauthorizedFunc(w, r, err)`. It receives the
/// rejected request and the error, and returns the response to send.
pub type UnauthorizedHandler = Arc<dyn Fn(&Request, &SecurityError) -> Response + Send + Sync>;

/// `BearerConfig` tunes [`BearerLayer`].
///
/// Where the Go zero value panics on a nil `Verifier`, the Rust
/// constructor takes the verifier up front — there is no nil case.
#[derive(Clone)]
pub struct BearerConfig {
    /// The authentication port consulted for each token.
    pub verifier: Arc<dyn Verifier>,
    /// When true, a missing token does not 401 — the request proceeds
    /// with the [`Authentication::anonymous`] principal attached.
    pub allow_anonymous: bool,
    /// Header to read the token from; empty means `"Authorization"`.
    pub header_name: String,
    /// Custom rejection renderer; `None` emits the canonical 401
    /// `application/problem+json` envelope.
    pub unauthorized: Option<UnauthorizedHandler>,
}

impl BearerConfig {
    /// Builds a config around `verifier` with Go-parity defaults:
    /// anonymous access off, `Authorization` header, problem+json
    /// rejections.
    pub fn new(verifier: impl Verifier + 'static) -> Self {
        Self {
            verifier: Arc::new(verifier),
            allow_anonymous: false,
            header_name: String::new(),
            unauthorized: None,
        }
    }

    /// Sets whether a missing token falls through as anonymous.
    pub fn allow_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }

    /// Overrides the header the token is read from
    /// (default `"Authorization"`).
    pub fn header_name(mut self, name: impl Into<String>) -> Self {
        self.header_name = name.into();
        self
    }

    /// Installs a custom rejection renderer.
    pub fn unauthorized(
        mut self,
        f: impl Fn(&Request, &SecurityError) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.unauthorized = Some(Arc::new(f));
        self
    }

    /// Renders a rejection through the custom handler or the canonical
    /// 401 problem envelope.
    fn reject(&self, req: &Request, err: &SecurityError) -> Response {
        match &self.unauthorized {
            Some(f) => f(req, err),
            None => problem::unauthorized(&err.to_string()),
        }
    }
}

/// `BearerLayer` extracts an `Authorization: Bearer <token>` header,
/// calls the configured [`Verifier`], and stores the resulting
/// [`Authentication`] on the request extensions. Failures emit a 401
/// `application/problem+json` response unless
/// [`BearerConfig::allow_anonymous`] is set.
///
/// Apply it to an axum `Router` with `.layer(...)`; remember axum runs
/// the **last-added layer first**, so add the bearer layer after any
/// [`FilterChain`](crate::FilterChain) layer that consumes its output.
#[derive(Clone)]
pub struct BearerLayer {
    cfg: Arc<BearerConfig>,
}

impl BearerLayer {
    /// Wraps `cfg` into a reusable tower layer
    /// (Go: `BearerMiddleware(cfg)`).
    pub fn new(cfg: BearerConfig) -> Self {
        Self { cfg: Arc::new(cfg) }
    }
}

impl<S> Layer<S> for BearerLayer {
    type Service = BearerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BearerService {
            inner,
            cfg: Arc::clone(&self.cfg),
        }
    }
}

/// The tower service produced by [`BearerLayer`].
#[derive(Clone)]
pub struct BearerService<S> {
    inner: S,
    cfg: Arc<BearerConfig>,
}

impl<S> Service<Request> for BearerService<S>
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

    fn call(&mut self, mut req: Request) -> Self::Future {
        let cfg = Arc::clone(&self.cfg);
        // Take the service that was driven to readiness; leave a fresh
        // clone behind (standard tower buffering pattern).
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let header = if cfg.header_name.is_empty() {
                "Authorization"
            } else {
                cfg.header_name.as_str()
            };
            // Owned copy so the borrow of `req` ends before we mutate it.
            let raw: Option<Result<String, ()>> = req
                .headers()
                .get(header)
                .map(|v| v.to_str().map(str::to_owned).map_err(|_| ()));
            let raw = match raw {
                // Go's Header.Get returns "" for both a missing header
                // and an empty value — treat them identically.
                None => None,
                Some(Ok(s)) if s.is_empty() => None,
                Some(Ok(s)) => Some(s),
                // Non-UTF-8 header bytes can never be `Bearer <token>`.
                Some(Err(())) => {
                    return Ok(cfg.reject(&req, &SecurityError::MalformedHeader));
                }
            };

            let Some(raw) = raw else {
                if cfg.allow_anonymous {
                    req.extensions_mut().insert(Authentication::anonymous());
                    return inner.call(req).await;
                }
                return Ok(cfg.reject(&req, &SecurityError::Unauthenticated));
            };

            let Some(token) = raw.strip_prefix("Bearer ") else {
                return Ok(cfg.reject(&req, &SecurityError::MalformedHeader));
            };

            match cfg.verifier.verify(token).await {
                Ok(auth) => {
                    req.extensions_mut().insert(auth);
                    inner.call(req).await
                }
                Err(err) => Ok(cfg.reject(&req, &err)),
            }
        })
    }
}
