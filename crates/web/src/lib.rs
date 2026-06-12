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

mod correlation;
mod idempotency;
mod pii;
mod problem;

pub use correlation::{CorrelationId, CorrelationLayer, CorrelationService};
pub use idempotency::{
    IdempotencyConfig, IdempotencyLayer, IdempotencyRecord, IdempotencyService, IdempotencyStore,
    MemoryIdempotencyStore,
};
pub use pii::{mask_map, mask_pii};
pub use problem::{
    error_response, problem_response, ProblemLayer, ProblemService, WebError, WebResult,
};

/// The released framework version, mirroring [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;
