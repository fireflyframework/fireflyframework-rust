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
//! ## pyfly parity: protocol clients
//!
//! Beyond REST, the crate ships the four thin protocol clients pyfly's
//! `client` package exposes:
//!
//! * [`GraphQlBuilder`] / [`GraphQlClient`] — POST `{ query, variables?,
//!   operationName? }`, raise [`ClientError::GraphQl`] on a non-empty
//!   `errors` array, decode `data` into a typed `T`. Always available
//!   (no extra deps).
//! * [`SoapBuilder`] / [`SoapClient`] — wrap a body in a SOAP 1.1
//!   envelope, POST `text/xml` with an optional `SOAPAction` header,
//!   return the raw response XML. Always available.
//! * `WsBuilder` / `WsClient` — connect / stream over
//!   `tokio-tungstenite`. Behind the `websocket` feature (so the links
//!   resolve only when that feature is enabled).
//! * `GrpcBuilder` — build a [`tonic`](https://docs.rs/tonic) channel
//!   for a caller-supplied generated stub. Behind the `grpc` feature
//!   (add `grpc-tls` for TLS).
//!
//! The legacy [`new_soap`] / [`new_grpc`] / [`new_websocket`] free
//! functions are retained for backward compatibility and still return
//! [`ClientError::TransportNotRegistered`] — they predate the typed
//! builders above, which are the supported entry points.
//!
//! ## Reactive surface: [`WebClient`]
//!
//! Alongside the eager [`RestClient`], the crate ships a **reactive**
//! HTTP client — the Rust analog of Spring WebFlux's `WebClient`, built
//! on [`firefly-reactive`](firefly_reactive)'s [`Mono`](firefly_reactive::Mono)
//! / [`Flux`](firefly_reactive::Flux). It is strictly additive: the
//! eager surface and its wire format are untouched.
//!
//! ```no_run
//! # async fn demo() {
//! use firefly_client::WebClientBuilder;
//! use http::Method;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct Tick { seq: u64 }
//!
//! let client = WebClientBuilder::new("https://api.example.com").build();
//!
//! // body_to_mono: a single value -> Mono<T>.
//! let one = client.method(Method::GET).uri("/latest")
//!     .retrieve().body_to_mono::<Tick>();
//!
//! // body_to_flux: a streamed NDJSON / SSE body -> Flux<T>, lazily,
//! // with backpressure.
//! let stream = client.method(Method::GET).uri("/ticks")
//!     .header("Accept", "application/x-ndjson")
//!     .retrieve().body_to_flux::<Tick>();
//! # let _ = (one, stream);
//! # }
//! ```
//!
//! See [`WebClient`], [`WebClientBuilder`], [`RequestSpec`],
//! [`ResponseSpec`], and [`WebClientResponse`] for the full surface.
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
//! * Injects the W3C `traceparent` / `tracestate` from the
//!   observability task-local scope when present (pyfly's httpx adapter
//!   behaviour), keeping the distributed trace unbroken across hops.
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
mod graphql;
mod rest;
mod scaffold;
mod soap;
mod webclient;

#[cfg(feature = "grpc")]
mod grpc;
#[cfg(feature = "websocket")]
mod websocket;

pub use error::ClientError;
pub use graphql::{no_variables, GraphQlBuilder, GraphQlClient};
pub use rest::{new_rest, RestBuilder, RestClient, NO_BODY};
pub use scaffold::{
    new_grpc, new_soap, new_websocket, GrpcPlaceholder, SoapPlaceholder, WebSocketPlaceholder,
};
pub use soap::{wrap_envelope, SoapBuilder, SoapClient};
pub use webclient::{
    new_web_client, RequestSpec, ResponseSpec, WebClient, WebClientBuilder, WebClientResponse,
    NDJSON_CONTENT_TYPE, SSE_CONTENT_TYPE,
};

#[cfg(feature = "grpc")]
pub use grpc::{GrpcBuilder, GrpcError};
#[cfg(feature = "websocket")]
pub use websocket::{WsBuilder, WsClient, WsStream};
