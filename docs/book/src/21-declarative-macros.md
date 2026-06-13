# Declarative Services with Macros

Every chapter so far wired the framework by hand — `impl Message for …`,
`bus.register(move |req| …)`, `Router::new().route(…)`, `Scheduler::fixed_rate(…)`.
That is explicit and fully type-safe, and it is exactly what the framework runs.
But for the common cases it is also *mechanical*. Version 26.6.3 adds the
**declarative layer** — the Rust answer to Spring annotations and the pyfly
decorator set — so that the common cases become a *declaration sitting next to
the code it describes*, with the wiring generated for you.

It is two crates:

- [`firefly`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/crates/firefly)
  — the **one-dependency facade**, and
- [`firefly-macros`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/crates/macros)
  — the `#[derive(...)]` / `#[...]` macros (re-exported through the facade).

## One dependency, one prelude

Before, a service listed ten-to-fifteen `firefly-*` crates and imported from
each. With the facade you add one dependency:

```toml
[dependencies]
firefly = "26.6.3"            # the whole framework + every macro
axum    = "0.7"               # you author axum handlers
serde   = { version = "1", features = ["derive"] }
tokio   = { version = "1", features = ["rt-multi-thread", "macros"] }
```

and glob-import one prelude:

```rust,ignore
use firefly::prelude::*;
```

That glob brings the whole framework into scope — `Bus`, `Container`,
`Scheduler`, `Saga`/`Step`, `Application`/`ShutdownHandle`, `Core`/`CoreConfig`,
`WebResult`/`WebError`/`problem_response`, `FireflyError`/`FireflyResult`,
`Mono`/`Flux` — **and** every macro. There are also ergonomic per-crate aliases
(`firefly::cqrs::Bus` instead of `firefly_cqrs::Bus`, one for every runtime
crate). `serde` and `axum` are the only two ecosystem crates a Firefly service
still writes against directly.

### Staying lean

The default `firefly` build pulls only the framework's lean *port* crates — no
heavy third-party drivers. Heavy adapters are opt-in cargo features:

```toml
firefly = { version = "26.6.3", features = ["data-sqlx", "eda-kafka"] }
```

| Feature | Pulls in |
|---------|----------|
| `data-sqlx` | relational repository adapter (Postgres / MySQL / SQLite) |
| `data-mongodb` | document repository adapter (MongoDB) |
| `eda-kafka` / `eda-rabbitmq` / `eda-redis` / `eda-postgres` | event-broker transports |
| `cache-redis` / `cache-postgres` | cache backends |
| `admin` | the admin dashboard |
| `full` | all of the above |

A minimal `firefly` dependency compiles none of them.

## The macros

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | a message struct | `impl Message` (`#[firefly(validate)]` / `#[firefly(cache_ttl = "…")]`) |
| `#[command_handler]` / `#[query_handler]` | `async fn(Msg) -> Result<R, CqrsError>` | a `register_<fn>(bus)` helper |
| `#[derive(Component)]` / `#[derive(Service)]` / `#[derive(Repository)]` | a struct with `#[autowired]` fields | a `firefly_register(container)` method |
| `register_all!(&container, [A, B, …])` | — | calls each type's `firefly_register` |
| `#[scheduled]` | a zero-arg `async fn` | a `schedule_<fn>(scheduler)` helper |
| `#[rest_controller]` + `#[get/post/put/delete/patch]` | an `impl` block | a `routes(state) -> axum::Router` |
| `#[derive(DomainEvent)]` / `#[derive(AggregateRoot)]` | a struct | event-type / aggregate ergonomics |
| `#[event_listener]` | `async fn(Event) -> FireflyResult<()>` | a `subscribe_<fn>(broker)` helper |

### CQRS — messages and handlers

`#[derive(Command)]` / `#[derive(Query)]` generate the `Message` impl;
`#[firefly(validate)]` makes an empty / zero field fail validation before the
handler runs, and `#[firefly(cache_ttl = "…")]` feeds the query cache:

```rust,ignore
use firefly::prelude::*;
use serde::Serialize;

#[derive(Clone, Serialize, Command)]
pub struct PlaceOrder {
    #[firefly(validate)] pub customer: String,
    #[firefly(validate)] pub sku: String,
    #[firefly(validate)] pub quantity: u32,
}

#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetOrder { pub id: String }
```

`#[command_handler]` / `#[query_handler]` mark a free `async fn` and generate a
`register_<fn>(bus)` helper that installs it on a `Bus`:

```rust,ignore
#[command_handler]
pub async fn place_order(cmd: PlaceOrder) -> Result<OrderView, CqrsError> { /* … */ }

#[query_handler]
pub async fn get_order(q: GetOrder) -> Result<OrderView, CqrsError> { /* … */ }
// generated: register_place_order(&bus);  register_get_order(&bus);
```

### Components — dependency injection

`#[derive(Component)]` (and the `Service` / `Repository` aliases) generates a
`firefly_register(container)` method; `register_all!` installs a list of them in
the DI [`Container`](./04-dependency-wiring.md):

```rust,ignore
#[derive(Component, Default)]
#[firefly(scope = "singleton")]
pub struct OrderStore { /* … */ }

let container = Container::new();
register_all!(&container, [OrderStore]);
let store = container.resolve::<OrderStore>().unwrap();
```

### REST controllers

`#[rest_controller(path = "…")]` turns an `impl` block into a generated
`routes(state) -> axum::Router`. Methods carry `#[get]` / `#[post]` / … and use
ordinary axum extractors, returning `WebResult<T>` so errors render as RFC 9457
problems:

```rust,ignore
#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    #[post("")]
    async fn create(State(api): State<OrderApi>, Json(body): Json<PlaceOrder>)
        -> WebResult<Json<OrderView>> { /* dispatch through api.bus … */ }

    #[get("/:id")]
    async fn fetch(State(api): State<OrderApi>, Path(id): Path<String>)
        -> WebResult<Json<OrderView>> { /* … */ }
}
// generated: OrderApi::routes(OrderApi { bus })  ->  axum::Router
```

### Scheduling and events

`#[scheduled]` generates a `schedule_<fn>(scheduler)` helper, and
`#[event_listener]` a `subscribe_<fn>(broker)` helper:

```rust,ignore
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn sweep_stale_orders() -> Result<(), std::io::Error> { /* … */ }

#[event_listener("orders.created")]
async fn on_order_created(ev: Event) -> FireflyResult<()> { Ok(()) }
```

## How it works: the `__rt` contract

A `proc-macro` crate cannot re-export runtime types, so macro-generated code
references every runtime type through the facade's hidden **`__rt` contract
path** — e.g. `::firefly::__rt::firefly_cqrs::Bus`. That is why a service that
depends only on `firefly` (plus the `axum`/`serde` it writes against anyway)
compiles whatever a macro expands to without listing the underlying `firefly-*`
crates. You never write `__rt` yourself. If you rename or shim the facade, pass
`#[firefly(crate = "my_firefly")]` to any macro to override the leading segment.

Rust has no package scanning or reflective autowiring, so the declarative layer
removes the *mechanical* boilerplate, not the explicitness: you still list the
components in `register_all!` and still register the generated handlers on the
bus — the macros just generate the `impl`s, the routers, and the helper
functions you would otherwise hand-write.

## Before / after

The [`macro-quickstart`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/macro-quickstart)
sample re-implements the [`orders`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/orders)
reference service through the declarative layer. The same behaviour, far less
code:

| Measure (`src/*.rs`) | orders (builder) | macro-quickstart (macros) | Win |
|----------------------|------------------|---------------------------|-----|
| Total lines | 1022 | 376 | −63% |
| Code lines | 587 | 182 | −69% |
| Source modules | 7 | 2 | −5 files |

```rust,ignore
// BEFORE — builder wiring: a Message impl + manual bus registration + router
impl Message for GetOrderQuery {
    fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(30)) }
}
pub fn register(bus: &Bus, repo: Arc<dyn Repository>) {
    bus.register(move |q: GetOrderQuery| { /* … */ });
}
pub fn api_router(bus: Arc<Bus>) -> Router {
    Router::new()
        .route("/api/v1/orders", post(place_order))
        .route("/api/v1/orders/:id", get(get_order))
        .with_state(bus)
}

// AFTER — macro declarations: the wiring is generated
#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetOrder { pub id: String }

#[query_handler]
pub async fn get_order(q: GetOrder) -> Result<OrderView, CqrsError> { /* … */ }

#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    #[post("")]    async fn create(/* … */) -> WebResult<Json<OrderView>> { /* … */ }
    #[get("/:id")] async fn fetch(/* … */)  -> WebResult<Json<OrderView>> { /* … */ }
}
// register_get_order(&bus);  and  OrderApi::routes(state)  are generated.
```

Run and test the sample:

```bash
cargo run  -p firefly-sample-macro-quickstart
cargo test -p firefly-sample-macro-quickstart
```

The tests drive the macro-generated `OrderApi::routes(...)` router in-process
through `tower::ServiceExt::oneshot` (no socket bound), proving the generated
routes, CQRS handlers, validation (422), the not-found path (404), and the
`#[scheduled]` registration all work end to end.
