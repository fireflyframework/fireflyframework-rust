# Your First HTTP API

This chapter builds a real HTTP API: routes, JSON bodies, typed errors that
render as RFC 7807 problems, idempotent writes, correlation propagation, and a
reactive streaming endpoint. The HTTP layer is axum; Firefly contributes the
middleware and the problem/idempotency/correlation behaviour through
`firefly-web`, all composed for you by `Core::apply_middleware`.

## The middleware chain

`Core::apply_middleware(router)` wraps your router in the canonical outermost
chain. The default chain (matching the Go-parity core) is:

```text
incoming request
      │
      ▼
  ProblemLayer      panic → 500 application/problem+json
      │
      ▼
  CorrelationLayer  read/generate X-Correlation-Id, scope it, echo it back
      │
      ▼
  IdempotencyLayer  replay cached 2xx if an Idempotency-Key repeats
      │
      ▼
   your router
```

Optional pyfly-parity layers (CORS, security headers, CSRF, request logging,
request metrics, HTTP-exchange recording) weave in at their canonical filter
order when you set the matching `CoreConfig` knob — all OFF by default. See
[Production & Deployment](./20-production.md).

## Routes and JSON

A handler is a plain axum handler. Use axum's `Json` extractor/responder for
bodies and `Path` for path parameters:

```rust,no_run
use axum::{extract::Path, routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct CreateOrder {
    customer: String,
}

#[derive(Serialize)]
struct Order {
    id: String,
    customer: String,
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        id: "o1".into(),
        customer: body.customer,
    })
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order { id, customer: "alice".into() })
}

let router = Router::new()
    .route("/orders", post(create_order))
    .route("/orders/{id}", get(get_order));
```

## Typed errors → RFC 7807 problems

Firefly errors are `firefly_kernel::FireflyError`, and `firefly-web` renders
them as `application/problem+json`. Use `WebResult<T>` (an alias whose error
arm is a `FireflyError`) so `?` turns any error into the right problem response:

```rust,no_run
use axum::{extract::Path, Json};
use firefly_kernel::FireflyError;
use firefly_web::WebResult;
use serde::Serialize;

#[derive(Serialize)]
struct Order { id: String }

async fn get_order(Path(id): Path<String>) -> WebResult<Json<Order>> {
    if id.is_empty() {
        // 400 problem+json, type .../bad-request.
        return Err(FireflyError::bad_request("id must not be empty"));
    }
    if id == "missing" {
        // 404 problem+json.
        return Err(FireflyError::not_found("no such order"));
    }
    Ok(Json(Order { id }))
}
```

The `FireflyError` constructors map straight to HTTP status:

| Constructor                              | Status | Use                          |
|------------------------------------------|--------|------------------------------|
| `FireflyError::bad_request(detail)`      | 400    | malformed input              |
| `FireflyError::unauthorized(detail)`     | 401    | missing/invalid credentials  |
| `FireflyError::forbidden(detail)`        | 403    | authenticated but not allowed |
| `FireflyError::not_found(detail)`        | 404    | absent resource              |
| `FireflyError::conflict(detail)`         | 409    | state conflict               |
| `FireflyError::validation(detail)`       | 422    | semantic validation failure  |
| `FireflyError::business_rule(rule, detail)` | 422 | domain rule violated         |
| `FireflyError::internal(detail)`         | 500    | server fault                 |

A rendered problem looks like:

```json
{
  "type": "https://fireflyframework.org/problems/not-found",
  "title": "Not Found",
  "status": 404,
  "detail": "no such order"
}
```

## Correlation IDs

Every response carries an `X-Correlation-Id`. An incoming one is honoured;
otherwise one is generated. The id is scoped through
`firefly_kernel::with_correlation_id` for the whole request, so every log line,
every emitted event, and every outbound client call inherits it automatically.
Read it in a handler:

```rust,ignore
use axum::Extension;
use firefly_web::CorrelationId;

async fn handler(Extension(cid): Extension<CorrelationId>) -> String {
    format!("correlation: {}", cid.0)
}
```

The extended `CorrelationLayer` also mints/echoes `X-Request-Id`, propagates
`X-Tenant-Id` and `X-Transaction-Id` into the kernel task-locals, and echoes
W3C `traceparent` / `tracestate`.

## Idempotent writes

Any `POST`/`PUT`/`PATCH` carrying an `Idempotency-Key` header is recorded. A
repeat of the same key replays the stored response with `Idempotent-Replay:
true`; reusing the key with a *different* body is a `409`. This is on by default
through the middleware chain — you write the handler once and get
exactly-once-from-the-client-perspective semantics for free.

```bash
# First call: executes and stores.
curl -X POST localhost:8080/orders \
  -H 'Idempotency-Key: abc-123' \
  -H 'Content-Type: application/json' \
  -d '{"customer":"alice"}'

# Same key, same body: replays the stored response.
curl -i -X POST localhost:8080/orders \
  -H 'Idempotency-Key: abc-123' \
  -H 'Content-Type: application/json' \
  -d '{"customer":"alice"}'
# Idempotent-Replay: true
```

The default store is in-process (`MemoryIdempotencyStore`); for a multi-replica
deployment, implement the `IdempotencyStore` trait over Redis or Postgres and
pass it via `IdempotencyConfig`.

## A reactive endpoint

Mount a streaming endpoint with the reactive responders from
[The Reactive Model](./05-reactive-model.md). The `Flux` is bridged straight
into the response body with backpressure:

```rust,no_run
use axum::{routing::get, response::IntoResponse, Router};
use firefly_reactive::{Flux, Mono};
use firefly_web::{MonoJson, NdJson};
use serde::Serialize;

#[derive(Serialize, Clone)]
struct Order { id: String }

async fn one_order() -> impl IntoResponse {
    MonoJson(Mono::just(Order { id: "o1".into() }))
}

async fn stream_orders() -> impl IntoResponse {
    // Emits one application/x-ndjson line per order, flushed as produced.
    NdJson(Flux::just(vec![
        Order { id: "o1".into() },
        Order { id: "o2".into() },
    ]))
}

let router = Router::new()
    .route("/orders/one", get(one_order))
    .route("/orders/stream", get(stream_orders));
```

## Putting it together

A complete service mounts the routes through `apply_middleware`, serves the
public API and actuator on separate ports, and runs under the lifecycle
application for graceful shutdown:

```rust,no_run
use axum::{routing::{get, post}, Json, Router};
use firefly_starter_core::{Core, CoreConfig};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct CreateOrder { customer: String }
#[derive(Serialize)]
struct Order { id: String, customer: String }

async fn create_order(Json(b): Json<CreateOrder>) -> Json<Order> {
    Json(Order { id: "o1".into(), customer: b.customer })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::new(CoreConfig { app_name: "orders".into(), ..Default::default() });
    core.init_logging()?;

    let api = core.apply_middleware(
        Router::new().route("/orders", post(create_order)),
    );
    let admin = core.actuator_router(Vec::new());

    let app = core
        .new_application()
        .on_server("api", move |sd| async move {
            let l = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
            axum::serve(l, api).with_graceful_shutdown(sd.wait()).await?;
            Ok(())
        })
        .on_server("admin", move |sd| async move {
            let l = tokio::net::TcpListener::bind("0.0.0.0:8081").await?;
            axum::serve(l, admin).with_graceful_shutdown(sd.wait()).await?;
            Ok(())
        });
    app.run().await?;
    Ok(())
}
```

Next, give your service a database with reactive repositories in
[Persistence & Reactive Repositories](./07-persistence.md).
