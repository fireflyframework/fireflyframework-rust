# `firefly`

> **Tier:** Front door · **Status:** Full · **Role:** the one-dependency facade — prelude + re-exports of the whole framework + macros · **pyfly analog:** `import pyfly` / `from pyfly import …`

## Overview

`firefly` is the **Spring-Boot-starter developer experience** for the Firefly
Framework. Add a single dependency, glob-import a single prelude, and you have
the whole framework — CQRS, the dependency-injection container, the reactive
web stack, event-driven messaging, scheduling, saga/TCC/workflow orchestration,
resilience, security, observability, lifecycle, and the declarative macro layer
— all under stable, ergonomic paths.

```toml
[dependencies]
firefly = "26.6"
```

```rust,no_run
use firefly::prelude::*;

#[tokio::main]
async fn main() -> FireflyResult<()> {
    let core = Core::new(CoreConfig {
        app_name: "orders".into(),
        app_version: "1.0.0".into(),
        ..CoreConfig::default()
    });
    core.print_banner();
    Ok(())
}
```

Before this crate, a service had to list ten-to-fifteen individual `firefly-*`
crates in its `Cargo.toml` and import from each. The facade collapses that to
one dependency and one `use`.

## Three ways in

| You write | You get |
|-----------|---------|
| `use firefly::prelude::*;` | The high-frequency surface: `Bus`, `Message`, `CqrsError`, `Container`, `Scope`, `Scheduler`, `Saga`, `Step`, `Application`, `ShutdownHandle`, `Core`, `CoreConfig`, `WebResult`, `WebError`, `problem_response`, `FireflyError`, `FireflyResult`, `Mono`, `Flux`, and **every macro**. |
| `firefly::cqrs::…`, `firefly::web::…`, `firefly::container::…` | Ergonomic per-crate aliases — write `firefly::cqrs::Bus` instead of `firefly_cqrs::Bus`. One alias exists for every runtime crate. |
| `firefly::__rt::firefly_cqrs::…` | The hidden, **stable** contract path that macro-generated code targets. You never write this by hand. |

### The prelude

The prelude is tuned for the names you reach for in nearly every service:

```rust
use firefly::prelude::*;

let bus = Bus::new();
let container = Container::new();
let scheduler = Scheduler::new();
let _ok: FireflyResult<()> = Ok(());
```

### Dependency injection + `ApplicationContext`

The facade is the front door to the framework's **best-in-class
dependency-injection** experience (Spring/pyfly parity). Annotate beans with the
stereotype derives, then let the `ApplicationContext` scan, gate, wire, and
warm them:

```rust,ignore
use firefly::prelude::*;
use std::sync::Arc;

#[derive(Repository, Default)]
struct OrderRepo;

#[derive(Service)]
#[firefly(profile = "prod", post_construct = "warm")]
struct OrderService { #[autowired] repo: Arc<OrderRepo> }
impl OrderService { fn warm(&mut self) {} }

// Scans the crate graph via `inventory`, evaluates conditionals/profiles,
// eagerly warms non-lazy singletons (running #[post_construct]), and is
// resolvable. `close()` runs #[pre_destroy] in reverse order.
let ctx = ApplicationContext::builder().profiles(["prod"]).build();
let svc = ctx.resolve::<OrderService>()?;
ctx.close();
```

`firefly::scan(&container)` is the free-function form of `Container::scan()` for
when you manage the container yourself. The prelude exports `Container`,
`Scope`, `Provider`, `ConditionContext`, and `ApplicationContext`; see the
[`firefly-macros`](../macros) README for the full derive/attribute surface
(`#[bean]` factory methods, `#[derive(ConfigProperties)]`,
`#[firefly(value = …)]`, conditionals, interface auto-bind, lifecycle hooks).

### Module aliases

Every runtime crate is re-exported under a short alias, so the `firefly_`
prefix never appears in your code:

| Alias | Crate |
|-------|-------|
| `firefly::cqrs` | `firefly-cqrs` |
| `firefly::web` | `firefly-web` |
| `firefly::container` | `firefly-container` |
| `firefly::eda` | `firefly-eda` |
| `firefly::scheduling` | `firefly-scheduling` |
| `firefly::orchestration` | `firefly-orchestration` |
| `firefly::data` | `firefly-data` |
| `firefly::cache` | `firefly-cache` |
| `firefly::config` | `firefly-config` |
| `firefly::observability` | `firefly-observability` |
| `firefly::actuator` | `firefly-actuator` |
| `firefly::resilience` | `firefly-resilience` |
| `firefly::security` | `firefly-security` |
| `firefly::lifecycle` | `firefly-lifecycle` |
| `firefly::eventsourcing` | `firefly-eventsourcing` |
| `firefly::reactive` | `firefly-reactive` |
| `firefly::kernel` | `firefly-kernel` |
| `firefly::client` | `firefly-client` |
| `firefly::starter_core` | `firefly-starter-core` |
| `firefly::starter_web` | `firefly-starter-web` |

## Macros and the `__rt` contract

Every macro from `firefly-macros` is re-exported at the crate root **and** in
the prelude, so `#[command_handler]`, `#[scheduled]`, `#[derive(Component)]`,
`#[rest_controller]`, … are reachable as `firefly::command_handler` or via the
glob.

Macro-generated code references runtime types through a single hidden module —
`firefly::__rt`. Each runtime crate appears there under **exactly its crate
name** (the crate name with the `firefly_` prefix):

```rust,ignore
// What a macro expands to (you never write this):
::firefly::__rt::firefly_cqrs::Bus
::firefly::__rt::firefly_scheduling::Scheduler
```

This is the **contract** between `firefly-macros` and the runtime: those module
names are guaranteed stable so generated code compiles for any user who depends
only on `firefly`. You should never type `firefly::__rt::…` yourself — use the
prelude or the aliases.

## Staying lean — cargo features

The default build pulls in only the framework's **port** crates (no heavy
third-party drivers like sqlx, the MongoDB driver, the Kafka/RabbitMQ clients,
or the Redis/Postgres adapters). A minimal `firefly` dependency stays small and
compiles fast.

Heavy adapters are opt-in features:

```toml
firefly = { version = "26.6", features = ["data-sqlx", "eda-kafka"] }
```

| Feature | Re-exported alias | Pulls in |
|---------|-------------------|----------|
| `data-sqlx` | `firefly::data_sqlx` | relational repository adapter (Postgres/MySQL/SQLite over sqlx) |
| `data-mongodb` | `firefly::data_mongodb` | document repository adapter (MongoDB) |
| `eda-kafka` | `firefly::eda_kafka` | Kafka broker |
| `eda-rabbitmq` | `firefly::eda_rabbitmq` | RabbitMQ broker |
| `eda-redis` | `firefly::eda_redis` | Redis Streams broker |
| `eda-postgres` | `firefly::eda_postgres` | Postgres broker |
| `cache-redis` | `firefly::cache_redis` | Redis cache adapter |
| `cache-postgres` | `firefly::cache_postgres` | Postgres cache adapter |
| `admin` | `firefly::admin` | back-office / admin surface |
| `full` | all of the above | — |

Each optional alias **and** its `__rt` entry are gated behind the matching
feature, so generated code that targets an adapter still resolves through the
same contract path once the feature is enabled — and a lean build compiles none
of them.

## Public surface

* `firefly::__rt` — the macro contract module (hidden; re-exports every runtime
  crate under its crate name).
* Module aliases (`firefly::cqrs`, `firefly::web`, …) — one per runtime crate,
  plus feature-gated aliases for each adapter.
* `firefly::*` (crate root) — every macro from `firefly-macros`.
* `firefly::prelude` — the high-frequency surface + every macro.
* `firefly::VERSION` — the calendar-versioned framework stamp (matches every
  other `firefly-*` crate).

## Design notes

* **Pure re-export crate.** `firefly` adds no runtime types of its own; it is a
  facade. This keeps it cheap to depend on and impossible to drift from the
  crates it fronts.
* **One stable path for macros.** Proc-macro crates cannot re-export runtime
  types, so generated code needs an absolute path it can always reach. `__rt`
  is that path. Without the facade, the macros would force a direct dependency
  on every runtime crate.
* **Lean by default, complete on demand.** Ports are always present; vendor
  adapters are feature-gated, so the front-door dependency never drags in a
  database or message-broker client you did not ask for.

## Related crates

* `firefly-macros` — the derive/attribute macros re-exported here.
* `firefly-starter-core` / `firefly-starter-web` — the wiring `Core` the
  prelude surfaces.
* Every other `firefly-*` crate — reached through the aliases and `__rt`.
