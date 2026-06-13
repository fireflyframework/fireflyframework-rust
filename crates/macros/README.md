# firefly-macros

The Firefly Framework's **declarative service-development layer** — the Rust
answer to Spring annotations and the pyfly decorator set. A `proc-macro` crate
of `#[derive(...)]` and `#[...]` attribute macros that collapse the framework's
closure/builder wiring into declarations sitting next to the code they describe.

It is normally reached through the [`firefly`](../firefly) facade
(`use firefly::prelude::*;`), which re-exports every macro at its root and in
its prelude. You should not depend on `firefly-macros` directly.

## The `__rt` contract

A `proc-macro` crate cannot re-export runtime types, so **every macro generates
code that references runtime types through the `firefly` facade's hidden
`__rt` contract path** — e.g. `::firefly::__rt::firefly_cqrs::Bus`. A service
that depends only on the `firefly` facade therefore compiles whatever a macro
expands to without listing the underlying `firefly-*` crates.

Override the leading facade segment with `#[firefly(crate = "...")]` on any
macro (for users who rename or shim the facade):

```rust,ignore
#[command_handler(crate = "my_firefly")]
async fn handle(cmd: CreateUser) -> Result<UserCreated, CqrsError> { /* … */ }
```

Two macros also reference an ecosystem crate the user already depends on:
`#[rest_controller]` emits `::axum::…` (you are writing axum handlers), and
`#[derive(DomainEvent)]`'s `to_domain_event` uses `::serde_json` (your payload
is `#[derive(Serialize)]`).

## The macros

### CQRS

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | a message struct | `impl firefly_cqrs::Message` |
| `#[command_handler]` / `#[query_handler]` | `async fn(Msg) -> Result<R, CqrsError>` | `register_<fn>(bus)` |

```rust,ignore
use firefly::prelude::*;
use serde::Serialize;

#[derive(Clone, Serialize, Command)]
struct CreateUser {
    #[firefly(validate)]            // empty/default → validation error
    name: String,
}

#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]      // Message::cache_ttl → QueryCache memoises
struct GetUser { id: String }

#[command_handler]
async fn handle_create_user(cmd: CreateUser) -> Result<UserCreated, CqrsError> {
    Ok(UserCreated { /* … */ })
}

// at startup:
let bus = Bus::new();
register_handle_create_user(&bus);
```

### Dependency injection

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(Component)]` / `Service` / `Repository` | a struct | `T::firefly_register(container)` |
| `register_all!(c, [A, B, …])` | — | calls each type's `firefly_register` in order |

```rust,ignore
#[derive(Repository, Default)]
struct OrderRepo { /* … */ }

#[derive(Service)]
#[firefly(scope = "singleton", primary)]
struct OrderService {
    #[autowired]                   // resolved via Container::resolve::<OrderRepo>()
    repo: std::sync::Arc<OrderRepo>,
}

let container = Container::new();
firefly::register_all!(&container, [OrderRepo, OrderService]);
let svc = container.resolve::<OrderService>()?;
```

`#[autowired]` fields must be typed `Arc<Dep>` (the container resolves beans as
`Arc<T>`); a `#[autowired(qualifier = "name")]` resolves by bean name. Struct
options: `scope = "singleton" | "transient" | "request" | "session"`,
`name = "…"`, `primary`.

### Scheduling

`#[scheduled]` on a zero-argument `async fn` generates `schedule_<fn>(scheduler)`.
**Exactly one trigger is required — a violation is a compile error** (pyfly's
runtime `ValueError`, lifted to compile time).

```rust,ignore
#[scheduled(cron = "0 2 * * *", zone = "America/New_York")]
async fn nightly_close() -> Result<(), MyError> { Ok(()) }

#[scheduled(fixed_rate = "30s", initial_delay = "5s")]
async fn flush() -> Result<(), MyError> { Ok(()) }

schedule_nightly_close(&scheduler);
schedule_flush(&scheduler);
```

Durations accept `s`/`ms`/`us`/`m`/`h`/`d` suffixes (a bare integer is seconds).

### Web

`#[rest_controller(path = "…")]` on an `impl` block generates
`fn routes(state) -> axum::Router`. Methods carry `#[get("/:id")]` / `#[post]` /
`#[put]` / `#[delete]` / `#[patch]`; their signatures use ordinary axum
extractors and return `firefly_web::WebResult<T>` so errors render as RFC 7807
problems.

```rust,ignore
use axum::extract::{Path, State};
use axum::Json;

#[derive(Clone)]
struct OrderApi { /* shared state */ }

#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    #[get("/:id")]
    async fn get_order(State(api): State<OrderApi>, Path(id): Path<String>)
        -> WebResult<Json<OrderView>> { /* … */ }

    #[post("")]
    async fn create(State(api): State<OrderApi>, Json(body): Json<CreateOrder>)
        -> WebResult<Json<OrderView>> { /* … */ }
}

let router = OrderApi::routes(OrderApi { /* … */ });
```

The `state` defaults to the controller (`Self`); override with
`#[rest_controller(path = "…", state = "MyState")]`.

### Event sourcing

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(DomainEvent)]` | a `Serialize` payload struct | `EVENT_TYPE`, `event_type()`, `to_domain_event(id, type, version)` |
| `#[derive(AggregateRoot)]` | a struct embedding `AggregateRoot` | `AGGREGATE_TYPE`, `aggregate()`, `aggregate_mut()` |

```rust,ignore
#[derive(Clone, Serialize, Deserialize, DomainEvent)]
struct AccountOpened { owner: String }

#[derive(Default, AggregateRoot)]
#[firefly(aggregate_type = "Account")]
struct Account {
    root: firefly::eventsourcing::AggregateRoot,   // embedded field (named `root`)
}
```

### Event-driven messaging

`#[event_listener("topic")]` on an `async fn(Event) -> FireflyResult<()>`
generates an async `subscribe_<fn>(broker)` helper.

```rust,ignore
#[event_listener("orders.created")]
async fn on_order_created(ev: Event) -> FireflyResult<()> { Ok(()) }

subscribe_on_order_created(&broker).await?;          // group via topic = / group =
```

## Tests

- `tests/behavioral.rs`, `tests/scheduling.rs`, `tests/web.rs`, `tests/eda.rs`
  drive each macro's generated code against the real `firefly` facade (a router
  via `tower::oneshot`, a CQRS round-trip on a `Bus`, a scheduler tick, a
  container resolve, a broker delivery).
- `tests/trybuild.rs` runs the `tests/ui/pass/*` compile-pass cases and the
  `tests/ui/fail/*` compile-fail cases (pinned diagnostics for no/two
  `#[scheduled]` triggers, a controller with no routes, a bad handler arity).
  Regenerate the `.stderr` snapshots with `TRYBUILD=overwrite cargo test`.

## License

Apache-2.0. Copyright 2026 Firefly Software Foundation.
