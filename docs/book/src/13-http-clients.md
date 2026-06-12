# HTTP Clients

`firefly-client` provides two HTTP clients that share the same automatics —
default `Accept`/`Content-Type`, correlation-id and W3C trace-context
propagation, and RFC 7807 problem decode into a typed `FireflyError`:

- the **eager `RestClient`** (built with `RestBuilder`) — an `async fn` that
  awaits a `Result`, with a built-in retry budget;
- the **reactive `WebClient`** — the Rust analog of WebFlux's `WebClient`, whose
  terminal operators hand back `Mono` / `Flux`.

The crate also ships scaffolds for SOAP, gRPC, GraphQL, and WebSocket clients.

> **Spring parity** — `RestClient` is `RestTemplate`/`RestClient`; `WebClient` is
> the WebFlux `WebClient` — fluent builder, `body_to_mono` / `body_to_flux`
> terminals, `exchange` for the raw response.

## The eager `RestClient`

Build a client with `RestBuilder`, then call `request` with a method, path, and
optional body. The retry budget, timeout, and default headers are configured on
the builder:

```rust,no_run
use std::time::Duration;
use firefly_client::RestBuilder;
use http::Method;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CreateOrder { customer: String }
#[derive(Deserialize)]
struct Order { id: String, customer: String }

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
            // Upstream RFC 7807 problems are decoded into a typed FireflyError.
            if let Some(fe) = err.as_firefly() {
                eprintln!("upstream {}: {}", fe.status, fe.detail);
            }
        }
    }
}
```

A non-2xx `application/problem+json` response is decoded into a `FireflyError`,
so an upstream failure carries the upstream's status and detail straight through
your service's own error stack.

## The reactive `WebClient`

The reactive client returns `Mono` / `Flux`, so an outbound call drops straight
into a reactive pipeline and composes end-to-end with the
[`NdJson` / `Sse` responders](./05-reactive-model.md). The fluent chain is the
WebFlux spelling:

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

    // body_to_flux — a streamed NDJSON OR SSE body, decoded lazily
    // element-by-element with backpressure.
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

The terminal operators:

| Operator                     | Returns                  | Behaviour                                   |
|------------------------------|--------------------------|---------------------------------------------|
| `body_to_mono::<T>()`        | `Mono<T>`                | the whole body decoded as one `T`           |
| `body_to_flux::<T>()`        | `Flux<T>`                | a streamed NDJSON/SSE body, element-by-element |
| `exchange()`                 | `Mono<WebClientResponse>` | the raw status + headers + body, no raise   |

### Streaming semantics

`body_to_flux` consumes the byte stream chunk-by-chunk and decodes one element
per frame, lazily and with backpressure — a slow downstream throttles the
producer, and `.take(n)` stops pulling early. The decoder is chosen from the
response `Content-Type`:

- `application/x-ndjson` (and any non-SSE type) → one JSON document per
  newline-terminated line;
- `text/event-stream` → SSE frames separated by a blank line; the `data:` lines
  are concatenated and comment / `event:` / `id:` lines are ignored.

A malformed element terminates the stream with a decode `FireflyError`
(Reactor's first-error-is-terminal semantics).

### Inspecting the raw response

`exchange()` hands back a `WebClientResponse` **without** raising on a non-2xx,
so you can inspect it and decide:

```rust,ignore
let resp = client.get().uri("/health").retrieve().exchange().block().await?.unwrap();
if resp.is_success() {
    let body: serde_json::Value = resp.body_json()?;
} else if let Some(problem) = resp.problem() {
    // a decoded RFC 7807 FireflyError, if the body was a problem document
}
```

### No baked-in retry

> **Reactor parity** — Unlike `RestBuilder::with_retries`, the `WebClient` has
> **no** retry budget. Compose retries on the returned publisher with
> `Mono::retry` / `Mono::retry_backoff`, exactly as WebFlux composes
> `retryWhen(..)` rather than baking retry into the client:
>
> ```rust,ignore
> use firefly_reactive::{Backoff, Mono};
> use std::time::Duration;
>
> let order = Mono::retry_backoff(
>     || client.get().uri("/orders/o1").retrieve().body_to_mono::<Order>(),
>     Backoff::new(3, Duration::from_millis(100)),
> );
> ```

## Composing with resilience

Both clients are deliberately small. For circuit breaking, rate limiting, or
bulkheads, wrap calls in `firefly-resilience` decorators (covered in
[Caching](./17-caching.md) and applied the same way to outbound calls):

```rust,ignore
use firefly_resilience::{CircuitBreaker, CircuitConfig};

// CircuitBreaker::execute returns the operation's value (Result<T, _>), so the
// guarded call still yields the Order. (Chain::execute is for guarded ops whose
// value you discard — it returns Result<(), _>.)
let breaker = CircuitBreaker::new(CircuitConfig::default());

let order = breaker.execute(|| async {
    client.request::<_, Order>(Method::POST, "/charge", Some(&req)).await
}).await?;
```

## Other protocols

The crate ships builders/scaffolds for the protocols a back-office platform
needs — SOAP (CXF-style envelope), gRPC, GraphQL, and WebSocket — selected by
feature so heavy dependencies stay out of services that do not use them. The
REST and GraphQL surfaces are fully wired; SOAP and the streaming protocols are
feature-gated.

Outbound calls inherit the caller's correlation id automatically, so a request
that fans out to three upstreams stitches together in your traces. The next
chapter secures the inbound side. Continue to [Security](./14-security.md).
