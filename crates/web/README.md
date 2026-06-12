# `firefly-web`

> **Tier:** Foundational · **Status:** Full · **Go module:** `web` · **Java original:** `firefly-web` + `firefly-spring-utils`

## Overview

`firefly-web` is the framework's **HTTP-layer middleware tier** — it
converts errors into RFC 7807 `application/problem+json` responses,
propagates correlation IDs, replays idempotent requests, and scrubs PII
out of log lines. Composed at the outermost edge of every Firefly
service via `firefly-starter-core`.

Every middleware is a [`tower::Layer`], so it composes with axum and any
tower-compatible router — the Rust analog of the Go module's
`func(http.Handler) http.Handler` middlewares.

## Why a separate crate?

Spring's `@ControllerAdvice` and ASP.NET's exception handlers cover
problem-detail rendering only. The framework needs four orthogonal
middlewares to behave identically across runtimes, with the same header
names, response shapes, and conflict semantics — `firefly-web` provides
them as one composable bundle, wire-compatible with the Java, .NET, Go,
and Python ports.

## Mental model

```
incoming request
      │
      ▼
┌─────────────────────────────────────────┐
│ ProblemLayer      (panic → 500 RFC7807) │
└─────────────────────────────────────────┘
      │
      ▼
┌─────────────────────────────────────────┐
│ CorrelationLayer  (X-Correlation-Id)    │
└─────────────────────────────────────────┘
      │
      ▼
┌─────────────────────────────────────────┐
│ IdempotencyLayer  (replay if Key)       │
└─────────────────────────────────────────┘
      │
      ▼
   your router
```

This is the chain composed by `firefly-starter-core`.

## Public surface

### Problem-detail rendering

| Symbol                          | Behaviour                                                                  |
|---------------------------------|----------------------------------------------------------------------------|
| `problem_response(&ProblemDetail)` | Builds the `application/problem+json` response with the problem's status |
| `error_response(&dyn Error)`    | Converts via `firefly_kernel::as_problem` then renders                     |
| `ProblemLayer`                  | Catches panics, renders 500 with `firefly_kernel::TYPE_INTERNAL`           |
| `WebError` / `WebResult<T>`     | Handler error type: `FireflyError` → `?` → RFC 7807 response (the Go `ErrorHandler` adapter) |

### Correlation

| Symbol                | Behaviour                                                                                       |
|-----------------------|-------------------------------------------------------------------------------------------------|
| `CorrelationLayer`    | Reads / generates `X-Correlation-Id`, scopes it via `firefly_kernel::with_correlation_id`, stores `CorrelationId` in request extensions, echoes back — including on the panic→500 path recovered by `ProblemLayer`, as in Go |
| `CorrelationId(String)` | Extractable in handlers with `axum::Extension<CorrelationId>`                                 |

### Idempotency

| Symbol                                   | Behaviour                                                              |
|------------------------------------------|-------------------------------------------------------------------------|
| `IdempotencyConfig { store, ttl, methods }` | Tunes the middleware                                                 |
| `IdempotencyConfig::default()`           | 24 h TTL, memory store, POST/PUT/PATCH                                  |
| `IdempotencyLayer`                       | Replays cached 2xx responses (`Idempotent-Replay: true`); returns 409 on key reuse with a different body; first-pass responses stream through unbuffered while being captured (Go `captureWriter` parity) |
| `MemoryIdempotencyStore`                 | Default in-process store                                                |
| `IdempotencyStore` trait                 | Plug your own (Redis / Postgres / etc.)                                 |
| `IdempotencyRecord`                      | Stored response; JSON shape matches the Go port for cross-runtime stores |

### PII masking

| Symbol                  | Behaviour                                                                |
|-------------------------|---------------------------------------------------------------------------|
| `mask_pii(&str) -> String` | Redacts emails, IBANs, cards, E.164 phones as `[REDACTED:<kind>]`; matches Go RE2's ASCII `\b`/`\d` semantics, so numbers adjacent to non-ASCII text are still masked |
| `mask_map(&Map) -> Map` | Recursive redaction over a JSON object; sensitive keys (`password`, `token`, `secret`, `authorization`, `cookie`, `api_key`, `apikey`, `private_key`) replaced wholesale |

## pyfly parity

Beyond the Go-parity chain above, the crate ships the full pyfly
`web` + `server` middleware surface. Every layer follows the same
hand-rolled `tower::Layer` style (the workspace deliberately avoids
`tower-http`) and keeps wire formats byte-identical to the Java, .NET,
Go, and Python ports.

### CORS — `cors.rs`

| Symbol | Behaviour |
|--------|-----------|
| `CorsConfig { allowed_origins, allowed_methods, allowed_headers, allow_credentials, exposed_headers, max_age }` | Config struct (serde kebab-case); `default()` permits `*` origin/header with `GET`, `max_age` 600 |
| `CorsConfig::permit_defaults()` | Spring's permit set: `GET`/`HEAD`/`POST` (`PERMIT_DEFAULT_METHODS`) |
| `CorsLayer` | Short-circuits preflight `OPTIONS` (400 on disallowed origin/method, echoes requested headers for `*`), decorates simple responses with `Access-Control-Allow-*`, reflects origin under credentials |

### Security headers — `headers.rs`

| Symbol | Behaviour |
|--------|-----------|
| `SecurityHeadersConfig` | Same 7 fields/defaults as pyfly: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, HSTS, `X-XSS-Protection: 0`, `Referrer-Policy`, optional CSP + Permissions-Policy |
| `SecurityHeadersLayer` | Pre-encodes the static header pairs once and appends them to every response |

### CSRF — `csrf.rs`

| Symbol | Behaviour |
|--------|-----------|
| `CsrfLayer` | Double-submit cookie: `XSRF-TOKEN` cookie vs `X-XSRF-TOKEN` header, safe-method pass-through with cookie refresh, **Bearer bypass**, timing-safe SHA-256 digest compare (no `subtle` dep), 403 `problem+json` on mismatch |
| `generate_csrf_token()` / `validate_csrf_token(cookie, header)` | Token mint + constant-time validation helpers |

### Request access log — `request_log.rs`

| Symbol | Behaviour |
|--------|-----------|
| `RequestLogLayer` | One `tracing` event per request (`http_request` INFO with `method`/`path`/`status_code`/`duration_ms`/`transaction_id`/`correlation_id`; `http_request_failed` ERROR on panic, re-raised so `ProblemLayer` still renders the recovered 500) on target `REQUEST_LOG_TARGET` |

### HTTP server metrics — `metrics.rs`

| Symbol | Behaviour |
|--------|-----------|
| `MetricsLayer` | Records `method` / templated `uri` (axum `MatchedPath`, not raw path) / `status` / `Outcome` / `exception` + duration per request, plus the two-window rolling `_max` (`HTTP_SERVER_REQUESTS_MAX_METRIC`) |
| `RequestObserver` trait | Local sink (no `firefly-actuator` dep — starter-core bridges to the `MetricRegistry` later); `RequestMetric` carries the Micrometer-parity tags |

### Extended correlation — `correlation.rs`

`CorrelationLayer` keeps `X-Correlation-Id` behaviour identical and
additionally mints/echoes `X-Request-Id`, propagates `X-Tenant-Id` and
`X-Transaction-Id` into the kernel task-locals, and echoes
`traceparent`/`tracestate`. The full snapshot is stored as a
`CorrelationContext` request extension.

### Content negotiation — `content_negotiation.rs`

| Symbol | Behaviour |
|--------|-----------|
| `MessageConverterRegistry`, `parse_accept` | `Accept` q-value parsing + converter selection |
| `JsonMessageConverter`, `XmlMessageConverter` (quick-xml) | Read/write JSON and XML; `value_to_xml`/`xml_to_value` for dict↔XML |
| `Negotiate<T>` | Responder that serialises `T` to the negotiated media type |

### Server bootstrap — `server.rs`

| Symbol | Behaviour |
|--------|-----------|
| `ServerProperties { host, port, graceful_timeout, keep_alive_timeout, backlog, max_concurrent_connections, tls }` | serde-bound under `server.*` |
| `TlsConfig { cert_file, key_file }` | TLS termination (axum-server `tls-rustls`) |
| `ServerInfo { name, version, host, port, http_protocol, tls }` | Runtime snapshot for `/actuator/info` |
| `Server::bind(props)` / `serve(router, props, shutdown)` | Builds the listener (socket2 backlog/`SO_REUSEADDR`, `ConcurrencyLimitLayer`), serves plain-HTTP or TLS, honours the lifecycle drain — drops straight into `Application::on_server` |

## Reactive (WebFlux/Reactor) surface — `reactive.rs`

An **additive** reactive HTTP surface built on the [`firefly-reactive`]
crate (`Mono<T>` / `Flux<T>`). It is the Rust analog of returning
`Mono<T>` / `Flux<T>` from a Spring WebFlux `@RestController` — and it
reuses this crate's RFC 7807 problem renderer plus
[`firefly-sse`]'s wire format, so every reactive response is
byte-compatible with the rest of the framework. Nothing here changes an
existing signature or wire format; it sits alongside.

| Spring WebFlux | firefly-web |
|----------------|-------------|
| `Mono<T>` handler return | `MonoJson(Mono<T>)` |
| `Mono<T>` empty → `404` | `Ok(None)` → `application/problem+json` 404 |
| `Mono<T>` error → problem | `Err(FireflyError)` → that error's RFC 7807 response |
| `Flux<T>` + `APPLICATION_NDJSON_VALUE` | `NdJson(Flux<T>)` |
| `Flux<ServerSentEvent<T>>` | `Sse(Flux<T>)` / `SseEvents(Flux<Event>)` |

| Symbol | Behaviour |
|--------|-----------|
| `MonoJson(Mono<T>)` | Resolves the `Mono`: `Ok(Some)` → `200` `application/json`; `Ok(None)` → `404` `application/problem+json`; `Err(FireflyError)` → that error's problem response |
| `NdJson(Flux<T>)` | Streams `application/x-ndjson` — one compact JSON doc + `'\n'` per element, flushed incrementally with real backpressure (the `Flux`'s `Stream` is bridged straight into an axum streaming `Body`; the whole stream is **never** buffered). An `Err` item mid-stream terminates the body cleanly |
| `Sse(Flux<T>)` | Streams `text/event-stream` — each element serialized to JSON as a bare `data: <json>\n\n` frame via `firefly_sse::Event::to_wire` (byte-identical to the `firefly-sse` writer). Same backpressure + clean error-mid-stream truncation |
| `SseEvents(Flux<Event>)` | Streams `text/event-stream` over pre-built `firefly_sse::Event` values (use when you need `id` / `event` / `retry` fields) |
| `NDJSON_CONTENT_TYPE` / `SSE_CONTENT_TYPE` | The `application/x-ndjson` and `text/event-stream` media types |

`MonoJson` resolves the (async) `Mono` from the synchronous
`IntoResponse::into_response` so the HTTP status faithfully reflects the
terminal signal: on the framework's default multi-thread runtime it uses
`tokio::task::block_in_place` (no other task is starved); off a runtime
(or on a current-thread runtime) it falls back to a transient runtime.
For an explicitly streamed body, prefer `NdJson` / `Sse`.

```rust
use axum::{response::IntoResponse, routing::get, Router};
use firefly_reactive::{Flux, Mono};
use firefly_web::{MonoJson, NdJson, Sse};

async fn one_order() -> impl IntoResponse {
    // Ok(Some) → 200 JSON, Ok(None) → 404 problem, Err → that problem.
    MonoJson(Mono::just(serde_json::json!({ "id": "o1" })))
}

async fn stream_orders() -> impl IntoResponse {
    // application/x-ndjson, one line per element, backpressured.
    NdJson(Flux::just(vec![1, 2, 3]))
}

async fn live_orders() -> impl IntoResponse {
    // text/event-stream, one `data:` frame per element.
    Sse(Flux::just(vec![1, 2, 3]))
}

let app: Router = Router::new()
    .route("/orders/o1", get(one_order))
    .route("/orders", get(stream_orders))
    .route("/orders/live", get(live_orders));
# let _ = app;
```

[`firefly-reactive`]: ../reactive/README.md
[`firefly-sse`]: ../sse/README.md

## Quick start

```rust
use axum::{routing::post, Router};
use firefly_kernel::FireflyError;
use firefly_web::{CorrelationLayer, IdempotencyLayer, ProblemLayer, WebResult};
use tower::ServiceBuilder;

async fn create_order() -> WebResult<&'static str> {
    // … your domain logic; `?` on any FireflyResult renders RFC 7807 …
    Err(FireflyError::bad_request("customer is required").into())
}

#[tokio::main]
async fn main() {
    let app = Router::new().route("/orders", post(create_order)).layer(
        // ServiceBuilder applies top-down: ProblemLayer is outermost.
        ServiceBuilder::new()
            .layer(ProblemLayer::new())
            .layer(CorrelationLayer::new())
            .layer(IdempotencyLayer::default()),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

## Testing

```bash
cargo test -p firefly-web
```

Suite covers panic→500, the typed `WebResult` handler, correlation id
generation + echo-back + extension extraction (including the echo on
the panic→500 path), idempotency replay (replay header, body, captured
headers), streaming passthrough of first-pass keyed responses, conflict
on key reuse, store TTL expiry, Go-compatible record JSON, and PII
redaction across emails / IBANs / cards / phones — with Go-parity ASCII
`\b`/`\d` semantics next to non-ASCII text — plus map-key
sensitive-name scrubbing. The reactive surface adds `MonoJson`
(200 / 404 / problem), exact NDJSON multi-line body bytes,
error-mid-stream truncation, and SSE frame bytes — all via
`tower::ServiceExt::oneshot`.
