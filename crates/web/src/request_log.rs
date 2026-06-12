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

//! Structured request access-log middleware — the Rust port of pyfly's
//! `RequestLoggingFilter` (order `HIGHEST_PRECEDENCE + 200`).
//!
//! Emits exactly one `tracing` event per request on target
//! [`REQUEST_LOG_TARGET`]:
//!
//! * `http_request` (INFO) — method, path, status_code, duration_ms,
//!   transaction_id, correlation_id;
//! * `http_request_failed` (ERROR) — when the handler panics; the panic
//!   is re-raised so the outer [`crate::ProblemLayer`] still renders the
//!   recovered 500.
//!
//! Field names match the pyfly structlog event field-for-field
//! (`method`, `path`, `status_code`, `duration_ms`, `transaction_id`).

use std::convert::Infallible;
use std::panic::{resume_unwind, AssertUnwindSafe};
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::response::Response;
use futures::future::BoxFuture;
use futures::FutureExt;
use http::Request;
use tower::{Layer, Service};

use crate::correlation::CorrelationContext;

/// The `tracing` target the access-log events are emitted on — filter
/// with `RUST_LOG=firefly_web::request_log=info`.
pub const REQUEST_LOG_TARGET: &str = "firefly_web::request_log";

/// Logs HTTP method, path, status code, and duration for each request.
/// Reads the [`CorrelationContext`] request extension (when
/// [`crate::CorrelationLayer`] sits outside this layer) to enrich the
/// event with `transaction_id` and `correlation_id`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestLogLayer;

impl RequestLogLayer {
    /// Returns the layer. It carries no state.
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for RequestLogLayer {
    type Service = RequestLogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestLogService { inner }
    }
}

/// The tower service produced by [`RequestLogLayer`].
#[derive(Debug, Clone)]
pub struct RequestLogService<S> {
    inner: S,
}

impl<S> Service<Request<Body>> for RequestLogService<S>
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
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        let method = req.method().to_string();
        let path = req.uri().path().to_string();
        let (transaction_id, correlation_id) = req
            .extensions()
            .get::<CorrelationContext>()
            .map(|ctx| (ctx.transaction_id.clone(), ctx.correlation_id.clone()))
            .unzip();

        Box::pin(async move {
            let start = Instant::now();
            let result = AssertUnwindSafe(inner.call(req)).catch_unwind().await;
            let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
            match result {
                Ok(res) => {
                    let res = res?;
                    tracing::info!(
                        target: REQUEST_LOG_TARGET,
                        method = %method,
                        path = %path,
                        status_code = res.status().as_u16(),
                        duration_ms = (duration_ms * 100.0).round() / 100.0,
                        transaction_id = transaction_id.as_deref(),
                        correlation_id = correlation_id.as_deref(),
                        "http_request"
                    );
                    Ok(res)
                }
                Err(payload) => {
                    let error = panic_message(payload.as_ref());
                    tracing::error!(
                        target: REQUEST_LOG_TARGET,
                        method = %method,
                        path = %path,
                        duration_ms = (duration_ms * 100.0).round() / 100.0,
                        transaction_id = transaction_id.as_deref(),
                        correlation_id = correlation_id.as_deref(),
                        error = %error,
                        "http_request_failed"
                    );
                    resume_unwind(payload)
                }
            }
        })
    }
}

/// Best-effort extraction of the human-readable panic message.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}
