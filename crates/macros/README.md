# firefly-macros

The Firefly Framework's **declarative service-development layer** — a Rust
take on annotation-style, declaration-next-to-code wiring. A `proc-macro` crate
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

### Dependency injection — the headline surface

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(Component)]` / `Service` / `Repository` / `Configuration` / `Controller` | a struct | `T::firefly_register(container)` + an `inventory` scan thunk |
| `#[derive(ConfigProperties)]` | a `Deserialize` struct | config-bound injectable bean (Spring `@ConfigurationProperties`) |
| `#[bean]` | an `impl` block of a `Configuration` | `T::firefly_register_beans(container)` — `@Bean` factories |
| `register_all!(c, [A, B, …])` | — | calls each type's `firefly_register` (the generic fallback to `scan()`) |

Every stereotype derive also submits an [`inventory`] thunk, so
`Container::scan()` (or `firefly::scan(&container)`, or
`ApplicationContext`) discovers and registers it across the whole crate graph —
classpath-style component scanning at compile time. *Generic* types can't be
inventoried; register those with `register_all!`.

```rust,ignore
#[derive(Repository, Default)]
struct OrderRepo { /* … */ }

#[derive(Service)]
#[firefly(scope = "singleton", primary, order = 10, profile = "prod",
          condition_on_property = "orders.enabled=true",
          provides = "dyn OrderPort",          // also binds the trait object
          post_construct = "warm", pre_destroy = "drain")]
struct OrderService {
    #[autowired] repo: Arc<OrderRepo>,          // resolve::<OrderRepo>()
    #[autowired] plugins: Vec<Arc<Plugin>>,     // resolve_all::<Plugin>()
    #[autowired] cache: Option<Arc<Cache>>,     // resolve(..).ok() (required=false)
    #[autowired] tickets: Provider<Ticket>,     // deferred provider::<Ticket>()
    #[firefly(qualifier = "primary_db")] db: Arc<DataSource>,
    #[firefly(value = "${orders.batch:50}")] batch: usize,   // @Value config injection
}
impl OrderService { fn warm(&mut self) {} fn drain(&self) {} }

// Component scan registers everything, honoring conditionals/profiles:
let ctx = firefly::ApplicationContext::builder().profiles(["prod"]).build();
let svc = ctx.resolve::<OrderService>()?;
ctx.close();                                    // runs #[pre_destroy] in reverse
```

**Field injection forms** (selected by the field type): `Arc<T>`,
`Vec<Arc<T>>`, `Option<Arc<T>>`, `Provider<T>`,
`#[firefly(qualifier = "name")]` (resolve by name), and
`#[firefly(value = "${key:default}")]` (config value parsed via `FromStr`); any
other field is `Default`-built.

**Struct `#[firefly(...)]` options:** `scope`, `name`, `primary`, `order = N`,
`lazy`, `profile = "expr"`, `condition_on_property = "k=v"`,
`condition_on_class = "label"`, `condition_on_bean = "Type"`,
`condition_on_missing_bean = "Type"`, `condition_on_single_candidate = "Type"`,
`provides = "dyn Port"`, `post_construct = "method"`, `pre_destroy = "method"`.

**`#[bean]` factories** on a `#[derive(Configuration)]` holder register one bean
per method, keyed by the method's return type; method `Arc<Dep>` arguments are
resolved from the container. Per-method options:
`#[bean(name = "...", scope = "...", primary, profile = "...")]`.

```rust,ignore
#[derive(Configuration, Default)] struct AppConfig;

#[firefly::bean]
impl AppConfig {
    #[bean(name = "clock", primary)]
    fn clock(&self) -> SystemClock { SystemClock::new() }   // concrete return type
    #[bean]
    fn repo(&self, db: Arc<DataSource>) -> SqlRepo { SqlRepo::new(db) }
}
AppConfig::firefly_register(&c);
AppConfig::firefly_register_beans(&c);
```

A `#[bean]` method returns a **concrete (sized) type** — that is the bean's key.
To expose it behind a trait, give the *holder* `#[firefly(provides = "dyn T")]`
or call `container.bind::<dyn T, Concrete>(|a| a)` after registration.

**`#[derive(ConfigProperties)]`** binds a struct from config under a prefix and
registers it as an injectable singleton (Spring `@EnableConfigurationProperties`):

```rust,ignore
#[derive(serde::Deserialize, ConfigProperties)]
#[firefly(prefix = "app.db")]
struct DbProperties { url: String, pool_size: u32 }
```

### Scheduling

`#[scheduled]` on a zero-argument `async fn` generates `schedule_<fn>(scheduler)`.
**Exactly one trigger is required — a violation is a compile error** (the
constraint is enforced at compile time rather than deferred to runtime).

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

`#[rest_controller]` also emits **route metadata** — a `Controller::ROUTES`
const slice of `RouteDescriptor { controller, method, path, handler }` and an
`inventory` submission — so the OpenAPI generator can enumerate every route via
`firefly::container::routes()` without re-parsing source.

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
  container resolve, a broker delivery, the `ROUTES` metadata).
- `tests/di.rs` proves the full DI surface end-to-end: `scan()` registers all
  stereotypes; `#[bean]` factories resolve; primary/order/qualifier
  disambiguation; `Vec`/`Provider`/`Option` injection; `#[post_construct]` /
  `#[pre_destroy]` ordering; conditional/profile gating; interface auto-binding;
  `@Value` and `#[derive(ConfigProperties)]` binding.
- `tests/trybuild.rs` runs the `tests/ui/pass/*` compile-pass cases (every macro,
  incl. the full DI surface) and the `tests/ui/fail/*` compile-fail cases (pinned
  diagnostics for no/two `#[scheduled]` triggers, a controller with no routes, a
  bad handler arity, a bad `#[autowired]` field type, a `#[bean]` impl with no
  methods). Regenerate the `.stderr` snapshots with `TRYBUILD=overwrite cargo test`.

[`inventory`]: https://docs.rs/inventory

## License

Apache-2.0. Copyright 2026 Firefly Software Foundation.
