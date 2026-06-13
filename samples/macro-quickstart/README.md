# macro-quickstart

A small but **complete** Firefly Framework service that proves the declarative
developer experience of `firefly-macros` reached over the one-dependency
[`firefly`](../../crates/firefly) facade. It re-implements the essence of the
[`orders`](../orders) reference sample with far less code: no hand-rolled bus
registration, router building, or scheduler wiring — every framework
integration is a *declaration sitting next to the code it describes*.

## One dependency, one prelude

```toml
[dependencies]
firefly = { workspace = true }   # the whole framework + every macro
axum   = { workspace = true }    # you author axum handlers
serde  = { workspace = true }    # your messages are Serialize
tokio  = { workspace = true }    # #[tokio::main]
```

```rust
use firefly::prelude::*;
```

That glob brings the whole framework into scope — `Bus`, `Container`,
`Scheduler`, `Core`, `WebResult`, `FireflyError`, **and** every
`#[derive(...)]` / `#[...]` macro. Macro-generated code resolves its runtime
types through the facade's hidden `__rt` contract, so the service never lists a
single underlying `firefly-*` crate.

## What it showcases

| Declaration | Macro | Generated wiring |
|-------------|-------|------------------|
| `PlaceOrder` | `#[derive(Command)]` | `impl Message` + `#[firefly(validate)]` field checks |
| `GetOrder` | `#[derive(Query)]` | `impl Message` with `#[firefly(cache_ttl = "30s")]` |
| `place_order` | `#[command_handler]` | `register_place_order(bus)` |
| `get_order` | `#[query_handler]` | `register_get_order(bus)` |
| `impl OrderApi` | `#[rest_controller(path = "/api/v1/orders")]` + `#[get]`/`#[post]` | `OrderApi::routes(state) -> axum::Router` |
| `OrderStore` | `#[derive(Component)]` | `OrderStore::firefly_register(container)` |
| `sweep_stale_orders` | `#[scheduled(fixed_rate = "60s")]` | `schedule_sweep_stale_orders(scheduler)` |

Everything lives in [`src/lib.rs`](src/lib.rs); [`src/main.rs`](src/main.rs)
boots it through the starter `Core`.

## The before/after: builder wiring vs macro declarations

The reference [`orders`](../orders) sample wires the same behaviour by hand —
`impl Message for …`, `bus.register(move |req| …)`,
`Router::new().route("/api/v1/orders", post(handler))`, a repository port and
adapter, and a five-module package layout. macro-quickstart collapses all of
that into the declarations above.

| Measure (`src/*.rs`) | orders (builder) | macro-quickstart (macros) | Win |
|----------------------|------------------|---------------------------|-----|
| Total lines | **1022** | **376** | **−63%** (2.7× smaller) |
| Code lines (non-comment, non-blank) | **587** | **182** | **−69%** (3.2× smaller) |
| Source modules | 7 (`interfaces`/`models`/`core`/`web`/`sdk`/`lib`/`main`) | 2 (`lib`/`main`) | −5 files |

Concretely, what the macros erase:

```rust
// BEFORE — builder wiring (orders): a Message impl + manual bus registration
impl Message for GetOrderQuery {
    fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(30)) }
}
pub fn register(bus: &Bus, repo: Arc<dyn Repository>) {
    bus.register(move |q: GetOrderQuery| {
        let repo = Arc::clone(&repo);
        async move { /* … look up, map errors … */ }
    });
}
pub fn api_router(bus: Arc<Bus>) -> Router {
    Router::new()
        .route("/api/v1/orders", post(place_order))
        .route("/api/v1/orders/:id", get(get_order))
        .with_state(bus)
}
```

```rust
// AFTER — macro declarations (macro-quickstart): the wiring is generated
#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetOrder { pub id: String }

#[query_handler]
pub async fn get_order(q: GetOrder) -> Result<OrderView, CqrsError> { /* … */ }

#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    #[post("")]      async fn create(/* … */) -> WebResult<Json<OrderView>> { /* … */ }
    #[get("/:id")]   async fn fetch(/* … */)  -> WebResult<Json<OrderView>> { /* … */ }
}
// register_get_order(&bus);  and  OrderApi::routes(state)  are generated.
```

## Run

```bash
cargo run -p firefly-sample-macro-quickstart
```

Serves the public API on `127.0.0.1:8080` (override with `QUICKSTART_ADDR`),
prints the Firefly banner, starts the `#[scheduled]` task, and shuts down
gracefully on SIGINT/SIGTERM.

### Place an order

```bash
curl -X POST http://127.0.0.1:8080/api/v1/orders \
  -H 'Content-Type: application/json' \
  -d '{"customer":"alice","sku":"SKU-1","quantity":2}'
```

### Read it back

```bash
curl http://127.0.0.1:8080/api/v1/orders/order-1
```

A `GET` for an unknown id renders as a `404`
`application/problem+json` (RFC 9457); a `POST` whose `customer`/`sku`/
`quantity` fails the `#[firefly(validate)]` checks renders as a `422`.

## Test

```bash
cargo test -p firefly-sample-macro-quickstart
```

The tests drive the macro-generated `OrderApi::routes(...)` router in-process
through `tower::ServiceExt::oneshot` (no socket bound), proving the generated
routes and CQRS handlers work end to end: a `POST` → `GET` round-trip, the 404
problem path, the 422 validation path, and that `#[scheduled]` registered a
named task on the scheduler.

## License

Apache-2.0. Copyright 2026 Firefly Software Foundation.
