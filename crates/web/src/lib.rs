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
//! bootstrap ([`server::ServerProperties`] / [`server::serve`]).

mod content_negotiation;
mod correlation;
mod cors;
mod csrf;
mod globs;
mod headers;
mod idempotency;
mod metrics;
mod pii;
mod problem;
mod request_log;
pub mod server;

pub use content_negotiation::{
    default_message_converters, parse_accept, value_to_xml, xml_to_value, ContentNegotiationLayer,
    ContentNegotiationService, JsonMessageConverter, MessageConverter, MessageConverterRegistry,
    NegotiablePayload, Negotiate, XmlMessageConverter,
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
pub use headers::{SecurityHeadersConfig, SecurityHeadersLayer, SecurityHeadersService};
pub use idempotency::{
    IdempotencyConfig, IdempotencyLayer, IdempotencyRecord, IdempotencyService, IdempotencyStore,
    MemoryIdempotencyStore,
};
pub use metrics::{
    MetricsLayer, MetricsService, Outcome, RequestMetric, RequestObserver, RollingMax,
    HTTP_SERVER_REQUESTS_MAX_METRIC, HTTP_SERVER_REQUESTS_METRIC,
};
pub use pii::{mask_map, mask_pii};
pub use problem::{
    error_response, problem_response, ProblemLayer, ProblemService, WebError, WebResult,
};
pub use request_log::{RequestLogLayer, RequestLogService, REQUEST_LOG_TARGET};

/// The released framework version, mirroring [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;
