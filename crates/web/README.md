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
| `CorrelationLayer`    | Reads / generates `X-Correlation-Id`, scopes it via `firefly_kernel::with_correlation_id`, stores `CorrelationId` in request extensions, echoes back |
| `CorrelationId(String)` | Extractable in handlers with `axum::Extension<CorrelationId>`                                 |

### Idempotency

| Symbol                                   | Behaviour                                                              |
|------------------------------------------|-------------------------------------------------------------------------|
| `IdempotencyConfig { store, ttl, methods }` | Tunes the middleware                                                 |
| `IdempotencyConfig::default()`           | 24 h TTL, memory store, POST/PUT/PATCH                                  |
| `IdempotencyLayer`                       | Replays cached 2xx responses (`Idempotent-Replay: true`); returns 409 on key reuse with a different body |
| `MemoryIdempotencyStore`                 | Default in-process store                                                |
| `IdempotencyStore` trait                 | Plug your own (Redis / Postgres / etc.)                                 |
| `IdempotencyRecord`                      | Stored response; JSON shape matches the Go port for cross-runtime stores |

### PII masking

| Symbol                  | Behaviour                                                                |
|-------------------------|---------------------------------------------------------------------------|
| `mask_pii(&str) -> String` | Redacts emails, IBANs, cards, E.164 phones as `[REDACTED:<kind>]`      |
| `mask_map(&Map) -> Map` | Recursive redaction over a JSON object; sensitive keys (`password`, `token`, `secret`, `authorization`, `cookie`, `api_key`, `apikey`, `private_key`) replaced wholesale |

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
generation + echo-back + extension extraction, idempotency replay
(replay header, body, captured headers), conflict on key reuse, store
TTL expiry, Go-compatible record JSON, and PII redaction across emails /
IBANs / cards / phones plus map-key sensitive-name scrubbing.
