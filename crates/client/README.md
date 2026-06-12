# `firefly-client`

> **Tier:** Adapter · **Status:** REST Full; SOAP / gRPC / WS scaffolds · **Go module:** `client` · **Java original:** `firefly-service-client`

## Overview

`firefly-client` is the framework's **outbound HTTP client builder** — a
fluent [`RestBuilder`] that composes timeouts, retries, default headers,
and correlation-id propagation into a [`RestClient`] whose
`request` / `send` methods are the Rust spelling of the Go port's single
`Do(ctx, method, path, body, out)`. Non-2xx responses are decoded into
the kernel's `FireflyError` (RFC 7807-aware), so every consumer of every
external service sees the same error shape.

SOAP / gRPC / WebSocket builders share the same `new_*(endpoint)` shape
and currently fail with `ClientError::TransportNotRegistered` —
production adapters land in dedicated transport modules.

## Why a separate crate?

The Java `firefly-service-client` integrates Resilience4j + service
discovery + OAuth2 token caching + a GraphQL helper. ASP.NET defers much
of this to `IHttpClientFactory` plus Polly. Go settles on a small stdlib
builder. All ports converge on the same shape: a typed builder that
yields a typed client. This crate is the Rust equivalent — small,
`reqwest`-based, composable with `firefly-resilience` decorators.

## Public surface

```rust,ignore
pub struct RestBuilder { /* … */ }
pub fn new_rest(base_url: impl AsRef<str>) -> RestBuilder; // Go: NewREST
impl RestBuilder {
    pub fn new(base_url: impl AsRef<str>) -> Self;
    pub fn with_header(self, key, value) -> Self;          // Go: WithHeader
    pub fn with_timeout(self, Duration) -> Self;           // Go: WithTimeout
    pub fn with_http_client(self, reqwest::Client) -> Self; // Go: WithHTTPClient
    pub fn with_retries(self, attempts: usize) -> Self;    // Go: WithRetries
    pub fn with_backoff_base(self, Duration) -> Self;      // Rust extension
    pub fn build(self) -> RestClient;
}

pub struct RestClient { /* … */ }
impl RestClient {
    // Go's Do(ctx, method, path, body, out) split into:
    pub async fn request<B, T>(&self, Method, path, Option<&B>) -> Result<T, ClientError>;
    pub async fn send<B>(&self, Method, path, Option<&B>) -> Result<Vec<u8>, ClientError>;
}
pub const NO_BODY: Option<&()> = None;

// Non-REST placeholders (TransportNotRegistered until wired)
pub fn new_soap(&str) -> Result<SoapClient, ClientError>;
pub fn new_grpc(&str) -> Result<GrpcClient, ClientError>;
pub fn new_websocket(&str) -> Result<WebSocketClient, ClientError>;

pub enum ClientError {
    Problem(FireflyError),  // decoded non-2xx upstream response
    Transport(reqwest::Error),
    InvalidUrl(String),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Exhausted(usize),
    TransportNotRegistered, // Go: ErrTransportNotRegistered
}
```

Every request automatically:

* JSON-encodes the body (when present) and sets
  `Content-Type: application/json`.
* Sets `Accept: application/json`.
* Forwards the correlation id from the kernel task-local scope
  (`firefly_kernel::with_correlation_id`) as `X-Correlation-Id`.
* Retries on network errors and 429 / 5xx status codes (exponential
  backoff: 100 ms doubling per attempt, capped at 2 s), re-sending the
  full JSON body on every attempt.
* Decodes RFC 7807 `application/problem+json` bodies into a typed
  `FireflyError` populated with `code`, `title`, `status`, `detail`,
  and `fields`.

> **Porting note (retry body):** the Go port creates the body's
> `bytes.Reader` once, outside its retry loop, so the first attempt
> exhausts it and every retried request is sent with an **empty body**
> (`ContentLength: 0`) — a bodied retry can never succeed, and no Go
> test exercises one. The Rust port deliberately diverges and re-sends
> the encoded body on every attempt, implementing the documented
> contract rather than the accidental behavior.

## Quick start

```rust,no_run
use std::time::Duration;

use firefly_client::RestBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CreateOrder {
    customer: String,
}
#[derive(Deserialize)]
struct Order {
    id: String,
    customer: String,
}

#[tokio::main]
async fn main() {
    let client = RestBuilder::new("https://api.example.com")
        .with_header("X-Tenant", "acme")
        .with_timeout(Duration::from_secs(5))
        .with_retries(3)
        .build();

    let req = CreateOrder { customer: "acme".into() };
    match client.request::<_, Order>(Method::POST, "/orders", Some(&req)).await {
        Ok(order) => println!("created {} for {}", order.id, order.customer),
        Err(err) => {
            if let Some(fe) = err.as_firefly() {
                eprintln!("upstream {}: {}", fe.status, fe.detail);
            }
        }
    }
}
```

## Composition with `firefly-resilience`

The client is deliberately small; wrap calls in resilience decorators
exactly as the Go port composes with `resilience.Chain`:

```rust,ignore
let guarded = chain(vec![as_decorator(timeout), as_decorator(circuit_breaker)]);
guarded(|| async { client.send(Method::POST, "/charge", Some(&req)).await }).await?;
```

## Testing

```bash
cargo test -p firefly-client
```

Covers the Go suite — happy-path JSON round-trip, `ProblemDetail`
decoding into a `FireflyError`, retry on 5xx (3 attempts), and the
SOAP / gRPC / WS sentinel returns — plus Rust-specific cases:
correlation-id propagation from the kernel task-local, default /
`Accept` / `Content-Type` header behavior, 429 retry, attempt
exhaustion, zero-attempt budget, network-error retry, trailing-slash
trimming, empty-body decode, and raw-byte `send`. Tests run against a
real axum server bound to a random localhost port (the `httptest`
analog).
