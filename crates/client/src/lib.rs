//! # firefly-client
//!
//! The framework's **outbound HTTP client builder** — a fluent
//! [`RestBuilder`] that composes timeouts, retries, default headers,
//! and correlation-id propagation into a [`RestClient`] whose
//! [`request`](RestClient::request) / [`send`](RestClient::send)
//! methods are the Rust spelling of the Go port's single
//! `Do(ctx, method, path, body, out)`. Non-2xx responses are decoded
//! into the kernel's [`FireflyError`](firefly_kernel::FireflyError)
//! (RFC 7807-aware), so every consumer of every external service sees
//! the same error shape.
//!
//! SOAP / gRPC / WebSocket builders share the same `new_*(endpoint)`
//! shape and currently fail with
//! [`ClientError::TransportNotRegistered`] — production adapters land
//! in dedicated transport modules.
//!
//! ## Why a separate crate?
//!
//! The Java `firefly-service-client` integrates Resilience4j + service
//! discovery + OAuth2 token caching + a GraphQL helper; ASP.NET defers
//! much of this to `IHttpClientFactory` plus Polly; Go settles on a
//! small stdlib builder. All ports converge on the same shape: a typed
//! builder that yields a typed client. This crate is the Rust
//! equivalent — small, `reqwest`-based, and composable with
//! `firefly-resilience` decorators.
//!
//! ## What every request does automatically
//!
//! * JSON-encodes the body (when present) and sets
//!   `Content-Type: application/json`.
//! * Sets `Accept: application/json`.
//! * Forwards the correlation id from the kernel task-local scope
//!   ([`firefly_kernel::with_correlation_id`]) as `X-Correlation-Id`.
//! * Retries on network errors and 429 / 5xx statuses with exponential
//!   backoff (100 ms doubling per attempt, capped at 2 s).
//! * Decodes RFC 7807 `application/problem+json` bodies into a typed
//!   [`FireflyError`](firefly_kernel::FireflyError) populated with
//!   `code`, `title`, `status`, `detail`, and `fields`.
//!
//! ## Quick start
//!
//! ```no_run
//! # async fn demo() -> Result<(), firefly_client::ClientError> {
//! use std::time::Duration;
//!
//! use firefly_client::{ClientError, RestBuilder};
//! use http::Method;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize)]
//! struct CreateOrder {
//!     customer: String,
//! }
//! #[derive(Deserialize)]
//! struct Order {
//!     id: String,
//!     customer: String,
//! }
//!
//! let client = RestBuilder::new("https://api.example.com")
//!     .with_header("X-Tenant", "acme")
//!     .with_timeout(Duration::from_secs(5))
//!     .with_retries(3)
//!     .build();
//!
//! let req = CreateOrder { customer: "acme".into() };
//! match client.request::<_, Order>(Method::POST, "/orders", Some(&req)).await {
//!     Ok(order) => println!("created {}", order.id),
//!     Err(err) => {
//!         if let Some(fe) = err.as_firefly() {
//!             eprintln!("upstream {}: {}", fe.status, fe.detail);
//!         }
//!         return Err(err);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod error;
mod rest;
mod scaffold;

pub use error::ClientError;
pub use rest::{new_rest, RestBuilder, RestClient, NO_BODY};
pub use scaffold::{new_grpc, new_soap, new_websocket, GrpcClient, SoapClient, WebSocketClient};
