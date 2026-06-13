# Declarative Services with Macros

Lumen is finished. Over twenty chapters it grew from an empty scaffold into a
secure, observable, event-sourced CQRS service with a transfer saga and a
streaming endpoint — and it depends on exactly **one** Firefly crate. This
capstone re-reads the whole service through a single lens: the **declarative
macros**. By the end of this chapter you will be able to point at every
`#[derive(...)]` and `#[...]` in `samples/lumen` and say precisely what wiring it
collapsed into a declaration next to the code. That is the thesis the running
crate proves: *one facade + macros = the framework, with the boilerplate gone.*

> **Spring parity.** The declarative layer is the Rust answer to Spring's
> annotation set and pyfly's decorators. What changes is *when* the wiring is
> generated — at compile time by a `proc-macro`, not at runtime by reflection —
> so there is no startup scanning cost and no reflective surprises. The
> programming model is the same: a declaration sits next to the code it
> describes, and the framework generates the glue.

## One dependency, one prelude

Every chapter began the same way. Lumen's `Cargo.toml` lists one Firefly crate:

```toml
[dependencies]
firefly = { workspace = true }   # the whole framework + every macro
axum    = { workspace = true }   # you author the controller handlers
serde   = { workspace = true }   # your messages + event payloads
serde_json = { workspace = true }
tokio   = { workspace = true }
uuid    = { workspace = true }
chrono  = { workspace = true }
async-trait = { workspace = true }
```

and every module opens with one glob:

```rust,ignore
use firefly::prelude::*;
```

That glob brings the whole framework into scope — `Bus`, `Container`,
`Scheduler`, `Saga`/`Step`, `Application`/`ShutdownHandle`, `Core`/`CoreConfig`,
`WebResult`/`WebError`, `FireflyError`/`FireflyResult`, `Mono`/`Flux` — **and**
every macro. There are also per-crate aliases (`firefly::cqrs::Bus`,
`firefly::eventsourcing::EventStore`, `firefly::security::JwtService`, …) for the
types you name explicitly. `axum` and `serde` are the only two ecosystem crates
Lumen writes against directly.

### Staying lean

The default `firefly` build pulls only the framework's lean *port* crates — no
heavy third-party drivers. Lumen needs none, so its build is minimal. Heavy
adapters are opt-in cargo features (the swap path every chapter pointed at):

| Feature | Pulls in |
|---------|----------|
| `data-sqlx` | relational repository adapter (Postgres / MySQL / SQLite) |
| `data-mongodb` | document repository adapter (MongoDB) |
| `eda-kafka` / `eda-rabbitmq` / `eda-redis` / `eda-postgres` | event-broker transports |
| `cache-redis` / `cache-postgres` | cache backends |
| `admin` | the admin dashboard |
| `full` | all of the above |

## The macros Lumen uses

Lumen exercises the full declarative set. Here is the catalogue, each mapped to
the exact Lumen file it lands in:

| Macro | Lumen file | Generates |
|-------|-----------|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | `commands.rs` | the `Message` impl (`#[firefly(validate)]`, `#[firefly(cache_ttl = "…")]`) |
| `#[command_handler]` / `#[query_handler]` | `commands.rs` | a `register_<fn>(bus)` helper |
| `#[derive(DomainEvent)]` | `domain.rs` | `EVENT_TYPE` + `to_domain_event` |
| `#[derive(AggregateRoot)]` | `domain.rs` | `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` |
| `#[event_listener(topic = "…")]` | `ledger.rs` | a `subscribe_<fn>(broker)` helper |
| `#[rest_controller]` + `#[get/post]` | `web.rs` | a `routes(state) -> axum::Router` |
| `#[scheduled(fixed_rate = "…")]` | `housekeeping.rs` | a `schedule_<fn>(scheduler)` helper |

The DI stereotype derives (`#[derive(Component/Service/Repository/Configuration/
Controller)]`, `#[bean]`, `#[autowired]`, `register_all!`) and
`#[derive(ConfigProperties)]` round out the set; Lumen wires its collaborators
explicitly rather than through the container (chapter 4), so the
[DI deep-dive](./04a-dependency-injection.md) is where you saw those at work.

### CQRS — messages and handlers (`commands.rs`)

`#[derive(Command)]` / `#[derive(Query)]` generate the `Message` impl.
`#[firefly(validate)]` makes an empty / zero field fail validation before the
handler runs; `#[firefly(cache_ttl = "…")]` feeds the query cache. These are
verbatim Lumen:

```rust,ignore
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command)]
#[serde(default)]
pub struct OpenWallet {
    #[firefly(validate)]
    pub owner: String,
    #[serde(rename = "openingBalance")]
    pub opening_balance: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    pub id: String,
}
```

`#[command_handler]` / `#[query_handler]` mark a free `async fn` and generate a
`register_<fn>(bus)` helper that installs it on the bus:

```rust,ignore
#[command_handler]
pub async fn open_wallet(cmd: OpenWallet) -> Result<WalletView, CqrsError> { /* … */ }

#[query_handler]
pub async fn get_wallet(q: GetWallet) -> Result<WalletView, CqrsError> { /* … */ }
// generated: register_open_wallet(&bus);  register_get_wallet(&bus);
```

Lumen calls each generated helper in one `register(&bus)` fn. Because free fns
cannot capture wiring state, the resolved `Ledger` + `ReadModel` are published
once through a `bind` / `OnceLock` and reached from the handlers — the pattern
chapter 9 introduced.

> **Spring parity.** `#[derive(Command)]` + `#[command_handler]` is
> `@CommandHandler` on a typed handler; `#[firefly(cache_ttl)]` is `@Cacheable`
> on a query. The `Bus` is the command/query gateway.

### Event sourcing — domain events and the aggregate (`domain.rs`)

`#[derive(DomainEvent)]` stamps each payload with a stable `EVENT_TYPE`
discriminator (its struct name) and a `to_domain_event` conversion;
`#[derive(AggregateRoot)]` finds the embedded `AggregateRoot` field and generates
`Wallet::AGGREGATE_TYPE` plus `aggregate()` / `aggregate_mut()`:

```rust,ignore
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    pub opening_balance: i64,
}

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    pub root: AggregateRoot,   // the framework root — uncommitted-event buffer + version
    pub owner: String,
    pub balance: Money,
    pub opened: bool,
}
```

The only event-sourcing wiring Lumen writes by hand is the `apply` fold; the
discriminators and the wire conversion are generated.

> **Spring parity.** `#[derive(AggregateRoot)]` is the Axon-style event-sourced
> aggregate; the `EVENT_TYPE` discriminator is the `@Revision`/`@EventType`
> tagging that keeps the JSON wire-compatible across the ports.

### Messaging — the projection listener (`ledger.rs`)

`#[event_listener(topic = "…")]` marks a free `async fn(Event) -> FireflyResult<()>`
and generates a `subscribe_<fn>(broker)` helper that subscribes it to the topic.
Lumen's read-model projection is one declaration:

```rust,ignore
#[event_listener(topic = "wallets.events")]
pub async fn project_wallet_event(ev: Event) -> FireflyResult<()> {
    // reload the wallet's stream, fold to a WalletView, upsert — idempotent.
    Ok(())
}
// generated: subscribe_project_wallet_event(broker)
```

The composition root calls `ledger::subscribe_project_wallet_event(broker)` after
binding, closing the CQRS loop.

> **Spring parity.** `#[event_listener(topic = "…")]` is `@KafkaListener` /
> `@RabbitListener` — the topic subscription is generated; you write only the
> handler body.

### Web — the controller (`web.rs`)

`#[rest_controller(path = "…")]` turns an `impl` block into a generated
`WalletApi::routes(state) -> axum::Router`. Each method carries one verb mapping
and uses ordinary axum extractors, returning `WebResult<T>` so a handler error
renders as RFC 9457 `application/problem+json`:

```rust,ignore
#[rest_controller(path = "/api/v1")]
impl WalletApi {
    #[post("/wallets")]
    async fn open(State(api): State<WalletApi>, Json(body): Json<OpenWallet>)
        -> WebResult<(axum::http::StatusCode, Json<WalletView>)> { /* dispatch via api.bus */ }

    #[get("/wallets/:id")]
    async fn get(State(api): State<WalletApi>, Path(id): Path<String>)
        -> WebResult<Json<WalletView>> { /* … */ }
    // … deposit / withdraw / transfer
}
// generated: WalletApi::routes(state) -> axum::Router
```

The macro also submits each route into a link-time table, so the OpenAPI
generator and the actuator `/mappings` endpoint can enumerate Lumen's routes
without re-parsing the source.

> **Spring parity.** `#[rest_controller]` + `#[get]`/`#[post]` is
> `@RestController` + `@GetMapping`/`@PostMapping`; `WebResult<T>` rendering RFC
> 9457 is `@ControllerAdvice` returning a `ProblemDetail`.

### Scheduling — the heartbeat (`housekeeping.rs`)

`#[scheduled(...)]` generates a `schedule_<fn>(scheduler)` helper that registers a
zero-argument `async fn` on a `Scheduler`:

```rust,ignore
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> { /* … */ }
// generated: schedule_ledger_heartbeat(&scheduler)
```

> **Spring parity.** `#[scheduled(fixed_rate = "60s")]` is
> `@Scheduled(fixedRate = 60000)`; `#[scheduled(cron = "…")]` is the cron form.

## How it works: the `__rt` contract

A `proc-macro` crate cannot re-export runtime types, so macro-generated code
references every runtime type through the facade's hidden **`__rt` contract
path** — e.g. `::firefly::__rt::firefly_cqrs::Bus`. That is why Lumen, depending
only on `firefly` (plus the `axum`/`serde` it writes against anyway), compiles
whatever a macro expands to without listing the underlying `firefly-*` crates.
You never write `__rt` yourself. If you rename or shim the facade, pass
`#[firefly(crate = "my_firefly")]` to any macro to override the leading segment.

Rust has no reflective autowiring, so the declarative layer removes the
*mechanical* boilerplate, not the explicitness: Lumen still calls
`register(&bus)`, still calls `subscribe_project_wallet_event(broker)`, still
hands `WalletApi::routes(state)` to the web stack. The macros generate the
`impl`s, the routers, and the helper functions — the wiring you would otherwise
hand-write.

## The whole crate, declaratively

Read top to bottom, the macros tell Lumen's story:

```text
  money.rs        (no macros — a pure value object; the no-thiserror promise)
  domain.rs       #[derive(DomainEvent)] x3   #[derive(AggregateRoot)]
  ledger.rs       #[event_listener(topic = "wallets.events")]
  commands.rs     #[derive(Command)] x3   #[derive(Query)]
                  #[command_handler] x3   #[query_handler]
  transfer.rs     (Saga / Step builder — orchestration is a runtime API, not a macro)
  security.rs     (JwtService / BearerLayer / FilterChain — runtime APIs)
  web.rs          #[rest_controller] + #[get] / #[post] x6
  housekeeping.rs #[scheduled(fixed_rate = "60s", initial_delay = "5s")]
```

Note what is *not* a macro: the transfer saga and the security filter chain are
built with runtime builders (`Saga::new(...).step(...)`,
`FilterChain::new().require(...)`), because their shape is data, not a fixed
declaration — and Lumen keeps them explicit so the control flow stays visible.
Declarative where it collapses boilerplate, explicit where the graph is the
point: that balance is the whole design.

## Verifying the crate

Everything above compiles and is tested. From the workspace root:

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                       # 34 unit + 7 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming  # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
```

The HTTP tests drive the macro-generated `WalletApi::routes(...)` router
in-process through `tower::ServiceExt::oneshot` (no socket bound), proving the
generated routes, the CQRS handlers, validation (422), the not-found path (404),
the auth boundary (401), the transfer saga (happy + compensation), and the
projection convergence all work end to end — every prose listing in this book is
a slice of that running crate.

## What changed in Lumen

Nothing — this chapter is the retrospective, not a new feature. Re-read as a
catalogue, Lumen's macros are: three `#[derive(DomainEvent)]` + one
`#[derive(AggregateRoot)]` (`domain.rs`); one `#[event_listener]` (`ledger.rs`);
three `#[derive(Command)]` + one `#[derive(Query)]` + three `#[command_handler]`
+ one `#[query_handler]` (`commands.rs`); one `#[rest_controller]` with six verb
methods (`web.rs`); and one `#[scheduled]` (`housekeeping.rs`). Each replaced a
chunk of hand-written wiring with a declaration next to the code — and all of it
arrived through one dependency and one prelude glob.

## Exercises

1. **Trace one macro end to end.** Pick `#[derive(Query)]` on `GetWallet`. Find
   where its generated `cache_ttl()` is read (the `QueryCache` middleware in
   `web.rs`) and the test that asserts it (`get_wallet_carries_cache_ttl` in
   `commands.rs`). Change the TTL to `"5s"` and re-run the tests.
2. **Add a verb.** Add a `#[get("/wallets/:id/events")]`-style read method to the
   `#[rest_controller]` impl (non-streaming: return the event list as JSON) and
   confirm `WalletApi::routes` picks it up with no other change.
3. **Add a scheduled task.** Write a second `#[scheduled(cron = "0 0 * * *")]`
   function in `housekeeping.rs`, register it in `build_scheduler`, and assert it
   appears in `scheduler.tasks()`.
4. **Count the wiring you didn't write.** For each macro in the catalogue, name
   the helper or impl it generated (`register_*`, `subscribe_*`, `schedule_*`,
   `routes`, `EVENT_TYPE`, `AGGREGATE_TYPE`). That list is the boilerplate the
   declarative layer wrote for you.

That is Lumen, complete and declarative. The appendices that follow are
reference: a [Spring-Boot → Firefly-Rust cheat sheet](./90-appendix-spring.md), a
[module index](./91-appendix-modules.md), and a [glossary](./92-glossary.md).
