# `firefly-client`

> **Tier:** Adapter · **Status:** REST + GraphQL + SOAP + reactive `WebClient` Full; WebSocket / gRPC feature-gated · **Go module:** `client` · **Java original:** `firefly-service-client`

## Overview

`firefly-client` is the framework's **outbound HTTP client builder** — a
fluent [`RestBuilder`] that composes timeouts, retries, default headers,
and correlation-id propagation into a [`RestClient`] whose
`request` / `send` methods are the Rust spelling of the Go port's single
`Do(ctx, method, path, body, out)`. Non-2xx responses are decoded into
the kernel's `FireflyError` (RFC 7807-aware), so every consumer of every
external service sees the same error shape.

Beyond REST it ships the four thin protocol clients from pyfly's
`client` package — see the [pyfly parity](#pyfly-parity-protocol-clients)
section. The legacy `new_soap` / `new_grpc` / `new_websocket` free
functions remain for backward compatibility and still return
`ClientError::TransportNotRegistered`; the typed builders are the
supported entry points.

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

// Legacy Go-parity sentinels (always TransportNotRegistered)
pub fn new_soap(&str) -> Result<SoapPlaceholder, ClientError>;
pub fn new_grpc(&str) -> Result<GrpcPlaceholder, ClientError>;
pub fn new_websocket(&str) -> Result<WebSocketPlaceholder, ClientError>;

pub enum ClientError {
    Problem(FireflyError),  // decoded non-2xx upstream response
    Transport(reqwest::Error),
    InvalidUrl(String),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    Exhausted(usize),
    GraphQl(Vec<serde_json::Value>), // non-empty GraphQL `errors` array
    TransportNotRegistered, // Go: ErrTransportNotRegistered
}
```

Every request automatically:

* JSON-encodes the body (when present) and sets
  `Content-Type: application/json`.
* Sets `Accept: application/json`.
* Forwards the correlation id from the kernel task-local scope
  (`firefly_kernel::with_correlation_id`) as `X-Correlation-Id`, plus the
  W3C `traceparent` / `tracestate` from the observability task-local
  scope when present (pyfly's httpx adapter behaviour), keeping the
  distributed trace unbroken across hops.
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

## pyfly parity: protocol clients

The crate ports the four thin protocol clients from pyfly's `client`
package. GraphQL and SOAP need no extra dependencies and are always
available; WebSocket and gRPC are feature-gated to keep the core build
light.

| pyfly | Rust | Feature | Entry point |
| --- | --- | --- | --- |
| `GraphQLClient` | `GraphQlClient` / `GraphQlBuilder` | — | `execute<V, T>(query, variables, operation_name)` |
| `SoapClient` | `SoapClient` / `SoapBuilder` | — | `call(body_xml) -> String` |
| `WebSocketClient` | `WsClient` / `WsBuilder` | `websocket` | `connect() -> WsStream`, `stream(send) -> impl Stream` |
| `GrpcClientBuilder` | `GrpcBuilder` | `grpc` (`grpc-tls` for TLS) | `connect() -> tonic Channel`, `connect_lazy()`, `endpoint()` |

```rust,ignore
// GraphQL — POSTs {query, variables?, operationName?}; raises
// ClientError::GraphQl on a non-empty `errors` array, decodes `data`.
use firefly_client::{no_variables, GraphQlBuilder};
let gql = GraphQlBuilder::new("https://api.example.com/graphql")
    .with_header("Authorization", "Bearer t")
    .build();
let data: MyData = gql.execute("{ user { id } }", no_variables(), None).await?;

// SOAP 1.1 — wraps body in an envelope, POSTs text/xml + SOAPAction.
use firefly_client::SoapBuilder;
let soap = SoapBuilder::new("https://soap.example.com/svc")
    .with_action("GetThing")
    .build();
let xml = soap.call("<GetThing><id>42</id></GetThing>").await?;

// WebSocket (feature = "websocket") — connect / stream over tokio-tungstenite.
use firefly_client::WsBuilder;
use tokio_tungstenite::tungstenite::Message;
let ws = WsBuilder::new("wss://example.com/ws").build();
let mut conn = ws.connect().await?;          // raw WsStream (Sink + Stream)
let mut msgs = ws.stream(vec![Message::text("hi")]).await?; // inbound Stream

// gRPC (feature = "grpc") — build a tonic Channel for a generated stub.
use firefly_client::GrpcBuilder;
let channel = GrpcBuilder::new("http://127.0.0.1:50051").connect().await?;
// let stub = my_proto::GreeterClient::new(channel);
```

Deliberate adaptations from pyfly:

* `GraphQlClient::execute` is generic over both variables (`V: Serialize`)
  and the response (`T: DeserializeOwned`); pyfly returns a loose `dict`.
  GraphQL errors surface as the typed `ClientError::GraphQl(Vec<Value>)`
  (the structured array is preserved, not stringified). A non-2xx HTTP
  status decodes into `ClientError::Problem` like every other Firefly
  client, where pyfly raises `httpx.HTTPStatusError`.
* `SoapClient::call` returns the raw response XML; the envelope template
  is byte-for-byte identical to pyfly's. `wrap_envelope` is exported for
  callers that want to inspect the exact wire payload.
* `WsClient` returns the raw `tokio-tungstenite` `WebSocketStream` (which
  drives Ping/Pong transparently); `with_ping_interval` is recorded for
  API symmetry with pyfly's `ping_interval`.
* `GrpcBuilder` is channel-only, like pyfly — it never depends on a
  generated stub. `secured(true)` requires the `grpc-tls` feature;
  without it a secured target returns `GrpcError::TlsUnsupported` rather
  than silently downgrading.

## Reactive

Alongside the eager `RestClient`, the crate ships a **reactive** HTTP
client — the Rust analog of Spring WebFlux's `WebClient`, built on
[`firefly-reactive`](../reactive)'s `Mono<T>` / `Flux<T>`. It is strictly
**additive**: the eager `RestClient` surface, its wire format, and all of
its tests are untouched. Where `RestClient` returns bare futures,
`WebClient`'s terminal operators hand back `Mono` / `Flux`, so an
outbound call drops straight into a reactive pipeline (and composes with
`firefly-web`'s `Flux`→NDJSON/SSE responders end-to-end).

```rust,ignore
pub fn new_web_client(base_url: impl AsRef<str>) -> WebClientBuilder; // Spring: WebClient.builder().baseUrl(..)

pub struct WebClientBuilder { /* … */ }
impl WebClientBuilder {
    pub fn new(base_url: impl AsRef<str>) -> Self;
    pub fn with_header(self, key, value) -> Self;   // Spring: defaultHeader
    pub fn with_timeout(self, Duration) -> Self;
    pub fn with_http_client(self, reqwest::Client) -> Self;
    pub fn build(self) -> WebClient;
}

pub struct WebClient { /* … */ }
impl WebClient {
    pub fn method(&self, Method) -> RequestSpec;  // Spring: webClient.method(..)
    pub fn get/post/put/delete/patch(&self) -> RequestSpec;
}

pub struct RequestSpec { /* … */ }
impl RequestSpec {
    pub fn uri(self, impl AsRef<str>) -> Self;     // absolute or base-relative
    pub fn header(self, key, value) -> Self;
    pub fn query(self, key, value) -> Self;        // repeatable
    pub fn body<B: Serialize>(self, &B) -> Self;   // Spring: bodyValue
    pub fn retrieve(self) -> ResponseSpec;
}

pub struct ResponseSpec { /* … */ }
impl ResponseSpec {
    pub fn body_to_mono<T: DeserializeOwned>(self) -> Mono<T>; // whole body -> one T
    pub fn body_to_flux<T: DeserializeOwned>(self) -> Flux<T>; // streamed NDJSON/SSE -> 0..N T
    pub fn exchange(self) -> Mono<WebClientResponse>;          // raw status + headers + body
}

pub struct WebClientResponse { /* … */ }   // Spring: ClientResponse
impl WebClientResponse {
    pub fn status(&self) -> u16;
    pub fn is_success(&self) -> bool;
    pub fn headers(&self) -> &HeaderMap;
    pub fn body(&self) -> &Bytes;
    pub fn body_json<T: DeserializeOwned>(&self) -> Result<T, ClientError>;
    pub fn problem(&self) -> Option<FireflyError>; // RFC 7807 decode of a non-2xx
}
```

The fluent chain is the WebFlux spelling:

```rust,no_run
use firefly_client::WebClientBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CreateOrder { customer: String }
#[derive(Deserialize)]
struct Order { id: String }
#[derive(Deserialize)]
struct Tick { seq: u64 }

#[tokio::main]
async fn main() {
    let client = WebClientBuilder::new("https://api.example.com")
        .with_header("X-Tenant", "acme")
        .build();

    // body_to_mono — a single value -> Mono<Order>.
    let _order = client
        .method(Method::POST)
        .uri("/orders")
        .body(&CreateOrder { customer: "acme".into() })
        .retrieve()
        .body_to_mono::<Order>();

    // body_to_flux — a streamed application/x-ndjson OR text/event-stream
    // body, decoded lazily element-by-element with backpressure.
    let _ticks = client
        .get()
        .uri("/ticks")
        .header("Accept", "application/x-ndjson")
        .retrieve()
        .body_to_flux::<Tick>();

    // exchange — raw status + headers without raising on a non-2xx.
    let _resp = client.get().uri("/health").retrieve().exchange();
}
```

### Streaming semantics (`body_to_flux`)

`body_to_flux` consumes the `reqwest` byte stream chunk-by-chunk and
decodes one element per frame, **lazily and with backpressure** — a slow
downstream throttles the producer, and `.take(n)` stops pulling early. The
decoder is chosen from the response `Content-Type`:

* `application/x-ndjson` (and any non-SSE type) → one JSON document per
  newline-terminated line; blank lines are skipped.
* `text/event-stream` → SSE frames separated by a blank line; the `data:`
  lines of each event block are concatenated; comment (`:`) / `event:` /
  `id:` lines and keep-alive blocks are ignored.

A malformed element terminates the stream with a decode `FireflyError`
(Reactor's first-error-is-terminal semantics).

### Same automatics, same problem decode

Every `WebClient` request reuses the eager client's logic, so the two
surfaces never drift: `Accept: application/json` by default,
`Content-Type: application/json` on a bodied request, correlation-id +
W3C `traceparent` / `tracestate` propagation, and RFC 7807
`application/problem+json` decode into a typed `FireflyError`. On the
reactive surface that `FireflyError` is simply the publisher's terminal
`Err` signal — a non-2xx response short-circuits a `Mono` / `Flux`, while
`exchange()` hands the raw response back **without** raising so the caller
can inspect `status()` / `problem()` and decide.

> **No baked-in retry:** unlike `RestBuilder::with_retries`, `WebClient`
> has no retry budget — retries on a reactive pipeline are composed with
> `Mono::retry` / `Mono::retry_backoff` on the returned publisher, exactly
> as WebFlux composes `retryWhen(..)` rather than baking it into the
> client.

## Composition with `firefly-resilience`

The client is deliberately small; wrap calls in resilience decorators
exactly as the Go port composes with `resilience.Chain`:

```rust,ignore
let guarded = chain(vec![as_decorator(timeout), as_decorator(circuit_breaker)]);
guarded(|| async { client.send(Method::POST, "/charge", Some(&req)).await }).await?;
```

## Testing

```bash
cargo test -p firefly-client                          # REST + GraphQL + SOAP
cargo test -p firefly-client --features websocket,grpc # all protocols
```

Covers the Go suite — happy-path JSON round-trip, `ProblemDetail`
decoding into a `FireflyError`, retry on 5xx (3 attempts), and the
legacy SOAP / gRPC / WS sentinel returns — plus Rust-specific cases:
correlation-id propagation from the kernel task-local, default /
`Accept` / `Content-Type` header behavior, 429 retry, attempt
exhaustion, zero-attempt budget, network-error retry, trailing-slash
trimming, empty-body decode, and raw-byte `send`.

The pyfly protocol clients are tested 1:1 against pyfly's cases:
GraphQL and SOAP against in-process axum mocks (envelope wrapping,
`SOAPAction`, omitting `None` fields, the `errors`-array path);
WebSocket against an in-process axum ws **echo** route (connect +
send/recv and the `stream(send)` helper); and gRPC builder-only
(target validation, option chaining, lazy channel construction — no
server). All tests run against a real axum server bound to a random
localhost port (the `httptest` analog) and stay well under the 200 ms
budget.

The reactive `WebClient` is tested (`tests/webclient_test.rs`) against
in-process axum servers emitting NDJSON and SSE: `body_to_flux` streams
the elements (and composes `filter`/`map`/`take` lazily — `take(3)` over a
slow six-element producer never waits for the whole stream); `body_to_mono`
GET/POST round-trips (including a `204` empty-body decode and query
params); RFC 7807 problem decode on both `Mono` and `Flux`; `exchange`
exposing status + headers without raising on a non-2xx; correlation-id
propagation from the kernel task-local; and absolute-URI / invalid-URL /
transport-failure error paths. Every case stays under the 200 ms budget.
