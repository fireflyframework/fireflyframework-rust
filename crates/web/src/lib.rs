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

//! # firefly-web
//!
//! The HTTP-layer middleware tier of the Firefly Framework for Rust —
//! the port of the Go `web` module (Java original: `firefly-web` +
//! `firefly-spring-utils`). It converts errors into RFC 7807
//! `application/problem+json` responses, propagates correlation IDs,
//! replays idempotent requests, and scrubs PII out of log lines.
//! Composed at the outermost edge of every Firefly service.
//!
//! Every middleware is a [`tower::Layer`], so it composes with axum and
//! any tower-compatible stack — the Rust analog of the Go module's
//! `func(http.Handler) http.Handler` middlewares.
//!
//! ## The canonical chain
//!
//! ```text
//! incoming request
//!       │
//!       ▼
//! ┌─────────────────────────────────────────┐
//! │ ProblemLayer      (panic → 500 RFC7807) │
//! └─────────────────────────────────────────┘
//!       │
//!       ▼
//! ┌─────────────────────────────────────────┐
//! │ CorrelationLayer  (X-Correlation-Id)    │
//! └─────────────────────────────────────────┘
//!       │
//!       ▼
//! ┌─────────────────────────────────────────┐
//! │ IdempotencyLayer  (replay if Key)       │
//! └─────────────────────────────────────────┘
//!       │
//!       ▼
//!    your router
//! ```
//!
//! Wire formats — header names (`X-Correlation-Id`, `Idempotency-Key`,
//! `Idempotent-Replay`), problem JSON bytes, status codes, and conflict
//! semantics — are identical to the Java, .NET, Go, and Python ports.
//!
//! ## Quick start
//!
//! ```
//! use axum::{routing::post, Router};
//! use firefly_kernel::FireflyError;
//! use firefly_web::{CorrelationLayer, IdempotencyLayer, ProblemLayer, WebResult};
//! use tower::ServiceBuilder;
//!
//! async fn create_order() -> WebResult<&'static str> {
//!     // `?` on any FireflyResult also works — FireflyError converts
//!     // into WebError and renders as application/problem+json.
//!     Err(FireflyError::bad_request("customer is required").into())
//! }
//!
//! let app: Router = Router::new()
//!     .route("/orders", post(create_order))
//!     .layer(
//!         // ServiceBuilder applies top-down: ProblemLayer is outermost.
//!         ServiceBuilder::new()
//!             .layer(ProblemLayer::new())
//!             .layer(CorrelationLayer::new())
//!             .layer(IdempotencyLayer::default()),
//!     );
//! # let _ = app;
//! ```

//! ## pyfly parity layer
//!
//! Beyond the Go-parity chain above, this crate ships the pyfly web +
//! server surface: [`CorsLayer`], [`SecurityHeadersLayer`],
//! [`CsrfLayer`], [`RequestLogLayer`], [`MetricsLayer`] (with the
//! pluggable [`RequestObserver`]), the extended [`CorrelationContext`]
//! (`X-Request-Id`, `X-Tenant-Id`, `X-Transaction-Id`,
//! `traceparent`/`tracestate`), `Accept`-driven content negotiation
//! ([`MessageConverterRegistry`], [`Negotiate`],
//! [`ContentNegotiationLayer`]) and the config-driven [`server`]
//! bootstrap ([`server::ServerProperties`] / [`server::serve`]). The
//! [`ExceptionHandlerRegistry`] is the `@controller_advice` /
//! `@exception_handler` equivalent: it maps a domain error (by problem
//! `type` or status) to a custom RFC 7807 response, with
//! controller-local rules overriding global advice.
//!
//! ## Reactive (WebFlux/Reactor) surface
//!
//! The [`reactive`] module adds an **additive** reactive HTTP surface on
//! top of [`firefly_reactive`] — the Rust analog of returning `Mono<T>` /
//! `Flux<T>` from a Spring WebFlux `@RestController`: [`MonoJson`]
//! (resolves a `Mono` → `200` JSON / `404` problem / error problem),
//! [`NdJson`] (`Flux<T>` → backpressured `application/x-ndjson`), and
//! [`Sse`] / [`SseEvents`] (`Flux<T>` → `text/event-stream`, reusing
//! `firefly-sse`'s wire format). See the module docs for details.

mod content_negotiation;
mod controllers;
mod correlation;
mod cors;
mod csrf;
mod exception_handler;
mod extract;
mod globs;
mod headers;
mod idempotency;
mod json_mapper;
mod metrics;
mod multipart;
mod pageable;
mod pii;
mod problem;
pub mod reactive;
mod request_log;
pub mod server;
mod valid;

// Re-export the DI container's `inventory` registry + `Container` so the
// `#[rest_controller]` macro's generated `ControllerMount` thunks resolve
// through `firefly_web` (and, via the facade, `firefly::web`).
pub use firefly_container::{inventory, Container};

pub use content_negotiation::{
    default_message_converters, parse_accept, value_to_xml, xml_to_value, ContentNegotiationLayer,
    ContentNegotiationService, JsonMessageConverter, MessageConverter, MessageConverterRegistry,
    NegotiablePayload, Negotiate, XmlMessageConverter,
};
pub use controllers::{
    controller_count, mount_controllers, mount_route_contributors, ControllerMount,
    RouteContributor,
};
pub use correlation::{
    current_correlation_context, with_correlation_context, CorrelationContext, CorrelationId,
    CorrelationLayer, CorrelationService, HEADER_REQUEST_ID, HEADER_TENANT_ID, HEADER_TRACEPARENT,
    HEADER_TRACESTATE, HEADER_TRANSACTION_ID,
};
pub use cors::{CorsConfig, CorsLayer, CorsService, PERMIT_DEFAULT_METHODS};
pub use csrf::{
    generate_csrf_token, validate_csrf_token, CsrfLayer, CsrfService, CSRF_COOKIE_NAME,
    CSRF_HEADER_NAME, CSRF_SAFE_METHODS,
};
pub use exception_handler::{
    ExceptionAdviceLayer, ExceptionAdviceService, ExceptionHandlerRegistry,
};
pub use extract::{Path, Query, ValidPath, ValidQuery};
pub use headers::{SecurityHeadersConfig, SecurityHeadersLayer, SecurityHeadersService};
pub use idempotency::{
    IdempotencyConfig, IdempotencyLayer, IdempotencyRecord, IdempotencyService, IdempotencyStore,
    MemoryIdempotencyStore,
};
pub use json_mapper::{Inclusion, MappingJsonConverter, ObjectMapper, PropertyNaming};
pub use metrics::{
    MetricsLayer, MetricsService, Outcome, RequestMetric, RequestObserver, RollingMax,
    HTTP_SERVER_REQUESTS_MAX_METRIC, HTTP_SERVER_REQUESTS_METRIC,
};
pub use multipart::{Multipart, UploadedFile};
pub use pageable::PageRequest;
pub use pii::{mask_map, mask_pii};
pub use problem::{
    error_response, problem_response, ProblemLayer, ProblemService, WebError, WebResult,
};
pub use reactive::{MonoJson, NdJson, Sse, SseEvents, NDJSON_CONTENT_TYPE, SSE_CONTENT_TYPE};
pub use request_log::{RequestLogLayer, RequestLogService, REQUEST_LOG_TARGET};
pub use valid::Valid;

/// The released framework version, mirroring [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;
