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

- **Composed, not constructed.** One line boots the whole service. `FireflyApplication::new("lumen").run()`
  component-scans your beans, auto-wires and auto-mounts the controllers,
  handlers, listeners, and scheduled tasks, self-hosts an admin dashboard, and
  serves the public + management ports with graceful shutdown — the framework
  assembles the object graph instead of you spelling it out in a composition
  root. You write commands, queries, handlers, and routes; nothing more.
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

> **Design note.** `FireflyApplication::new(name).run()` is Firefly's composition
> root — the Rust analog of Spring Boot's `SpringApplication.run`. It stands up
> the middleware, the bus, the broker, health, and metrics, then component-scans
> and wires your beans, all from one line. Configuration layers defaults →
> profile → environment, and any handler can return a `Mono<T>` / `Flux<T>`. If
> you have used a batteries-included framework before, this will feel familiar.

## The one-dependency facade

Here is the part that surprises people. Lumen — a service with CQRS, event
sourcing, a saga, JWT security, scheduling, and an actuator surface — declares
exactly one Firefly dependency. This is its real `Cargo.toml`:

```toml
[dependencies]
# The whole framework AND every `#[derive(...)]` / `#[...]` macro.
firefly = { version = "26.6.20" }

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
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
     viewBox="0 0 900 520" role="img"
     aria-label="Firefly architecture: one firefly facade front door, four strictly-layered tiers (Foundational, Platform, Adapters, Starters) building left to right, on a firefly-reactive Mono/Flux core"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <defs>
    <linearGradient id="parch" x1="0" y1="0" x2="0" y2="520" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#fdf7ec"/><stop offset="1" stop-color="#f4e7d0"/>
    </linearGradient>
    <radialGradient id="amb" cx="770" cy="40" r="460" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#f6a821" stop-opacity="0.13"/><stop offset="1" stop-color="#f6a821" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="amber" x1="0" y1="92" x2="0" y2="156" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#ffd87a"/><stop offset="1" stop-color="#f3a41d"/>
    </linearGradient>
    <linearGradient id="goldbar" x1="0" y1="174" x2="0" y2="216" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#f9ba43"/><stop offset="1" stop-color="#ee9b14"/>
    </linearGradient>
    <linearGradient id="bedrock" x1="60" y1="0" x2="840" y2="0" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#241a10"/><stop offset="0.5" stop-color="#37270f"/><stop offset="1" stop-color="#241a10"/>
    </linearGradient>
    <g id="ff"><circle r="9" fill="#f6a821" opacity="0.10"/><circle r="5" fill="#ffc24a" opacity="0.22"/><circle r="2.6" fill="#ffd980" opacity="0.7"/><circle r="1.3" fill="#fff6e0"/></g>
    <g id="fg"><circle r="9" fill="#9bd24a" opacity="0.10"/><circle r="5" fill="#c2e85f" opacity="0.20"/><circle r="2.6" fill="#dff58a" opacity="0.7"/><circle r="1.3" fill="#fbffe2"/></g>
  </defs>

  <!-- canvas -->
  <rect x="0.5" y="0.5" width="899" height="519" rx="18" fill="url(#parch)" stroke="#e6d3ad"/>
  <rect rx="18" fill="url(#amb)"/>
  <!-- background motes -->
  <g fill="#e0b25a">
    <circle cx="828" cy="38" r="1.4" opacity="0.5"/><circle cx="690" cy="30" r="1.0" opacity="0.4"/>
    <circle cx="44" cy="300" r="1.2" opacity="0.4"/><circle cx="862" cy="300" r="1.1" opacity="0.42"/>
    <circle cx="500" cy="20" r="1.0" opacity="0.35"/>
  </g>
  <use xlink:href="#fg" transform="translate(842,470) scale(0.85)"/>
  <use xlink:href="#ff" transform="translate(36,150) scale(0.8)"/>

  <!-- title -->
  <use xlink:href="#ff" transform="translate(70,40) scale(1.25)"/>
  <text x="92" y="46" font-size="22" font-weight="800" fill="#2a1d10" letter-spacing="0.3">Architecture at a glance</text>
  <text x="840" y="44" text-anchor="end" font-size="12" font-weight="600" fill="#b18a52" letter-spacing="0.5">fireflyframework-rust</text>
  <line x1="60" y1="62" x2="840" y2="62" stroke="#f6a821" stroke-width="1.4" opacity="0.45"/>
  <text x="450" y="80" text-anchor="middle" font-size="11.5" font-style="italic" fill="#8a6f48">One dependency in; four strictly-layered tiers building left to right; a reactive core at the base.</text>

  <!-- front door (facade) -->
  <rect x="60" y="98" width="780" height="62" rx="14" fill="#e3cfa8" opacity="0.55"/>
  <rect x="60" y="94" width="780" height="62" rx="14" fill="url(#amber)" stroke="#d98f1e" stroke-width="1.4"/>
  <rect x="74" y="100" width="752" height="2" rx="1" fill="#fffdf5" opacity="0.30"/>
  <use xlink:href="#ff" transform="translate(98,126) scale(1.35)"/>
  <text x="128" y="119" font-size="11" font-weight="800" fill="#6e4710" letter-spacing="1.6">THE FRONT DOOR</text>
  <text x="128" y="141" font-size="16" font-weight="800" fill="#3a2310" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">firefly  +  firefly-macros</text>
  <text x="826" y="119" text-anchor="end" font-size="11.5" fill="#6e4710" font-weight="600">one dependency &#183; use firefly::prelude::*;</text>
  <text x="826" y="141" text-anchor="end" font-size="11.5" fill="#6e4710" font-weight="600">declarative macros &#183; stable __rt contract</text>

  <!-- front-door to tiers connectors -->
  <g stroke="#e0a93a" stroke-width="1.5" stroke-dasharray="2 3" opacity="0.75">
    <line x1="148.5" y1="158" x2="148.5" y2="173"/><line x1="349.5" y1="158" x2="349.5" y2="173"/>
    <line x1="550.5" y1="158" x2="550.5" y2="173"/><line x1="751.5" y1="158" x2="751.5" y2="173"/>
  </g>

  <!-- ===== TIER 1: FOUNDATIONAL ===== -->
  <rect x="60" y="178" width="177" height="268" rx="12" fill="#e7d4b2" opacity="0.5"/>
  <rect x="60" y="174" width="177" height="268" rx="12" fill="#fffdf8" stroke="#e3d0aa" stroke-width="1.4"/>
  <path d="M60,186 a12,12 0 0 1 12,-12 h153 a12,12 0 0 1 12,12 v30 h-177 z" fill="url(#goldbar)"/>
  <circle cx="80" cy="195" r="11" fill="#fff6e0" opacity="0.9"/><text x="80" y="199" text-anchor="middle" font-size="12" font-weight="800" fill="#b06a16">1</text>
  <text x="155" y="200" text-anchor="middle" font-size="14" font-weight="800" fill="#2a1d10" letter-spacing="0.4">FOUNDATIONAL</text>
  <text x="148.5" y="236" text-anchor="middle" font-size="9.5" font-style="italic" fill="#a07e4e">reactive base &#183; cross-cutting</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="78" y="262">kernel</text><text x="78" y="284">web</text>
    <text x="78" y="306">config</text><text x="78" y="328">validators</text>
    <text x="78" y="350">container</text><text x="78" y="372">i18n</text>
    <text x="78" y="394" fill="#a98f63">+ utils, session</text>
  </g>

  <!-- ===== TIER 2: PLATFORM ===== -->
  <rect x="261" y="178" width="177" height="268" rx="12" fill="#e7d4b2" opacity="0.5"/>
  <rect x="261" y="174" width="177" height="268" rx="12" fill="#fffdf8" stroke="#e3d0aa" stroke-width="1.4"/>
  <path d="M261,186 a12,12 0 0 1 12,-12 h153 a12,12 0 0 1 12,12 v30 h-177 z" fill="url(#goldbar)"/>
  <circle cx="281" cy="195" r="11" fill="#fff6e0" opacity="0.9"/><text x="281" y="199" text-anchor="middle" font-size="12" font-weight="800" fill="#b06a16">2</text>
  <text x="356" y="200" text-anchor="middle" font-size="14" font-weight="800" fill="#2a1d10" letter-spacing="0.4">PLATFORM</text>
  <text x="349.5" y="236" text-anchor="middle" font-size="9.5" font-style="italic" fill="#a07e4e">engines &#183; defines ports</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="279" y="262">cqrs</text><text x="279" y="284">eda</text>
    <text x="279" y="306">eventsourcing</text><text x="279" y="328">orchestration</text>
    <text x="279" y="350">cache</text><text x="279" y="372">security</text>
    <text x="279" y="394" fill="#a98f63">+ observability, &#8230;</text>
  </g>

  <!-- ===== TIER 3: ADAPTERS ===== -->
  <rect x="462" y="178" width="177" height="268" rx="12" fill="#e7d4b2" opacity="0.5"/>
  <rect x="462" y="174" width="177" height="268" rx="12" fill="#fffdf8" stroke="#e3d0aa" stroke-width="1.4"/>
  <path d="M462,186 a12,12 0 0 1 12,-12 h153 a12,12 0 0 1 12,12 v30 h-177 z" fill="url(#goldbar)"/>
  <circle cx="482" cy="195" r="11" fill="#fff6e0" opacity="0.9"/><text x="482" y="199" text-anchor="middle" font-size="12" font-weight="800" fill="#b06a16">3</text>
  <text x="557" y="200" text-anchor="middle" font-size="14" font-weight="800" fill="#2a1d10" letter-spacing="0.4">ADAPTERS</text>
  <text x="550.5" y="236" text-anchor="middle" font-size="9.5" font-style="italic" fill="#a07e4e">implement the ports</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="480" y="262">data-sqlx</text><text x="480" y="284">data-mongodb</text>
    <text x="480" y="306">eda-kafka</text><text x="480" y="328">cache-redis</text>
    <text x="480" y="350">idp-* &#183; ecm-*</text><text x="480" y="372">notifications-*</text>
    <text x="480" y="394" fill="#a98f63">+ client, webhooks</text>
  </g>

  <!-- ===== TIER 4: STARTERS ===== -->
  <rect x="663" y="178" width="177" height="268" rx="12" fill="#e7d4b2" opacity="0.5"/>
  <rect x="663" y="174" width="177" height="268" rx="12" fill="#fffdf8" stroke="#e3d0aa" stroke-width="1.4"/>
  <path d="M663,186 a12,12 0 0 1 12,-12 h153 a12,12 0 0 1 12,12 v30 h-177 z" fill="url(#goldbar)"/>
  <circle cx="683" cy="195" r="11" fill="#fff6e0" opacity="0.9"/><text x="683" y="199" text-anchor="middle" font-size="12" font-weight="800" fill="#b06a16">4</text>
  <text x="758" y="200" text-anchor="middle" font-size="14" font-weight="800" fill="#2a1d10" letter-spacing="0.4">STARTERS</text>
  <text x="751.5" y="236" text-anchor="middle" font-size="9.5" font-style="italic" fill="#a07e4e">compose &#183; ship</text>
  <g font-size="11.5" fill="#3a2a1c" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">
    <text x="681" y="262">starter-core</text><text x="681" y="284">starter-web</text>
    <text x="681" y="306">starter-domain</text><text x="681" y="328">starter-data</text>
    <text x="681" y="350">admin</text><text x="681" y="372">cli</text>
    <text x="681" y="394" fill="#a98f63">+ backoffice</text>
  </g>

  <!-- left-to-right "builds on" chevrons (direction explained in the subtitle) -->
  <g fill="#d4793a">
    <polygon points="243,300 259,310 243,320"/><polygon points="444,300 460,310 444,320"/><polygon points="645,300 661,310 645,320"/>
  </g>

  <!-- reactive bedrock -->
  <rect x="60" y="466" width="780" height="46" rx="13" fill="url(#bedrock)"/>
  <circle cx="94" cy="489" r="12" fill="none" stroke="#9bd24a" stroke-width="1.4" opacity="0.65"/>
  <ellipse cx="94" cy="489" rx="12" ry="4.5" fill="none" stroke="#c2e85f" stroke-width="1" opacity="0.5"/>
  <circle cx="94" cy="489" r="3.4" fill="#dff58a"/>
  <text x="120" y="484" font-size="13" font-weight="800" fill="#ffe9b0" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">firefly-reactive</text>
  <text x="120" y="500" font-size="10.5" fill="#cdb389">the Mono / Flux reactive core every tier is built on</text>
  <text x="824" y="492" text-anchor="end" font-size="10.5" fill="#8f7a52" font-weight="600" letter-spacing="0.4">tokio &#183; axum &#183; async-native</text>
</svg>
<figcaption>Firefly at a glance: a service depends only on the firefly facade (the front door); the four tiers build left to right, each depending only on the tiers to its left, all on the firefly-reactive Mono / Flux core.</figcaption>
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
  (`firefly::starter_core`) plus the web middleware — the stack
  `FireflyApplication` builds for you at boot.

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
in-memory baseline and the production deployment differ only in a `#[bean]`
factory — the wiring the framework scans, not the business code.

## The road ahead: Lumen, chapter by chapter

The rest of the book is Lumen's growth, additive and in order. The early
chapters introduce the framework with small standalone snippets; **Lumen proper
begins in [Chapter 6](./06-first-http-api.md)**:

- **Foundations** — scaffold and boot Lumen, bind its configuration and profiles,
  understand how `FireflyApplication` wires the beans it scans, master
  `Mono`/`Flux`, and expose the first validated REST endpoints.
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
  to make by changing a single `#[bean]` factory.

## Exercises

1. Open `samples/lumen/Cargo.toml` and confirm the dependency list: one
   `firefly`, plus `axum`/`serde`/`serde_json`/`tokio`/`uuid`/`chrono`/`async-trait`.
   Note that no `firefly-*` sub-crate is listed directly.
2. Skim `samples/lumen/src/main.rs` — the single-binary crate root. List the ten
   modules it declares
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
