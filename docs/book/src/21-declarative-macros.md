# Declarative Services with Macros

Lumen is finished. Over twenty chapters it grew from an empty scaffold into a
secure, observable, event-sourced CQRS service with a transfer saga, a
compliance workflow, a two-phase transfer, a scheduled heartbeat, and an
optional streaming endpoint — and it depends on exactly **one** Firefly crate.
This capstone re-reads the whole service through a single lens: the
**declarative macros**. By the end you will be able to point at every
`#[derive(...)]` and `#[...]` in `samples/lumen` and say precisely what wiring it
collapsed into a declaration next to the code. That is the thesis the running
crate proves: *one facade plus macros equals the framework, with the boilerplate
gone.*

This chapter does not introduce a new feature. It is a guided walk-through of the
declarative layer you have been using all along, slowed down so every macro is
explained from first principles before it is read in context. Where a macro is
exercised in `samples/lumen` we read the verbatim Lumen source; where it is a
first-class part of the framework that Lumen happens not to use, we read a
focused standalone example and say so.

By the end of this chapter you will:

- Explain what a *declarative macro* is in Firefly, and why the generated wiring
  is checked by the compiler rather than discovered by runtime reflection.
- Trace each macro Lumen uses — `#[derive(Command/Query)]`, `#[handlers]`,
  `#[derive(DomainEvent/AggregateRoot)]`, `#[event_listener]`,
  `#[rest_controller]`, `#[scheduled]`, `#[firefly::saga/workflow/tcc]` — to the
  exact `impl`, router, or registration it emits.
- Name the supporting declarative set Lumen does not use — `#[derive(Builder)]`,
  `#[derive(Mapper)]`, `#[derive(Entity)]` / `#[derive(SqlxRepository)]` /
  `#[firefly::repository]` / `#[firefly::transactional]`, the method-security and
  resilience decorators, `#[cacheable]`, and the rest — and read a correct
  example of each.
- Describe the hidden `__rt` contract path that lets a one-crate service compile
  whatever a macro expands to.
- Explain the `inventory` drain: how a *declared* bean, listener, task, or
  controller becomes *wired* at boot with no hand-written registration call.
- Verify the entire crate builds, tests, and lints clean from the workspace root.

## Concepts you will meet

Before the catalogue, here are the four ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — declarative macro.** A *declarative macro* is an
> attribute (`#[...]`) or derive (`#[derive(...)]`) that a `proc-macro` expands
> at **compile time** into the `impl`s, routers, and helper functions you would
> otherwise hand-write. The declaration sits next to the code it describes; the
> compiler checks the generated code like any other source. The Spring analog is
> an annotation (`@RestController`, `@Component`) — except Spring discovers and
> processes annotations by reflection at startup, while Firefly resolves them at
> compile time.

> **Note** **Key term — facade and prelude.** The *facade* is the single
> `firefly` crate that re-exports the whole framework and every macro; the
> *prelude* is `firefly::prelude`, a module of the high-frequency types you glob
> in with `use firefly::prelude::*;`. Depending on one facade and importing one
> prelude is the entire "one dependency, one import" story. The Spring analog is
> a single Spring Boot starter plus the auto-imported framework types.

> **Note** **Key term — bean.** A *bean* is an object the framework constructs,
> manages, and hands to whoever declares it needs it (with `#[autowired]`). You
> declare beans; the framework discovers them at startup and wires them together.
> This is exactly Spring's notion of a bean managed by the application context.

> **Note** **Key term — inventory registry.** `inventory` is a Rust crate that
> lets a macro register a value into a process-global table **at link time** —
> before `main` runs. Each declarative macro that produces a handler, listener,
> task, or controller submits a *registration* into one of these tables;
> `FireflyApplication` **drains** the tables at boot and installs each entry. The
> effect mirrors Spring's classpath component scan, but the inventory is built by
> the linker, not by walking the classpath at runtime.

## Step 1 — One dependency, one prelude

Every chapter began the same way, so start there. Open Lumen's `Cargo.toml`. It
lists exactly one Firefly crate; everything declarative arrives through it.

```toml
# samples/lumen/Cargo.toml
[dependencies]
# The one-dependency story: the `firefly` facade re-exports the whole framework
# AND every `#[derive(...)]` / `#[...]` macro. Generated code resolves runtime
# types through the facade, so Lumen never lists the underlying `firefly-*`
# crates. The `admin` feature pulls in the self-hosted admin dashboard.
firefly = { version = "26.6.28", features = ["admin"] }

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and event
# payloads are Serialize/Deserialize). `serde_json` encodes the event payloads.
axum = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Async runtime for `#[tokio::main]`, and the id/clock crates the domain uses.
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "signal"] }
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
async-trait = "0.1"

[features]
# The reactive streaming endpoint is feature-gated so the teaching baseline
# stays lean; the production chapter turns it on. It needs nothing beyond the
# `firefly` facade.
default = []
streaming = []
```

And every module opens with one glob import:

```rust,ignore
use firefly::prelude::*;
```

What just happened: that glob brings the whole high-frequency surface into scope
— `Bus`, `Container`, `Scheduler`, `Saga` / `Step`, `Application` /
`ShutdownHandle`, `Core` / `CoreConfig`, `WebResult` / `WebError`, `FireflyError`
/ `FireflyResult`, `Mono` / `Flux` — **and** every macro. For the types you name
explicitly there are per-crate aliases (`firefly::cqrs::Bus`,
`firefly::eventsourcing::EventStore`, `firefly::security::JwtService`, …), which
is why several Lumen modules also write `use firefly::cqrs::QueryCache;` or
`use firefly::eda::{Broker, Event};` next to the prelude glob. `axum` and `serde`
are the only two ecosystem crates Lumen writes against directly.

> **Note** A `proc-macro` crate cannot itself re-export runtime types, so
> macro-generated code references every runtime type through the facade's hidden
> `__rt` contract path — for example `::firefly::__rt::firefly_cqrs::Bus`. That
> is why Lumen, depending only on `firefly`, compiles whatever a macro expands to
> without ever listing the underlying `firefly-*` crates. You never write `__rt`
> yourself; if you rename or shim the facade, pass `#[firefly(crate =
> "my_firefly")]` to any macro to override the leading segment. We return to this
> contract in [Step 11](#step-11--how-the-wiring-actually-lands-the-__rt-contract-and-the-inventory-drain).

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
| `admin` | the self-hosted admin dashboard |
| `full` | all of the above |

> **Tip** **Checkpoint.** Open `samples/lumen/Cargo.toml` and confirm there is
> exactly one `firefly = { … }` line under `[dependencies]`, and that every
> source file under `samples/lumen/src/` opens with `use firefly::prelude::*;`.
> That one dependency and one import are the whole premise this chapter unpacks.

## Step 2 — The macro catalogue, mapped to Lumen files

Lumen exercises the core declarative set. Before reading any one macro in depth,
here is the map: each macro, the exact Lumen file it lands in, and what it
generates.

| Macro | Lumen file | Generates |
|-------|-----------|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | `commands.rs` | the `Message` impl (`#[firefly(validate)]`, `#[firefly(cache_ttl = "…")]`) |
| `#[derive(Schema)]` | `commands.rs`, `domain.rs`, `web.rs`, … | an OpenAPI schema for the type, so it appears in `/v3/api-docs` |
| `#[handlers]` (on a handler-bean `impl`) | `commands.rs`, `ledger.rs` | registers each `#[command_handler]` / `#[query_handler]` / `#[event_listener]` method of a DI bean on the bus / broker |
| `#[command_handler]` / `#[query_handler]` (method markers) | `commands.rs` | mark a CQRS handler method inside a `#[handlers]` impl |
| `#[derive(DomainEvent)]` | `domain.rs` | `EVENT_TYPE` discriminator + `to_domain_event` conversion |
| `#[derive(AggregateRoot)]` | `domain.rs` | `AGGREGATE_TYPE` + `aggregate()` / `aggregate_mut()` |
| `#[derive(Service)]` / `#[derive(Repository)]` | `commands.rs`, `ledger.rs` | a scanned `@Component` / `@Repository` bean with `#[autowired]` fields |
| `#[event_listener(topic = "…")]` (method marker) | `ledger.rs` | mark an EDA listener method inside a `#[handlers]` impl (the projection bean) |
| `#[derive(Configuration)]` + `#[bean]` | `web.rs` | a `@Configuration` holder whose `#[bean]` factories declare infra beans |
| `#[derive(Controller)]` + `#[rest_controller]` + `#[get/post]` | `web.rs` | an autowired controller bean and its `WalletApi::routes(state) -> axum::Router` |
| `#[scheduled(fixed_rate = "…")]` | `housekeeping.rs` | a `schedule_<fn>(scheduler)` helper plus a drained registration |
| `#[firefly::saga]` + `#[saga_step]` | `transfer.rs` | `TransferSaga::run` / `::saga()` — a step graph with compensation |
| `#[firefly::workflow]` + `#[workflow_step]` | `compliance.rs` | a workflow `run` over the DAG of steps |
| `#[firefly::tcc]` + `#[participant]` | `tcc_transfer.rs` | a TCC `run` driving each participant's try / confirm / cancel |

The next steps read each of these in its Lumen file, in the order the crate
itself is layered. After that, [Step 10](#step-10--the-rest-of-the-declarative-set-not-used-by-lumen)
catalogues the macros Lumen does *not* exercise — because it is event-sourced and
handles its cross-cutting concerns by other means — each with a correct
standalone example.

> **Tip** **Checkpoint.** Keep this table open in a second pane. As you read each
> step, find the row it corresponds to and confirm the "Generates" column matches
> the explanation. The table is the skeleton; the steps are the muscle.

## Step 3 — CQRS messages and their handler bean (`commands.rs`)

> **Note** **Key term — CQRS.** *Command/Query Responsibility Segregation* is a
> pattern that routes state-changing **commands** and read-only **queries**
> through separate handlers on a shared *bus*. A command mutates; a query reads;
> they never share a handler. The Spring analog is a command/query gateway over
> annotated `@CommandHandler` / `@QueryHandler` methods.

`#[derive(Command)]` and `#[derive(Query)]` generate the `Message` impl that lets
the bus route a struct. `#[firefly(validate)]` on a field makes an empty or zero
value fail validation *before* the handler runs; `#[firefly(cache_ttl = "…")]` on
a query feeds the read cache. Here is the verbatim Lumen declaration, including
the `#[derive(Builder)]` and `#[derive(Schema)]` it also carries:

```rust,ignore
// samples/lumen/src/commands.rs
#[derive(Debug, Clone, Default, Serialize, Deserialize, Command, Builder, Schema)]
#[serde(default)]
pub struct OpenWallet {
    #[firefly(validate)]
    #[builder(into)]                 // accept &str, String, …
    pub owner: String,
    #[serde(rename = "openingBalance")]
    #[builder(default)]              // unset → 0
    pub opening_balance: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    pub id: String,
}
```

What just happened, derive by derive:

- `Command` / `Query` emit the `firefly::cqrs::Message` impl — the trait the bus
  dispatches on. `#[firefly(validate)]` registers `owner` as a required field, so
  `OpenWallet::default().validate()` is an `Err`. `#[firefly(cache_ttl = "30s")]`
  is read by the query cache through the generated `Message::cache_ttl`.
- `Schema` emits an OpenAPI schema for the type, so `OpenWallet` shows up in the
  spec served at `/v3/api-docs` on the management port — no hand-written schema.
- `Builder` (the Lombok `@Builder` analog) is covered in
  [Step 10](#construction--the-fluent-builder-derivebuilder); ignore it for now.

> **Note** **Key term — handler bean.** A *handler bean* is a DI component whose
> methods are the command/query handlers. Its collaborators are `#[autowired]`
> from the container, so each handler reaches them through `self` — there is no
> process-global and no composition root. The Spring analog is a `@Component`
> whose `@CommandHandler` / `@QueryHandler` methods are scanned and registered.

In Lumen the handlers live on such a bean — `WalletHandlers`, a
`#[derive(Service)]` whose write-side `Ledger` and read-side `ReadModel` are
`#[autowired]` — and `#[handlers]` registers each method on the bus. This is
verbatim Lumen:

```rust,ignore
// samples/lumen/src/commands.rs
#[derive(Service)]
struct WalletHandlers {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletHandlers {
    #[command_handler]
    async fn open_wallet(&self, cmd: OpenWallet) -> Result<WalletView, CqrsError> {
        if cmd.opening_balance < 0 {
            return Err(CqrsError::validation("openingBalance must be >= 0"));
        }
        self.ledger
            .open(&cmd.owner, Money::cents(cmd.opening_balance))
            .await
            .map_err(to_cqrs)
    }

    #[query_handler]
    async fn get_wallet(&self, q: GetWallet) -> Result<WalletView, CqrsError> {
        if let Some(view) = self.read_model.find(&q.id) {
            return Ok(view);
        }
        let events = self.ledger.load_events(&q.id).await.map_err(to_cqrs)?;
        Ok(Wallet::rehydrate(&q.id, &events).view())
    }
    // … deposit / withdraw
}
```

What just happened: `#[handlers]` is an **impl-level** attribute (like
`#[rest_controller]`) applied to the `impl` block of a registered bean. Each
method marked `#[command_handler]` / `#[query_handler]` takes `&self` plus one
message argument and returns `Result<R, CqrsError>`. For every marker the macro
submits a `BeanHandlerRegistration` into a compile-time inventory registry that,
at boot, resolves the bean from the container and installs a closure capturing
it. `FireflyApplication` drains those registrations during
`register_discovered_handlers`. So Lumen installs all four handlers by *declaring*
the bean and its methods — there is no hand-written `register(&bus)` call and no
`OnceLock` publishing wiring state.

Why it matters: the handler reaches `self.ledger` and `self.read_model` through
the container's injection, so the same handler that an HTTP test drives is the
same handler the live bus dispatches to — one wiring, exercised two ways.

> **Note** `#[command_handler]` / `#[query_handler]` also work on a **free**
> `async fn(Msg) -> Result<R, CqrsError>`, in which case the macro generates a
> `register_<fn>(bus)` helper for a simple, collaborator-free handler — the form
> the `macro-quickstart` sample uses. `#[handlers]` is the **bean** form for a
> handler that autowires collaborators, which is Lumen's actual wiring. Same
> markers, two shapes; Lumen uses the bean shape.

> **Tip** **Checkpoint.** Find `get_wallet_carries_cache_ttl` in
> `samples/lumen/src/commands.rs`. It asserts `GetWallet::default().cache_ttl()`
> is `Some(_)` — direct proof that `#[firefly(cache_ttl = "30s")]` reached the
> generated `Message::cache_ttl`. Run `cargo test -p firefly-sample-lumen
> get_wallet_carries_cache_ttl` and watch it pass.

## Step 4 — Domain events and the aggregate (`domain.rs`)

> **Note** **Key term — event sourcing.** In *event sourcing* an aggregate's
> state is not stored as a row; it is the fold of an ordered stream of immutable
> **domain events**. To load a wallet you replay its events; to change it you
> append a new event. Each event needs a stable identity so a persisted stream
> stays readable as the schema evolves. The Spring analog is an Axon
> `@EventSourcingHandler` aggregate.

`#[derive(DomainEvent)]` stamps each payload struct with a stable `EVENT_TYPE`
discriminator (its struct name) and a `to_domain_event` conversion onto the
framework wire event. `#[derive(AggregateRoot)]` finds the embedded
`AggregateRoot` field and generates `Wallet::AGGREGATE_TYPE` plus the `aggregate()`
/ `aggregate_mut()` accessors:

```rust,ignore
// samples/lumen/src/domain.rs
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

What just happened: the only event-sourcing wiring Lumen writes by hand is the
`apply` fold that projects one event into in-memory state. The discriminators
(`WalletOpened::EVENT_TYPE`, used when the aggregate `raise`s an event) and the
wire conversion are generated. The `#[firefly(aggregate_type = "Wallet")]`
argument pins the aggregate's type string, which the generated
`Wallet::AGGREGATE_TYPE` const exposes and the event store stamps on every
persisted event.

Why it matters: the discriminator gives each event a stable, versioned JSON
identity, so a stream persisted today stays decodable after the payload struct
grows new fields tomorrow — the property event sourcing depends on.

> **Tip** **Checkpoint.** In `domain.rs`, `rehydrate_folds_the_full_stream`
> asserts `Wallet::AGGREGATE_TYPE == "Wallet"` and folds an
> open + deposit + withdraw stream back to the right balance and version. That
> single test exercises both derives at once.

## Step 5 — The projection listener (`ledger.rs`)

> **Note** **Key term — projection.** A *projection* is a read-model builder: it
> consumes published domain events and writes a query-optimised view. Because it
> rebuilds the view from the event stream (rather than mutating a row from a
> single delivery), it is **idempotent** — an at-least-once redelivery converges
> on the same view. The Spring analog is a `@Component @EventListener` that
> updates a read table.

First, the read model itself is a bean. Lumen's `ReadModel` is a
`#[derive(Repository)]` data-access component (Spring's `@Repository`) — an
in-memory map of wallet id to `WalletView`, kept dependency-free for the teaching
baseline:

```rust,ignore
// samples/lumen/src/ledger.rs
#[derive(Debug, Default, Repository)]
pub struct ReadModel {
    rows: Mutex<HashMap<String, WalletView>>,
}
```

`container.scan()` registers it as a singleton bean, so it can be autowired (as
`Arc<ReadModel>`) into the handler and projection beans. A production service
would back this with `firefly`'s reactive repository over Postgres; the in-memory
map keeps the baseline infrastructure-free.

The projection is an **EDA listener bean** — `WalletProjection`, a
`#[derive(Service)]` that `#[autowired]`s the `Ledger` (for the event store it
replays) and the `ReadModel` it feeds. Inside a `#[handlers]` impl, an
`#[event_listener(topic = "…")]` method marks the projection — exactly like the
CQRS bean above, but the marker subscribes the method to an EDA topic rather than
the bus:

```rust,ignore
// samples/lumen/src/ledger.rs
#[derive(Service)]
struct WalletProjection {
    #[autowired]
    ledger: Arc<Ledger>,
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletProjection {
    #[event_listener(topic = "wallets.events")]
    async fn project(&self, ev: Event) -> FireflyResult<()> {
        let Some(wallet_id) = ev.headers.get("aggregateId") else {
            return Ok(());
        };
        // reload the wallet's stream, fold to a WalletView, upsert — idempotent.
        if let Ok(events) = self.ledger.store().load(wallet_id).await {
            let view = Wallet::rehydrate(wallet_id, &events).view();
            self.read_model.upsert(view);
        }
        Ok(())
    }
}
```

What just happened: `#[handlers]` submits a `BeanListenerRegistration` into
inventory that, at boot, resolves the bean from the container and subscribes its
method to `wallets.events` on the very broker the ledger publishes to.
`FireflyApplication` drains it during `subscribe_discovered_listeners`. The
subscription that closes the CQRS loop — write side appends and publishes, read
side projects — is therefore wired entirely through the DI container, with no
`subscribe(&broker)` call in any composition root.

> **Note** Like the CQRS markers, `#[event_listener(topic = "…")]` also works on
> a **free** `async fn(Event) -> FireflyResult<()>`, generating a
> `subscribe_<fn>(broker)` helper for a simple, collaborator-free listener.
> `#[handlers]` is the **bean** form for a projection that autowires collaborators
> — Lumen's actual wiring.

> **Tip** **Checkpoint.** The HTTP test `open_then_get_round_trips_through_cqrs`
> (in `http_test.rs`) opens a wallet over `POST /api/v1/wallets`, then reads it
> over `GET /api/v1/wallets/:id` and sees the projected balance — proof the
> listener bean subscribed and the loop closed. It boots the full
> `FireflyApplication`, so it exercises the inventory drain end to end.

## Step 6 — The controller (`web.rs`)

> **Note** **Key term — REST controller.** A *REST controller* is a bean whose
> methods map HTTP verbs and paths to handler functions. In Firefly the handler
> bodies are ordinary `axum` handlers; the macro generates the router and
> mounts it. The Spring analog is `@RestController` with `@GetMapping` /
> `@PostMapping`.

`#[rest_controller(path = "…")]` turns an `impl` block into a generated
`WalletApi::routes(state) -> axum::Router`. The controller type itself is a
`#[derive(Controller)]` bean whose collaborators are `#[autowired]`:

```rust,ignore
// samples/lumen/src/web.rs
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,
    #[autowired]
    pub ledger: Arc<Ledger>,
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

Each method carries one verb mapping, uses ordinary axum extractors, and returns
`WebResult<T>` so a handler error renders as RFC 9457
`application/problem+json`. The verb attributes also carry the OpenAPI metadata
(`summary`, `description`, `status`, `tags`) the docs generator reads:

```rust,ignore
// samples/lumen/src/web.rs
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletApi {
    #[post(
        "/wallets",
        summary = "Open a wallet",
        description = "Opens a new wallet for an owner with an optional opening balance.",
        status = 201
    )]
    async fn open(
        State(api): State<WalletApi>,
        Json(body): Json<OpenWallet>,
    ) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
        let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
        Ok((axum::http::StatusCode::CREATED, Json(view)))
    }

    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }
    // … deposit / withdraw / transfer / compliance / 2pc
}
// generated: WalletApi::routes(state) -> axum::Router
```

What just happened: the macro emits `WalletApi::routes(state)` **and** submits a
`ControllerMount` plus a per-route descriptor into link-time tables. So
`FireflyApplication` **auto-mounts** the controller (resolving its autowired
state from the container through `firefly::web::mount_controllers`), and the
OpenAPI generator and the actuator `/mappings` endpoint can enumerate Lumen's
routes without re-parsing the source. Lumen never hands `WalletApi::routes(state)`
to the web stack — declaring the controller bean is the entire wiring.

Why it matters: `WebResult<T>` renders any handler error as an RFC 9457 problem
body uniformly, so error shaping is the same across every endpoint with no
per-handler code, and the `:id` path parameter is the ordinary axum `Path`
extractor — Firefly does not invent its own routing.

> **Tip** **Checkpoint.** Run Lumen (`cargo run -p firefly-sample-lumen`) and
> open `http://localhost:8081/swagger-ui` on the **management** port. The
> "Open a wallet" summary, the `201` response, and the `Wallets` tag all come
> from the verb attributes above — no separate spec file.

## Step 7 — The scheduled heartbeat (`housekeeping.rs`)

> **Note** **Key term — scheduled task.** A *scheduled task* is a zero-argument
> `async fn` the framework runs on a cadence — a fixed rate, a fixed delay, or a
> cron expression. The Spring analog is `@Scheduled`.

`#[scheduled(...)]` generates a `schedule_<fn>(scheduler)` helper that registers
the function on a `Scheduler`, and also submits a `ScheduledRegistration` the
framework drains:

```rust,ignore
// samples/lumen/src/housekeeping.rs
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn ledger_heartbeat() -> Result<(), std::io::Error> {
    HEARTBEAT_TICKS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
// generated: schedule_ledger_heartbeat(&scheduler)
```

What just happened: `#[scheduled]` emits the `schedule_<fn>(scheduler)` helper
*and* the registration. At boot the framework calls
`register_discovered_scheduled(&scheduler)`, which drains the inventory and
installs every `#[scheduled]` task — so Lumen never calls `schedule_<fn>` by
hand. Use `fixed_rate = "60s"` for a fixed cadence (with an optional
`initial_delay`), or `cron = "…"` for a cron expression.

> **Tip** **Checkpoint.** `scheduled_task_registers` (in `housekeeping.rs`)
> builds a fresh scheduler, calls `register_discovered_scheduled`, and asserts
> `scheduler.tasks()` contains `"ledger_heartbeat"` — proof the registration was
> drained from inventory with no manual `schedule_<fn>` call.

## Step 8 — The orchestration trio (`transfer.rs`, `compliance.rs`, `tcc_transfer.rs`)

Three declarative orchestration macros round out Lumen's own source. Each turns
an annotated `impl` block into a runnable coordinator. We meet the key terms
first, then read one declaration each.

> **Note** **Key term — saga.** A *saga* is a distributed transaction made of
> steps, each with a **compensation** that undoes it if a later step fails. There
> is no shared lock; consistency is restored by running compensations in reverse.
> The Spring/Axon analog is a `@Saga`.

A transfer is *not* a single atomic command: it debits the source, then credits
the destination, and if the credit fails the debit must be refunded. That is the
saga pattern, declared as annotated methods on `TransferSaga`:

```rust,ignore
// samples/lumen/src/transfer.rs
#[firefly::saga(name = "money-transfer")]
impl TransferSaga {
    #[saga_step(id = "debit", compensate = "refund_debit")]
    async fn debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.withdraw(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    async fn refund_debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    #[saga_step(id = "credit", depends_on = ["debit"])]
    async fn credit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
// generated: TransferSaga::run(req) and TransferSaga::saga()
```

What just happened: `#[firefly::saga]` lowers these methods onto the
`firefly-orchestration` `Saga` engine. `depends_on` orders the steps,
`compensate` names the rollback method, and each parameter is injected from the
saga context — here the request, via `#[input]`. The macro generates
`TransferSaga::run`, which `run_transfer` calls. When the credit leg fails, the
engine runs `refund_debit`, so the source stream shows a real debit *and* its
compensating refund.

> **Note** **Key term — workflow.** A *workflow* is a directed acyclic graph
> (DAG) of steps: independent steps run in parallel; a step with `depends_on`
> runs only after its prerequisites and reads their results via `#[from_step]`.
> Where a saga is a linear chain with compensation, a workflow is a parallel
> fan-in. The Spring analog is a DAG-based process such as Spring Cloud Data
> Flow's task graph.

Lumen's compliance check runs `balance-check` and `limit-check` in parallel, then
`approve` after both:

```rust,ignore
// samples/lumen/src/compliance.rs
#[firefly::workflow(name = "transfer-compliance")]
impl ComplianceCheck {
    #[workflow_step(id = "balance-check")]
    async fn balance_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> { /* … */ }

    #[workflow_step(id = "limit-check")]
    async fn limit_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> { /* … */ }

    #[workflow_step(id = "approve", depends_on = ["balance-check", "limit-check"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] funds_ok: bool,
        #[from_step("limit-check")] within_limit: bool,
    ) -> Result<(), ComplianceError> { /* … */ }
}
// generated: ComplianceCheck::run(req)
```

> **Note** **Key term — TCC (Try / Confirm / Cancel).** *TCC* is a two-phase
> distributed transaction: every participant first **reserves** (try); only when
> all reservations succeed does the coordinator **confirm** them; otherwise it
> **cancels** the ones already tried. Where a saga undoes a committed leg, TCC
> reserves first and commits last. The Spring/Seata analog is the TCC
> transaction mode.

Lumen's two-phase transfer holds the source, verifies the destination, then
captures on both sides:

```rust,ignore
// samples/lumen/src/tcc_transfer.rs
#[firefly::tcc(name = "transfer-2pc")]
impl TwoPhaseTransfer {
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* withdraw (hold) */ }
    async fn capture_source(&self) -> Result<(), DomainError> { Ok(()) }           // the debit was the capture
    async fn release_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* deposit (release) */ }

    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* verify exists */ }
    async fn capture_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> { /* deposit (capture) */ }
}
// generated: TwoPhaseTransfer::run(req)
```

What just happened across all three: each macro reads its annotated methods and
generates a `run` method over the orchestration engine, wiring the step/participant
graph, the parameter injection (`#[input]` / `#[from_step]`), and the
compensation or cancel path. You write the bodies; the macro writes the
coordinator.

> **Tip** **Checkpoint.** The HTTP tests
> `transfer_saga_overdraft_compensates_and_is_422`,
> `compliance_workflow_rejects_overdraft_with_422`, and
> `tcc_transfer_overdraft_releases_the_hold_and_is_422` (in `http_test.rs`)
> exercise the failure path of each macro end to end. Run `cargo test
> -p firefly-sample-lumen overdraft` and watch all three pass.

## Step 9 — The configuration holder and the streaming contributor (`web.rs`)

Lumen *does* use the DI container directly. `web.rs` carries a
`#[derive(Configuration)]` holder whose `#[bean]` factory methods **declare** the
infrastructure beans:

> **Note** **Key term — configuration holder and bean factory.** A *configuration
> holder* is a `#[derive(Configuration)]` type whose `#[bean]` methods are
> *factories*: each returns a constructed value the container registers as a bean
> and can autowire elsewhere. The Spring analog is a `@Configuration` class whose
> `@Bean` methods produce beans.

```rust,ignore
// samples/lumen/src/web.rs
#[derive(Configuration)]
struct LumenBeans;

#[bean]
impl LumenBeans {
    #[bean]
    fn event_store(&self) -> MemoryEventStore { MemoryEventStore::new() }

    #[bean]
    fn query_cache(&self) -> QueryCache { QueryCache::new() }

    #[bean]
    fn jwt_service(&self) -> JwtService { JwtService::new(crate::security::DEMO_SIGNING_KEY) }

    #[bean]
    fn security_filter_chain(&self) -> FilterChain { crate::security::security_layers().1 }

    #[bean]
    fn bearer_layer(&self) -> BearerLayer { crate::security::security_layers().0 }

    #[bean]
    fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
        let store: Arc<dyn EventStore> = store;
        Ledger::new(store, broker)
    }
}
```

What just happened: `container.scan()` discovers and registers every `#[bean]`
method, so `build_app` calls no `register_arc` — the **framework** does the
registration. The `ledger` factory even *autowires its own arguments* (`store`
and the framework-provided `Broker` port), so a bean factory is itself a wiring
point. The `security_filter_chain` and `bearer_layer` beans are auto-discovered
and layered onto the API with no `.security(...)` call — the Spring
`SecurityFilterChain` pattern. The full DI mechanics are in the
[Dependency Injection deep-dive](./04a-dependency-injection.md).

The optional streaming endpoint shows one more declarative seam — a
`RouteContributor` bean:

> **Note** **Key term — route contributor.** A *route contributor* is a bean that
> hands the framework an extra `axum::Router` to merge into the public API. It is
> how you add routes that do not fit the `#[rest_controller]` shape (here, a
> feature-gated reactive stream) by *declaring a bean* rather than touching a
> composition root.

```rust,ignore
// samples/lumen/src/web.rs  (feature `streaming`)
#[cfg(feature = "streaming")]
#[derive(Service)]
#[firefly(provides = "dyn firefly::web::RouteContributor")]
struct StreamingRoutes {
    #[autowired]
    api: Arc<WalletApi>,
}

#[cfg(feature = "streaming")]
impl firefly::web::RouteContributor for StreamingRoutes {
    fn routes(&self) -> axum::Router {
        streaming_router((*self.api).clone())
    }
}
```

What just happened: `#[firefly(provides = "dyn firefly::web::RouteContributor")]`
tells the container to register this `#[derive(Service)]` under the
`RouteContributor` port. The framework discovers it and merges its routes — a
feature-gated `GET /api/v1/wallets/:id/events` endpoint wired by declaring a
bean, not by a composition root.

> **Tip** **Checkpoint.** Open `samples/lumen/src/web.rs` and confirm
> `build_router()` (the test seam) is just
> `FireflyApplication::new(APP_NAME).version(VERSION).bootstrap().await…
> .api_router` — no hand-written builder. Every bean in this step is
> auto-registered by `container.scan()`; the controller is auto-mounted; security
> and the read-cache middleware are auto-discovered.

## Step 10 — The rest of the declarative set (not used by Lumen)

Several more macros are first-class parts of the framework that Lumen does **not**
exercise in its own source — it is event-sourced (so the relational
`#[firefly::repository]` / `#[firefly::transactional]` never appear) and handles
the remaining cross-cutting concerns by other means. Each is shown here as a
focused, correct standalone example so the catalogue is complete.

| Macro | Purpose | Generates |
|-------|---------|-----------|
| `#[derive(Builder)]` | a fluent constructor with required/defaulted fields | `T::builder()` → fluent setters → `build() -> Result<T, String>` |
| `#[derive(Mapper)]` | compile-time struct-to-struct conversion | one `From<Source>` per `#[firefly(from = "…")]` |
| `#[derive(Entity)]` | the `@Entity` mapping from annotated struct fields | a `SqlxEntity` impl (`@Table` / `@Id` / `@Version` / `@Column`) |
| `#[derive(SqlxRepository)]` | a fully-wired sqlx `@Repository` bean | `ReactiveCrudRepository` **and** `ReactiveSpecificationRepository` impls plus the `repository()` accessor |
| `#[firefly::repository]` | derived-query and custom-query method bodies | method bodies on a `SqlxReactiveRepository` impl from method names or `#[query(…)]` |
| `#[firefly::transactional]` | a declared transaction boundary | a commit-on-`Ok` / rollback-on-`Err` boundary around an `async fn` |
| `#[firefly::pre_authorize]` / `#[firefly::post_authorize]` | method-level access control | an access check before the body, or a returnObject check after it |
| `#[derive(Validate)]` (+ `Valid<T>`) | JSR-380 bean validation | an `impl Validate`; the `Valid<T>` extractor rejects a constraint failure with 422 |
| `#[cacheable]` / `#[cache_put]` / `#[cache_evict]` | declarative caching | a read-through / write-through / evict body around the registered cache adapter |
| `#[retry]` / `#[circuit_breaker]` / `#[rate_limit]` / `#[bulkhead]` / `#[timeout]` | resilience decorators | the body wrapped in the matching `firefly_resilience` primitive |
| `#[async_method]` | fire-and-forget async | an `async fn(self: Arc<Self>, …) -> R` rewritten to a non-async `fn … -> TaskHandle<R>` |
| `#[application_event_listener]` / `#[transactional_event_listener]` | in-process events | an `@EventListener` / `@TransactionalEventListener` discovered via inventory |
| `#[aspect]` (+ `#[before]`/`#[after]`/`#[around]`) | aspect-oriented advice | `impl firefly_aop::Aspect` + an inventory registration |

The remaining DI stereotype derives round out the set:
`#[derive(Component/Service/Repository/Configuration/AutoConfiguration/Controller)]`,
`#[bean]`, `#[autowired]`, `register_all!`, and `#[derive(ConfigProperties)]`.
`#[derive(AutoConfiguration)]` is the auto-config holder whose `#[bean]`s back off
behind a `condition_on_missing_bean`, so an application can override any default by
declaring its own bean of the same type; `Container::scan()` auto-registers every
`#[bean]` method, and `Container::scan_packages([..])` restricts discovery to
named module paths.

### Construction — the fluent builder (`#[derive(Builder)]`)

Rust's stdlib derives already cover value-object boilerplate — `Debug`, `Clone`,
`PartialEq`, `Default`. The one ergonomic gap they leave is a *fluent builder*,
and that is what `#[derive(Builder)]` (Lombok's `@Builder`) fills. It generates
`T::builder()` returning a `TBuilder` with one setter per field and a
`build() -> Result<T, String>`. By default every field is **required**: `build`
returns an `Err` naming the first unset field. `#[builder(default)]` falls back to
`Default::default()`, `#[builder(default = "expr")]` to a custom expression, and
`#[builder(into)]` makes the setter accept `impl Into<FieldTy>`. Lumen's
`OpenWallet` (from [Step 3](#step-3--cqrs-messages-and-their-handler-bean-commandsrs))
carries it:

```rust,ignore
let cmd = OpenWallet::builder()
    .owner("ada")            // impl Into<String>
    .opening_balance(10_000)
    .build()?;               // Result<OpenWallet, String>
```

Returning a `Result` keeps missing-field handling on the normal `?` path rather
than a panic. Reach for `#[derive(Builder)]` when a struct has many
optional/defaulted fields; keep a plain literal when every field is required and
present.

### Conversion — the compile-time mapper (`#[derive(Mapper)]`)

`#[derive(Mapper)]` generates a compile-time, type-checked `From<Source>` that
maps a source struct to a target field-by-field. One `#[firefly(from = "Source")]`
produces one `From` impl, and the attribute is **repeatable** to map from several
sources. Per-field attributes adjust the mapping: `#[firefly(rename = "src")]`
reads a differently named source field, `#[firefly(into)]` applies `.into()`,
`#[firefly(with = "fn")]` runs a conversion function, and `#[firefly(default)]` /
`#[firefly(default_expr = "expr")]` fill a target field with no source read:

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize, Mapper)]
#[firefly(from = "Wallet")]
pub struct WalletView {
    #[firefly(rename = "root", with = "aggregate_id")]  // read src.root, run aggregate_id(..)
    pub id: String,
    pub owner: String,                        // same name on both ends: a plain move
    #[firefly(with = "Money::cents_value")]   // src.balance: Money -> i64 via a fn
    pub balance: i64,
    #[firefly(default)]                       // version set by the projector, not the fold
    pub version: i64,
}
// generates: impl From<Wallet> for WalletView { fn from(src: Wallet) -> Self { … } }
```

Because the generated code is a plain `From` impl, every field is checked by the
compiler with no runtime cost — that compile-time guarantee is the whole point.
Contrast it with the **runtime** `firefly_data::Mapper`, which converts via two
serde passes: use the runtime mapper when the source type is not known until
runtime (mapping arbitrary JSON), and prefer `#[derive(Mapper)]` whenever both
ends are concrete types.

> **Note** Lumen's real `WalletView` is built by a hand-written `Wallet::view`
> method (in `domain.rs`) rather than `#[derive(Mapper)]`; the listing above is
> the equivalent declarative form, shown to illustrate the macro.

### Persistence — entities, repositories, and transactions (relational)

These macros sit on the relational persistence path. Lumen's read model is an
in-memory projection over an event stream, so it uses none of them — but in a
relational service they are the everyday tools. The full reference is the
[Persistence chapter](./07-persistence.md).

`#[derive(Entity)]` generates the `SqlxEntity` mapping (`@Table` / `@Id` /
`@Version` / `@Column`) from annotated fields. Scalar fields map automatically; a
non-scalar field uses `#[firefly(with(read = "path", write = "path"))]`:

```rust,ignore
#[derive(Debug, Clone, Entity)]
#[firefly(table = "accounts")]
pub struct Account {
    #[firefly(id)]
    pub id: String,
    pub owner: String,
    pub status: String,
    #[firefly(version)]
    pub version: i64,
}
```

`#[derive(SqlxRepository)]` builds a fully-wired `@Repository` bean from the
injected `Db` datasource (via `repository_for`). It implements both
`ReactiveCrudRepository` (the `save` / `find_by_id` / `delete_by_id` / `count`
surface) **and** `ReactiveSpecificationRepository` (`find_by_spec`, the
`JpaSpecificationExecutor` analog) by delegation, and exposes the `repository()`
accessor that `#[firefly::repository]` builds on:

```rust,ignore
#[derive(SqlxRepository)]
#[firefly(entity = "Account")]
pub struct AccountRepo {
    db: Arc<Db>,
}
```

`#[firefly::repository]` turns a `find_by_…` / `count_by_…` / `exists_by_…` /
`delete_by_…` method name into a working query body. The runtime method is chosen
from the **return type** (`Vec<T>` / `Option<T>` → find, `i64` → count, `bool` →
exists, `u64` → delete); the placeholder `unimplemented!()` bodies are discarded:

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }
    async fn find_by_iban(&self, iban: &str)     -> Result<Option<Account>, DataError> { unimplemented!() }
    async fn count_by_owner(&self, owner: &str)  -> Result<i64, DataError>          { unimplemented!() }
    async fn exists_by_email(&self, email: &str) -> Result<bool, DataError>         { unimplemented!() }
}
```

Give a `find_by_…` method a trailing `Pageable` argument (and a
`Result<Vec<T>, DataError>` return) and the generated body appends the page's sort
and window, delegating to `find_by_derived_paged`. Note that `Pageable::of`
returns a `Result`:

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    async fn find_by_owner(&self, owner: &str, page: Pageable)
        -> Result<Vec<Account>, DataError> { unimplemented!() }
}

// Build the page (1-based index) with sort + window — `of` returns a Result:
let page = Pageable::of(1, 20, RequestSort::of([Order::desc("id")])).unwrap();
let rows = repo.find_by_owner("ada", page).await?;
```

When a name-derived query is not enough, annotate a stub with `#[query(...)]` and
write the statement directly. Native SQL binds each `:name` placeholder to the
argument named `name`; the **return type** still selects the operation —
`Vec<T>` / `Option<T>` is a list, `i64` a count, `bool` an existence check, and
`u64` a modifying statement (returning affected rows):

```rust,ignore
#[firefly::repository]
impl AccountRepo {
    #[query("SELECT id, owner FROM accounts WHERE status = :status ORDER BY id DESC")]
    async fn active_by_status(&self, status: &str) -> Result<Vec<Account>, DataError> { unimplemented!() }

    #[query("UPDATE accounts SET status = :status WHERE id = :id")]
    async fn set_status(&self, id: &str, status: &str) -> Result<u64, DataError> { unimplemented!() }
}
```

`#[query(sql = "…")]` is the explicit spelling of the native form, and
`#[query(jpql = "…", entity = "Account")]` writes the statement against entity
names.

> **Note** **Key term — transaction boundary.** A *transaction boundary* is a
> region of code whose database work commits together or rolls back together.
> `#[firefly::transactional]` makes that boundary a declaration on an `async fn`.
> The Spring analog is `@Transactional`.

`#[firefly::transactional]` wraps an `async fn`'s body in a transaction governed
by the registered `TransactionManager` — commit on `Ok`, roll back on `Err`. The
function must be `async`, must return `Result<T, E>`, and its error type must
implement `From<firefly_transactional::TxError>` so begin/commit failures surface
through `?`. Bare, or with options:

```rust,ignore
#[firefly::transactional]
async fn open_account(repo: &AccountRepo, acct: Account) -> Result<(), DataError> {
    repo.insert(&acct).await?;        // committed together on Ok,
    repo.insert_audit(&acct).await?;  // rolled back together on Err
    Ok(())
}

#[firefly::transactional(propagation = "requires_new", isolation = "serializable", read_only = false, timeout_ms = 5000)]
async fn reconcile(repo: &LedgerRepo) -> Result<(), DataError> { /* … */ }
```

By default the boundary runs through the **process-global** registered
`TransactionManager`. `manager = "<expr>"` (Spring's
`@Transactional("txManager")`) instead binds it to an **explicit** manager the
service owns — the expression yields a value `m` with
`&m: &Arc<dyn TransactionManager>`. Use it for a multi-datasource service, or to
keep per-instance/per-test isolation. The `lumen-ledger` sample's `transfer` use
case is wired exactly this way:

```rust,ignore
// samples/lumen-ledger — core/src/services/wallet/v1/wallet_service_impl.rs
#[firefly::transactional(manager = "self.tx_manager()")]   // self owns the manager
async fn transfer_tx(&self, from: Uuid, to: Uuid, amount: i64) -> Result<WalletResponse, ServiceError> {
    let mut src = self.load_active(from).await?;            // debit + credit commit
    let mut dst = self.load_active(to).await?;              // together, or roll back
    src.balance -= amount; let saved = self.persist(src).await?;
    dst.balance += amount; self.persist(dst).await?;
    Ok(saved)
}
```

Two further options control which errors roll back, both deliberately *not* named
`rollback_for` (Spring's `rollbackFor` is a footgun because its
already-marked-rollback-only edge case surprises people):

- `no_rollback_for = "<pat>"` — Spring's `@Transactional(noRollbackFor = …)`: when
  the `Err` matches the pattern, the boundary **commits** instead of rolling back.
- `rollback_only_for = "<pat>"` — roll back **only** when the `Err` matches the
  pattern, committing on any other error. The pattern is a match-style pattern
  over the function's error type, alternatives allowed:
  `no_rollback_for = "Error::A | Error::B"`. With both, `no_rollback_for` wins on
  overlap.

```rust,ignore
#[firefly::transactional(no_rollback_for = "DataError::NotFound(_)")]
async fn upsert(repo: &AccountRepo, acct: Account) -> Result<(), DataError> { /* … */ }
```

These two relational macros are the counterpart to how Lumen achieves consistency
*without* a transaction manager: it appends events to the `EventStore` under
optimistic concurrency and projects them, rather than mutating rows inside a
`#[transactional]` boundary. Same goal — atomic, consistent writes — reached by
two different architectures.

### Method security — `#[pre_authorize]` / `#[post_authorize]`

Two macros enforce access control at the method boundary, reading the caller's
identity from the ambient security context rather than from a `Request`. The full
treatment is in the [Security chapter](./14-security.md).

`#[firefly::pre_authorize(...)]` runs an access check **before** the body. Apply
it to a `fn` returning `Result<T, E>` whose error implements
`From<firefly_security::SecurityError>`, so a denial travels the `?` path:

```rust,ignore
#[firefly::pre_authorize]                              // `authenticated` — any caller in scope
async fn whoami() -> Result<Profile, AppError> { /* … */ }

#[firefly::pre_authorize(role = "ADMIN")]              // a single role
async fn close_books(&self) -> Result<(), AppError> { /* … */ }

#[firefly::pre_authorize(any_role = ["TELLER", "ADMIN"])]
async fn open_account(&self, req: OpenAccount) -> Result<Account, AppError> { /* … */ }

#[firefly::pre_authorize(authority = "wallet:write")]  // a single fine-grained authority
async fn deposit(&self, id: &str, cents: i64) -> Result<(), AppError> { /* … */ }
```

When no caller is in scope the body is skipped and the macro returns
`Err(SecurityError::Unauthenticated.into())`; when a caller is present but lacks
the required role/authority it returns `Err(SecurityError::Forbidden.into())`.

`#[firefly::post_authorize(<bool expr>)]` runs **after** an `async fn` returns and
gates the value on a boolean expression that sees `result` (a `&T` to the returned
value) and `auth` (a `&Authentication`); if it is `false` the value is discarded
and the call returns `Forbidden`:

```rust,ignore
// Only return the wallet if the caller owns it.
#[firefly::post_authorize(result.owner == auth.subject())]
async fn get_wallet(&self, id: &str) -> Result<WalletView, AppError> { /* … */ }
```

Because `BearerLayer` scopes the authentication for the whole downstream call,
these checks work on a service method that never sees the `Request` — the macro
reads from scope, not from a handler argument.

### Validation — `#[derive(Validate)]` and `Valid<T>`

`#[derive(Validate)]` generates an `impl Validate` that runs each field's
`#[validate(email/url/not_empty/length/range/pattern/custom)]` constraint, and the
`Valid<T>` web extractor rejects a constraint failure with `422`:

```rust,ignore
#[derive(Debug, Deserialize, Validate)]
struct CreateUser {
    #[validate(not_empty, length(min = 2, max = 64))]
    name: String,
    #[validate(email)]
    email: String,
}

// In a controller, `Valid<CreateUser>` returns 422 if any constraint fails:
async fn create(Valid(body): Valid<CreateUser>) -> WebResult<Json<UserView>> { /* … */ }
```

### Caching, async, in-process events, and aspects

`#[cacheable]` / `#[cache_put]` / `#[cache_evict]` wrap a method body in a
read-through / write-through / evict path around the process-registered cache
adapter. `#[cacheable]` also takes `condition = "<bool expr>"` (bypass the cache
when the parameter expression is `false`) and `unless = "<bool expr>"` (do not
store when the result expression — bound as `result: &V` — is `true`):

```rust,ignore
#[cacheable(key = "format!(\"order:{}\", id)", unless = "result.is_empty()")]
async fn load_order(&self, id: &str) -> Result<Order, DataError> { /* … */ }
```

`#[async_method]` rewrites an `async fn(self: Arc<Self>, …) -> R` into a
non-async `fn … -> TaskHandle<R>` that spawns the body on the registered
executor — fire-and-forget, with a handle to await later.

`#[application_event_listener]` / `#[transactional_event_listener]` are the
in-process event listeners (Spring's `@EventListener` /
`@TransactionalEventListener`): each is discovered via inventory and fired by
`publish_event`, the transactional one bound to a commit phase.

`#[aspect]` (with `#[before]` / `#[after]` / `#[around]` advice) generates an
`impl firefly_aop::Aspect` plus an inventory registration; advice runs around the
explicit `advised(…)` weave point.

### Resilience decorators

Where the `firefly_resilience` primitives are the build-it-yourself surface
(`Retry::new().max_attempts(3).execute(op)`), five **decorator** macros put the
same guards on a method — the Resilience4j / Spring-Retry analogs:

```rust,ignore
#[firefly::retry(max_attempts = 4, delay = "100ms", backoff = 2.0, max_delay = "2s")]
async fn fetch_quote(&self) -> Result<Quote, IntegrationError> { /* … */ }

#[firefly::circuit_breaker(failure_threshold = 5, open_duration = "30s")]
async fn call_upstream(&self) -> Result<Reply, IntegrationError> { /* … */ }

#[firefly::rate_limit(rate = 100.0, burst = 20)]    // 100/s, bucket of 20
async fn search(&self, q: &str) -> Result<Hits, SearchError> { /* … */ }

#[firefly::bulkhead(20)]                              // ≤ 20 calls in flight
async fn render(&self, doc: &Doc) -> Result<Pdf, RenderError> { /* … */ }

#[firefly::timeout("2s")]
async fn slow_report(&self) -> Result<Report, ReportError> { /* … */ }
```

Apply them to an `async fn` returning `Result<T, E>` whose error implements
`std::error::Error + Send + Sync + 'static + From<firefly_resilience::ResilienceError>`.
The decorator threads the body's own failure through the primitive and recovers
the **original `E`** on the way out, while a guard's own short-circuit (a timeout,
an open circuit, a rejection) surfaces through `E::from(ResilienceError)`. The
attributes **stack**, outermost first:

```rust,ignore
#[firefly::retry(max_attempts = 3, delay = "50ms")]   // outer: re-runs the call
#[firefly::circuit_breaker(failure_threshold = 5)]    // inner: trips on a failing dep
async fn call_upstream(&self) -> Result<Reply, IntegrationError> { /* … */ }
```

The stateful guards (`#[circuit_breaker]`, `#[rate_limit]`, `#[bulkhead]`) keep
their state in a per-method `static`, shared across every call — the Resilience4j
registry-bean semantics; `#[retry]` and `#[timeout]` are stateless and rebuilt per
call. Durations accept a unit-suffixed string (`"100ms"`, `"2s"`, `"1m"`) or a
bare integer of milliseconds.

### The outbound HTTP client — `#[http_client]`

`#[http_client]` is the declarative HTTP-interface client (Spring's
`@HttpExchange`). Applied to a `trait`, it emits the trait verbatim **and** a
`<Trait>Impl` struct that wraps a `WebClient` and implements the trait by
translating each method's verb attribute and `:id`-style path into a call. The
awaited `async fn -> Result<T, ClientError>` shape decodes the body, surfaces a
404 as `ClientError::Problem`, and supports a custom error via
`E: From<ClientError>`; non-awaited `Mono<T>` / `Flux<T>` returns surface the raw
`ClientError` unchanged:

```rust,ignore
#[http_client(path = "/api/v1/orders")]
trait OrderClient {
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    #[post("/")]
    async fn create(&self, body: NewOrder) -> Result<Order, ClientError>;

    #[get("/opt/:id")]
    async fn find_opt(&self, id: String) -> Result<Option<Order>, ClientError>;
}
// generated: struct OrderClientImpl { … }  impl OrderClient for OrderClientImpl { … }
```

> **Tip** **Checkpoint.** You will not run any of Step 10's examples against
> Lumen — they are catalogue entries, not Lumen source. The litmus test is the
> next step: confirm the *Lumen* macros all compile and pass.

## Step 11 — How the wiring actually lands: the `__rt` contract and the inventory drain

You have now seen every macro Lumen uses. The last piece is *why declaring a bean
is the whole wiring*. Two mechanisms make it work.

First, the **`__rt` contract path** from [Step 1](#step-1--one-dependency-one-prelude).
A `proc-macro` crate cannot re-export runtime types, so macro-generated code names
every runtime type through `::firefly::__rt::firefly_cqrs::Bus` and friends. That
is the reason a one-crate service compiles whatever a macro expands to without
listing the underlying `firefly-*` crates.

Second, the **inventory drain**. The declarative layer does more than generate
helpers: each handler bean, listener bean, scheduled task, and controller also
submits a registration into a compile-time inventory registry, and
`FireflyApplication` drains those registries at boot. So Lumen calls *none* of the
wiring by hand:

- no `register(&bus)` — drained by `register_discovered_handlers`,
- no `subscribe(&broker)` — drained by `subscribe_discovered_listeners`,
- no `schedule_<fn>(scheduler)` — drained by `register_discovered_scheduled`,
- no `WalletApi::routes(state)` handed to the web stack — drained by
  `mount_controllers`,
- and no `OnceLock` publishing the handlers' collaborators — they autowire from
  the container.

Lumen declares the `WalletHandlers` / `WalletProjection` beans, the heartbeat
task, the `LumenBeans` factories, and the `WalletApi` controller, and the
framework resolves each bean from the container and installs it. The free-`fn`
form of `#[command_handler]` / `#[query_handler]` / `#[event_listener]` /
`#[scheduled]` still generates a `register_<fn>` / `subscribe_<fn>` /
`schedule_<fn>` helper for the collaborator-free case; but because Lumen's
handlers autowire collaborators, it uses the bean form, and the running service is
wired entirely by the inventory drain.

> **Tip** **Checkpoint.** Run Lumen and read the startup report. The
> `:: cqrs handlers: … | event listeners: … | scheduled tasks: … | controllers:
> … ::` line is the inventory the framework drained — the count is exactly the
> beans, listeners, tasks, and controllers you declared, with no registration
> call anywhere in the source.

## Step 12 — The whole crate, declaratively

Read top to bottom, the macros tell Lumen's story:

```text
  money.rs        (no macros — a pure value object; the no-thiserror promise)
  domain.rs       #[derive(DomainEvent)] x3   #[derive(AggregateRoot)]   #[derive(Schema)]
  ledger.rs       #[derive(Repository)] ReadModel   #[derive(Service)] WalletProjection
                  #[handlers] + #[event_listener(topic = "wallets.events")]
  commands.rs     #[derive(Command)] x3   #[derive(Query)]   #[derive(Builder/Schema)]
                  #[derive(Service)] WalletHandlers
                  #[handlers] + #[command_handler] x3 + #[query_handler]
  transfer.rs     #[firefly::saga] + #[saga_step] x2
  compliance.rs   #[firefly::workflow] + #[workflow_step] x3
  tcc_transfer.rs #[firefly::tcc] + #[participant] x2
  security.rs     (JwtService / BearerLayer / FilterChain — runtime APIs)
  web.rs          #[derive(Configuration)] + #[bean] x6   #[derive(Controller)]
                  #[rest_controller] + #[get] / #[post] x7
  housekeeping.rs #[scheduled(fixed_rate = "60s", initial_delay = "5s")]
```

What is *not* a macro is just as telling: the security filter chain is built with
a runtime builder (`FilterChain::new().require(...)`), because its shape is data,
not a fixed declaration — and Lumen keeps it explicit so the control flow stays
visible. The saga, workflow, and TCC *are* declarative macros; only the filter
chain remains a runtime builder. Declarative where it collapses boilerplate,
explicit where the graph is the point: that balance is the whole design.

## Step 13 — Verify the crate

Everything above compiles and is tested. From the workspace root:

```bash
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                       # 42 unit + 12 HTTP = 54 tests
cargo test   -p firefly-sample-lumen --features streaming  # 57 tests (+3 streaming)
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
```

The HTTP tests drive the framework-assembled router in-process: `build_router()`
bootstraps a `FireflyApplication` (auto-mounting the controller, draining the
handlers/listener, layering security) and returns its public router, exercised
through `tower::ServiceExt::oneshot` with no socket bound. They prove the
auto-mounted routes, the CQRS handlers, validation (422), the not-found path
(404), the auth boundary (401), the transfer saga (happy + compensation), the
compliance workflow, the TCC transfer, and the projection convergence all work end
to end — every prose listing in this book is a slice of that running crate.

> **Tip** **Checkpoint.** All three commands succeed: build is clean, the default
> test run reports `54 passed`, the streaming run reports `57 passed`, and clippy
> is silent under `-D warnings`. That is the whole declarative crate, verified.

## Recap

- A **declarative macro** in Firefly expands at compile time into the `impl`s,
  routers, and registrations you would otherwise hand-write — checked by the
  compiler, never discovered by runtime reflection.
- The macros Lumen uses, file by file: `#[derive(Command/Query/Schema)]` and
  `#[handlers]` (`commands.rs`); `#[derive(DomainEvent/AggregateRoot)]`
  (`domain.rs`); `#[derive(Repository/Service)]` + `#[event_listener]`
  (`ledger.rs`); `#[derive(Configuration)]` + `#[bean]` and
  `#[derive(Controller)]` + `#[rest_controller]` (`web.rs`); `#[scheduled]`
  (`housekeeping.rs`); and the orchestration trio `#[firefly::saga]` /
  `#[firefly::workflow]` / `#[firefly::tcc]`.
- The supporting set Lumen does not use is still first-class:
  `#[derive(Builder/Mapper/Validate)]`, the relational
  `#[derive(Entity/SqlxRepository)]` / `#[firefly::repository]` /
  `#[firefly::transactional]` (with `propagation` / `isolation` / `read_only` /
  `timeout_ms` / `manager`, plus `no_rollback_for` / `rollback_only_for`), the
  method-security and resilience decorators, `#[cacheable]`, `#[async_method]`,
  the in-process event listeners, `#[aspect]`, and `#[http_client]`.
- Macro-generated code names runtime types through the hidden `__rt` contract
  path, which is why a one-crate service compiles whatever a macro expands to.
- The **inventory drain** is what turns a declared bean, listener, task, or
  controller into wired behaviour at boot — so Lumen writes no `register`,
  `subscribe`, `schedule`, or `routes` call by hand.

This chapter added no feature; it re-read Lumen as a catalogue. Every macro
replaced a chunk of hand-written wiring with a declaration next to the code, and
all of it arrived through one dependency and one prelude glob — the thesis the
running crate proves.

## Exercises

1. **Trace one macro end to end.** Pick `#[derive(Query)]` on `GetWallet`. Find
   where its generated `cache_ttl()` is read (the `QueryCache` invalidation in
   `web.rs`) and the test that asserts it (`get_wallet_carries_cache_ttl` in
   `commands.rs`). Change the TTL to `"5s"` and re-run
   `cargo test -p firefly-sample-lumen get_wallet_carries_cache_ttl`.
2. **Add a verb.** Add a `#[get("/wallets/:id/balance")]`-style read method to the
   `#[rest_controller]` impl in `web.rs` (return the balance as JSON, dispatching
   `GetWallet` through the bus) and confirm the auto-mounted controller serves it
   with no other change — no `routes()` edit, no registration call.
3. **Add a scheduled task.** Write a second `#[scheduled(cron = "0 0 * * * *")]`
   function in `housekeeping.rs` and assert it appears in `scheduler.tasks()`
   alongside `ledger_heartbeat` — the framework drains the new
   `ScheduledRegistration` from inventory, so you add no registration call.
4. **Read the inventory in the startup report.** Run Lumen and find the
   `:: cqrs handlers … | event listeners … | scheduled tasks … | controllers … ::`
   line. Count each against the beans you read in this chapter, then add the
   verb from exercise 2 and watch the controller route count follow.
5. **Count the wiring you didn't write.** For each macro in
   [Step 2](#step-2--the-macro-catalogue-mapped-to-lumen-files)'s table, name the
   helper or impl it generated (`register_*`, `subscribe_*`, `schedule_*`,
   `routes`, `EVENT_TYPE`, `AGGREGATE_TYPE`, the `Message` impl). That list is the
   boilerplate the declarative layer wrote for you.

## Where to go next

- Compose Lumen's single crate into a multi-crate, layered service in
  **[Layered Microservices](./22-layered-microservices.md)** — where the
  `lumen-ledger` sample (with the `#[firefly::transactional]` use case from Step
  10) splits domain, core, web, and models into separate crates.
- Revisit how the framework scans and wires the beans this chapter declared in
  the **[Dependency Injection deep-dive](./04a-dependency-injection.md)**.
- The appendices are reference: a **[Module Index](./91-appendix-modules.md)** of
  every `firefly-*` crate and a **[Glossary](./92-glossary.md)** of the terms used
  throughout the book.
