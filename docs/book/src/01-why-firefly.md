# Why Firefly for Rust

By the end of this chapter you will understand the problem Firefly solves, how
its tiers fit together behind a single facade crate, and why Lumen — the
digital-wallet service you grow over the rest of the book — depends on exactly
**one** Firefly crate to get all of it. No code lands in Lumen yet; this chapter
sets the stage and the philosophy. The next one boots the scaffold.

## The cohesion problem

Picture your first day on a new Rust microservice. Before you write one line of
business logic, you face a cascade of choices. Which HTTP layer — axum, actix,
warp, poem? Which database story — sqlx, SeaORM, diesel, raw `tokio-postgres`?
How do you wire dependencies — a hand-rolled `AppState`, a DI crate, lazy
statics? How do you handle configuration, errors, correlation IDs, metrics,
graceful shutdown? Every team invents its own answer.

You assemble a bespoke stack, glue it together with good intentions, and ship.
Six months later a second team starts a second service and makes entirely
different choices. Now you have two codebases with incompatible conventions,
different error shapes, different observability stories, and no shared
understanding of how anything works.

**Rust gives you infinite choice. What it does not give you is cohesion.**

The stack-assembly problem is not a skills failure — it is a tooling gap. Mature
ecosystems closed it with a single opinionated, batteries-included framework that
makes sensible choices, lets you override what matters, and enforces a consistent
idiom across every service. Firefly is that framework for Rust: it makes the
cross-cutting decisions once, so every service shares one idiom.

## What is Firefly?

Firefly is a **cohesive, reactive, async-native framework** for building
production-grade Rust services. It makes the cross-cutting decisions for you —
HTTP middleware, configuration, caching, CQRS, messaging, security,
observability — all integrated, all consistent, with production-ready defaults
from the very first `cargo run`.

Under the hood Firefly delegates to battle-tested libraries — `tokio` for the
runtime, `axum`/`tower` for HTTP, `serde` for serialization, `tracing` for
logging, RustCrypto for crypto — but you depend on **Firefly's ports**
(object-safe `async_trait` traits), and you select concrete adapters at wiring
time. Swap an in-memory event store for PostgreSQL, or the in-process broker for
Kafka, without touching a single line of business logic — exactly the swap Lumen
is structured to make.

Firefly's defining principles:

- **Composed, not constructed.** One call wires the whole infrastructure tier —
  middleware chain, cache, CQRS bus, event broker, health composite, metrics,
  scheduler, lifecycle. You write commands, queries, handlers, and routes;
  nothing more. Lumen builds its core with `WebStack::new(CoreConfig { .. })`.
- **Contract-first and interoperable.** The wire contract — the
  `application/problem+json` shape, the `Idempotency-Key` semantics, the saga
  step definitions, the event envelopes — is a stable, versioned, language-neutral
  specification. Any service that honors it interoperates with a Firefly service
  byte-for-byte, so Firefly slots into a polyglot fleet without bespoke glue.
- **Pluggable at the adapter layer.** Each integration point (cache, broker,
  IDP, ECM, notification channel) is an object-safe port with multiple adapter
  implementations selected at wiring time as an `Arc<dyn Port>`.
- **Observable by default.** `tracing` structured logging with correlation-id
  enrichment, actuator health/metrics endpoints, RFC 9457 error envelopes, and a
  startup banner are all on out of the box.
- **Reactive to the core.** A first-class `Mono`/`Flux` reactive surface runs
  from reactive endpoints through reactive repositories, the reactive HTTP
  client, and reactive EDA/CQRS — a lazy, composable, backpressure-aware
  streaming model built natively on tokio.

> **Design note.** One `WebStack::new` call stands up the middleware, the bus,
> the broker, health, and metrics — Firefly's composition root. Configuration
> layers defaults → profile → environment, and any handler can return a
> `Mono<T>` / `Flux<T>`. If you have used a batteries-included framework before,
> this will feel familiar.

## The one-dependency facade

Here is the part that surprises people. Lumen — a service with CQRS, event
sourcing, a saga, JWT security, scheduling, and an actuator surface — declares
exactly one Firefly dependency. This is its real `Cargo.toml`:

```toml
[dependencies]
# The whole framework AND every `#[derive(...)]` / `#[...]` macro.
firefly = { version = "26.6.5" }

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and
# event payloads are Serialize/Deserialize).
axum  = { version = "0.7" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Async runtime, plus the id/clock crates the wallet domain uses.
tokio  = { version = "1" }
uuid   = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }

# Backs the `async fn` trait methods the domain ports implement.
async-trait = { version = "0.1" }
```

The `firefly` crate is a **facade**: it re-exports every `firefly-*` crate behind
a clean path (`firefly::cqrs`, `firefly::eventsourcing`, `firefly::reactive`,
`firefly::security`, …) and re-exports every macro at the crate root. The
high-frequency surface — plus all the macros — comes in through a single glob:

```rust
use firefly::prelude::*;
```

That one line gives Lumen the CQRS `Bus`, the dependency-injection `Container`,
the `Scheduler`, the saga `Saga`/`Step`, the lifecycle `Application`, the
reactive `Mono`/`Flux`, the `WebResult`/`WebError` web types, the `FireflyError`
kernel error, and every `#[derive(...)]` / `#[...]` macro the service uses.

Lumen takes the discipline one step further: even its typed error enums —
`MoneyError`, `DomainError`, `CqrsError` mapping — hand-write `Display` and
`std::error::Error` instead of reaching for `thiserror`. The one-dependency
promise holds end to end, and the chapters point it out where it matters.

> **Design note.** The `firefly` facade is a single front-door crate: one
> coordinate on your dependency list pulls in a curated, calendar-version-aligned
> stack, and `use firefly::prelude::*;` brings the whole high-frequency surface
> and every macro into scope at once. One dependency, no version skew to manage.

## The tiers behind the facade

Behind that single crate the framework is organized into strictly-layered tiers,
with a left-to-right dependency direction. Each tier may depend on the tiers to
its left, never to its right; the Cargo crate graph enforces the layering. You
rarely name these crates directly — the facade re-exports them — but knowing the
shape tells you where each capability lives.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 720 244" role="img"
     aria-label="The four framework tiers, left to right: Foundational, Platform, Adapters, Starters"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <!-- Foundational -->
  <rect x="8" y="20" width="150" height="216" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M8,30 a10,10 0 0 1 10,-10 h130 a10,10 0 0 1 10,10 v22 h-150 z" fill="#f6a821"/>
  <text x="83" y="41" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">FOUNDATIONAL</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="22" y="74">kernel</text><text x="22" y="95">reactive</text>
    <text x="22" y="116">web</text><text x="22" y="137">config</text>
    <text x="22" y="158">validators</text><text x="22" y="179">i18n</text>
    <text x="22" y="200">container</text>
  </g>
  <!-- Platform -->
  <rect x="186" y="20" width="150" height="216" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M186,30 a10,10 0 0 1 10,-10 h130 a10,10 0 0 1 10,10 v22 h-150 z" fill="#f6a821"/>
  <text x="261" y="41" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">PLATFORM</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="200" y="74">cache</text><text x="200" y="95">observability</text>
    <text x="200" y="116">cqrs</text><text x="200" y="137">eda</text>
    <text x="200" y="158">eventsourcing</text><text x="200" y="179">orchestration</text>
    <text x="200" y="200">scheduling</text><text x="200" y="221">security</text>
  </g>
  <!-- Adapters -->
  <rect x="364" y="20" width="150" height="216" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M364,30 a10,10 0 0 1 10,-10 h130 a10,10 0 0 1 10,10 v22 h-150 z" fill="#f6a821"/>
  <text x="439" y="41" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">ADAPTERS</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="378" y="74">client</text><text x="378" y="95">idp-*</text>
    <text x="378" y="116">ecm-*</text><text x="378" y="137">notifications-*</text>
    <text x="378" y="158">cache-redis</text><text x="378" y="179">eda-kafka</text>
    <text x="378" y="200">data-sqlx</text><text x="378" y="221">data-mongodb</text>
  </g>
  <!-- Starters -->
  <rect x="542" y="20" width="150" height="216" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <path d="M542,30 a10,10 0 0 1 10,-10 h130 a10,10 0 0 1 10,10 v22 h-150 z" fill="#f6a821"/>
  <text x="617" y="41" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10">STARTERS</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="556" y="74">starter-core</text><text x="556" y="95">starter-web</text>
    <text x="556" y="116">starter-domain</text><text x="556" y="137">starter-data</text>
    <text x="556" y="158">starter-experience</text><text x="556" y="179">admin</text>
    <text x="556" y="200">cli</text>
  </g>
  <!-- left-to-right "builds on" arrows -->
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="160" y1="128" x2="178" y2="128"/><polygon points="186,128 178,124 178,132"/>
    <line x1="338" y1="128" x2="356" y2="128"/><polygon points="364,128 356,124 356,132"/>
    <line x1="516" y1="128" x2="534" y2="128"/><polygon points="542,128 534,124 534,132"/>
  </g>
</svg>
<figcaption>The framework's four tiers. Each tier may depend only on the tiers to its left; the Cargo crate graph enforces the layering.</figcaption>
</figure>

- **Foundational** crates are the vocabulary: `firefly-kernel` (errors, clock,
  correlation scopes, DDD kit), `firefly-reactive` (`Mono`/`Flux`),
  `firefly-web` (middleware), `firefly-config`, `firefly-validators`,
  `firefly-i18n`, and `firefly-container` — a full dependency-injection engine
  with component scanning and stereotype derives, covered in depth in Chapter 4.
- **Platform** crates are the capabilities: caching, CQRS, EDA, event sourcing,
  orchestration, scheduling, resilience, security, observability. Lumen reaches
  for `firefly::cqrs`, `firefly::eventsourcing`, `firefly::orchestration`,
  `firefly::scheduling`, and `firefly::security` here.
- **Adapters** are the concrete integrations: the REST/reactive HTTP client, the
  IDP vendors, ECM, notifications, the event transports (Kafka, RabbitMQ,
  Postgres outbox, Redis Streams), and the persistence adapters (`firefly-data-sqlx`
  for relational stores, `firefly-data-mongodb` for documents) — a pluggable
  multi-database story that Chapter 7 builds on. Lumen ships on the in-memory
  adapters and points at the production swaps in callouts.
- **Starters** bundle a sensible default stack so a service depends on one crate.
  Lumen's web tier is `firefly::starter_web::WebStack`, which wires the core
  (`firefly::starter_core`) plus the web middleware in one constructor.

For the full per-crate catalogue see the [Module Index](./91-appendix-modules.md).

## Choosing your adapters

Lumen runs with **zero external infrastructure** — that is what makes it a good
teaching baseline and a fast test target. It boots on the in-process
`MemoryEventStore` and the in-process broker, so `cargo run` and `cargo test`
need nothing but the crate. When you are ready for production, you change the
*wiring*, not the handlers:

- **Event store.** Swap `MemoryEventStore` for a durable adapter where the
  `Arc<dyn EventStore>` is constructed; the `Ledger`, the projection, and every
  command handler are untouched.
- **Event transport.** The in-process broker that carries Lumen's domain events
  implements the same `Broker` port as `firefly-eda-kafka`, `-rabbitmq`,
  `-postgres`, and `-redis`. Swap the constructor, keep your `#[event_listener]`.
- **Cache, identity, notifications.** Code against the parent-port trait
  (`cache::Adapter`, `security::Verifier`, `notifications::Channel`) and pull in
  the concrete adapter crate at wiring time, so heavy SDKs stay out of services
  that do not use them.

This is the thread that runs through the whole book: Lumen is written so the
in-memory baseline and the production deployment differ only at the composition
root.

## The road ahead: Lumen, chapter by chapter

The rest of the book is Lumen's growth, additive and in order. The early
chapters introduce the framework with small standalone snippets; **Lumen proper
begins in [Chapter 6](./06-first-http-api.md)**:

- **Foundations** — scaffold and boot Lumen, bind its configuration and profiles,
  understand the composition root, master `Mono`/`Flux`, and expose the first
  validated REST endpoints.
- **Modeling & persisting** — a read model behind a repository, the `Money`
  value object and the `Wallet` aggregate, and the CQRS command/query split on a
  bus.
- **Event-driven** — domain events, a projection that keeps the read model
  current, and the event-sourced ledger that folds its stream.
- **Into microservices** — an HTTP-client sketch and the compensating transfer
  saga.
- **Secure · observe · ship** — JWT bearer auth and RBAC, the actuator surface,
  caching, a scheduled task, the test suite, and the production entry point with
  graceful shutdown and a reactive streaming endpoint.

By the last page, Lumen is the complete `samples/lumen` crate — and you have
written every line of it.

## Recap — what changed in Lumen

Nothing in code yet. This chapter framed the journey:

- The **cohesion problem** Firefly exists to solve, and the opinionated,
  one-framework answer it brings to Rust.
- The **one-dependency facade** — Lumen depends on a single `firefly` crate, and
  `use firefly::prelude::*;` brings in the whole high-frequency surface and every
  macro. Even the typed errors avoid `thiserror`, so the promise holds end to
  end.
- The **tiers** behind that facade (foundational → platform → adapters →
  starters) and the in-memory-to-production **adapter swap** that Lumen is built
  to make at the composition root.

## Exercises

1. Open `samples/lumen/Cargo.toml` and confirm the dependency list: one
   `firefly`, plus `axum`/`serde`/`serde_json`/`tokio`/`uuid`/`chrono`/`async-trait`.
   Note that no `firefly-*` sub-crate is listed directly.
2. Skim `samples/lumen/src/lib.rs`. List the ten modules it declares
   (`money`, `domain`, `ledger`, `commands`, `transfer`, `tcc_transfer`,
   `security`, `compliance`, `web`, `housekeeping`) and predict which book part
   introduces each.
3. Run `cargo doc -p firefly-sample-lumen --open` and read the crate-level
   documentation. It contains the same "building block → module → Firefly
   surface" table the book is organized around.
4. For each of these production swaps, find the port trait it would implement in
   the facade: a Postgres event store, a Kafka broker, a Redis cache. (Hint:
   `firefly::eventsourcing`, `firefly::eda`, `firefly::cache`.)

The next chapter gets Lumen running. Turn to the [Quickstart](./02-quickstart.md).
