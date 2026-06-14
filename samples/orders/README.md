# Orders sample

The reference Firefly Framework Rust service. Demonstrates:

- The five-module layout
  (`interfaces`, `models`, `core`, `web`, `sdk`)
- `Core::new(...)` one-call composition (`firefly-starter-core`)
- CQRS dispatch with validation + query caching (`firefly-cqrs`)
- Idempotency replay on `POST /api/v1/orders` (`firefly-web`)
- RFC 7807 `application/problem+json` error rendering
- Correlation-id propagation
- Startup banner

| Rust module             | Contents                              |
|-------------------------|---------------------------------------|
| `interfaces`            | Wire shapes + CQRS messages           |
| `models`                | `Order` entity + `Repository` port    |
| `core`                  | CQRS handler registration             |
| `web` + `src/main.rs`   | Router composition + HTTP entry point |
| `sdk`                   | Typed client over `/api/v1/orders`    |

## Run

```bash
cargo run -p firefly-sample-orders
```

The public API binds `127.0.0.1:8080`; the actuator admin surface
(`/actuator/health`, `/actuator/info`, `/actuator/metrics`,
`/actuator/env`, `/actuator/tasks`, `/actuator/version`) binds
`127.0.0.1:8081` so management endpoints never leak onto the public
network. Override either address with the `ORDERS_ADDR` /
`ORDERS_ADMIN_ADDR` environment variables. The process shuts down
gracefully on SIGINT/SIGTERM.

## Place an order

```bash
curl -X POST http://localhost:8080/api/v1/orders \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: order-1' \
  -d '{"customer":"alice","sku":"SKU-1","quantity":2,"total":19.99}'
```

Repeat the same request — the server returns `Idempotent-Replay: true`
with the originally captured response.

## Read the order back

```bash
curl http://localhost:8080/api/v1/orders/<id>
```

The first hit goes to the handler; subsequent reads within 30 s are
served from the CQRS query cache
(`GetOrderQuery::cache_ttl()`).

## Errors

Every failure renders as RFC 7807 `application/problem+json`:

- malformed JSON → `400` (`invalid json: …`)
- a request failing domain validation → `422`
  (`customer is required`, …)
- an unknown order id → `404` (`order <id> not found`)
- an `Idempotency-Key` reused with a different payload → `409`
  idempotency-conflict

## SDK

```rust,no_run
use firefly_sample_orders::interfaces::PlaceOrderRequest;
use firefly_sample_orders::sdk::Client;

# async fn demo() -> Result<(), firefly_client::ClientError> {
let client = Client::new("http://localhost:8080");
let placed = client
    .place(&PlaceOrderRequest {
        customer: "alice".into(),
        sku: "SKU-1".into(),
        quantity: 2,
        total: 19.99,
    })
    .await?;
let fetched = client.get(&placed.id).await?;
assert_eq!(fetched, placed);
# Ok(())
# }
```

## Test

```bash
cargo test -p firefly-sample-orders
```

Boots the full stack via `build_router()` and
asserts on the wire shape — in-process through
`tower::ServiceExt::oneshot`, no sockets needed. A counting repository
proves the idempotency replay never re-runs the handler and that the
second GET is served from the query cache; the SDK tests drive the same
router over an ephemeral 127.0.0.1 port through `firefly-client`.
