# Why Firefly for Rust

Every service in this book is **Lumen** — the digital-wallet and ledger service
you will grow, chapter by chapter, into the complete
[`samples/lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen)
crate. Before you scaffold it in the next chapter, this one answers the question
underneath the whole project: *why does a Rust service need a framework at all,
and why this one?* No code lands in Lumen yet. By the end you will understand the
problem Firefly solves, the single dependency it arrives through, the tiers that
live behind that dependency, and the one design choice — the in-memory-to-
production adapter swap — that the rest of the book is built around.

This is a read-and-orient chapter, not a type-along one, but it is not hand-wavy:
every term you will meet for the next nineteen chapters is defined here from first
principles, and every claim is something you can verify against the real
`samples/lumen` crate in the closing exercises.

By the end of this chapter you will:

- Explain the **cohesion problem** that an opinionated framework exists to solve,
  and why Rust in particular feels its absence.
- Describe what Firefly *is* — a cohesive, reactive, async-native framework — and
  name the battle-tested libraries it delegates to underneath.
- Read Lumen's real `Cargo.toml` and explain why a service this rich depends on
  exactly **one** Firefly crate, the `firefly` facade.
- Map the four **tiers** behind the facade (foundational → platform → adapters →
  starters) and say where each capability lives.
- Describe the **adapter swap** — how Lumen moves from an in-memory baseline to a
  production deployment by changing wiring, not business logic.

## Concepts you will meet

Before the prose, here are the four ideas this chapter leans on. Each is
reintroduced in context where it first appears; this is the short version, so the
later sections read fast.

> **Note** **Key term — framework vs. library.** A *library* is code you call: you
> stay in charge of control flow and reach into the library when you need it. A
> *framework* is code that calls you: it owns the lifecycle — startup, request
> dispatch, shutdown — and invokes the small pieces you supply. This inversion is
> the whole point of Firefly, and it is exactly the relationship Spring Boot has
> with a Java service.

> **Note** **Key term — facade crate.** A *facade* is a single crate that
> re-exports a whole family of crates (and their macros) so that you depend on one
> name instead of many. Firefly ships its entire framework behind the `firefly`
> facade. The Spring analog is a Spring Boot *starter* — except here there is
> essentially one front door that covers everything.

> **Note** **Key term — port and adapter.** A *port* is an abstract capability
> expressed as a trait — "something that stores events," "something that publishes
> messages" — with no implementation. An *adapter* is a concrete implementation of
> that port — an in-memory store, a PostgreSQL store, a Kafka broker. You write
> your code against the port; you pick the adapter at wiring time. This is the
> hexagonal-architecture vocabulary and it maps to Spring's interface-plus-bean
> idiom.

> **Note** **Key term — bean and wiring.** A *bean* is an object the framework
> constructs and manages for you, then hands to whoever needs it. *Wiring* is the
> act of connecting beans together — giving each one the collaborators it
> depends on. You declare beans; the framework discovers and wires them at
> startup. This is exactly Spring's notion of a bean in an application context.

## Step 1 — Recognize the cohesion problem

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

What just happened: you named the problem. The stack-assembly tax is not a skills
failure — it is a tooling gap. Mature ecosystems closed it with a single
opinionated, batteries-included framework that makes sensible choices, lets you
override what matters, and enforces a consistent idiom across every service.

> **Design note.** This is the framework-vs-library inversion in practice. A pile
> of libraries leaves *you* holding the lifecycle: you decide when the HTTP server
> binds, how configuration loads, where errors turn into responses. A framework
> makes those cross-cutting decisions once, so every service that uses it shares
> one idiom — and an operator who learns one Firefly service can read all of them.

Firefly is that framework for Rust. It makes the cross-cutting decisions once, so
every service shares one idiom — and the cost of starting service number two is
no longer a fresh round of architecture debates.

> **Tip** **Checkpoint.** You can state the problem in one sentence: *Rust offers
> infinite choice but no built-in cohesion, and an opinionated framework supplies
> the missing cohesion.* If that sentence feels obvious, the rest of the book will
> read as "here is how Firefly supplies it."

## Step 2 — Understand what Firefly is (and delegates to)

Firefly is a **cohesive, reactive, async-native framework** for building
production-grade Rust services. It makes the cross-cutting decisions for you —
HTTP middleware, configuration, caching, Command/Query Responsibility
Segregation, messaging, security, observability — all integrated, all consistent,
with production-ready defaults from the very first `cargo run`.

> **Note** **Key term — reactive (`Mono` / `Flux`).** *Reactive* here means a
> lazy, composable, backpressure-aware streaming model. A `Mono<T>` is an async
> computation that yields *at most one* value; a `Flux<T>` yields *zero or more*
> over time. They are built natively on Tokio and run end to end — from reactive
> endpoints through reactive repositories, the reactive HTTP client, and reactive
> messaging. If you have used Project Reactor in the Spring world, these are the
> same two types by the same names. You will master them in
> [the reactive model](./05-reactive-model.md).

Firefly does not reinvent the wheel underneath. It **delegates to battle-tested
libraries** — `tokio` for the runtime, `axum`/`tower` for HTTP, `serde` for
serialization, `tracing` for structured logging, RustCrypto for cryptography. The
twist is the direction you depend on them:

- You depend on **Firefly's ports** — object-safe `async_trait` traits — for
  cross-cutting capabilities like event storage and messaging.
- You select **concrete adapters** at wiring time, as an `Arc<dyn Port>`.

Because of that indirection you can swap an in-memory event store for PostgreSQL,
or the in-process broker for Kafka, without touching a single line of business
logic — exactly the swap Lumen is structured to make and that Step 5 returns to.

Firefly's defining principles, each of which a later chapter makes concrete:

- **Composed, not constructed.** One line boots the whole service.
  `FireflyApplication::new("lumen").run()` component-scans your beans, auto-wires
  and auto-mounts the controllers, handlers, listeners, and scheduled tasks,
  self-hosts an admin dashboard, and serves the public + management ports with
  graceful shutdown — the framework assembles the object graph instead of you
  spelling it out by hand. You write commands, queries, handlers, and routes;
  nothing more. [Quickstart](./02-quickstart.md) walks this line stage by stage.
- **Contract-first and interoperable.** The wire contract — the
  `application/problem+json` error shape (RFC 9457), the `Idempotency-Key`
  semantics, the saga step definitions, the event envelopes — is a stable,
  versioned, language-neutral specification. Any service that honors it
  interoperates with a Firefly service byte-for-byte, so Firefly slots into a
  polyglot fleet without bespoke glue.
- **Pluggable at the adapter layer.** Each integration point (cache, broker,
  identity provider, content store, notification channel) is a port with multiple
  adapter implementations, selected at wiring time as an `Arc<dyn Port>`.
- **Observable by default.** `tracing` structured logging with correlation-id
  enrichment, actuator health and metrics endpoints, RFC 9457 error envelopes, and
  a startup banner are all on out of the box.
- **Reactive to the core.** The `Mono`/`Flux` surface runs from endpoints to
  repositories to the HTTP client to messaging — lazy, composable, and
  backpressure-aware.

> **Note** **Key term — RFC 9457 problem responses.** RFC 9457 (which obsoletes
> RFC 7807) defines `application/problem+json` — a standard JSON shape for HTTP
> errors with a `type`, `title`, `status`, and `detail`. Firefly renders every
> handler error in this shape automatically, so your API speaks one error dialect
> from the first endpoint. You meet it for real in
> [Your First HTTP API](./06-first-http-api.md).

> **Design note.** `FireflyApplication::new(name).run()` is Firefly's composition
> root — the Rust analog of Spring Boot's `SpringApplication.run(App.class, args)`.
> It stands up the middleware, the bus, the broker, health, and metrics, then
> component-scans and wires your beans, all from one line. Configuration layers
> defaults → profile → environment, and any handler can return a `Mono<T>` /
> `Flux<T>`. If you have used a batteries-included framework before, this will feel
> familiar.

> **Tip** **Checkpoint.** You can name two things at once: *what* Firefly gives you
> (one cohesive, reactive, observable stack) and *what it stands on* (tokio, axum,
> serde, tracing, RustCrypto). Firefly is the cohesion layer, not a from-scratch
> reimplementation.

## Step 3 — Read the one-dependency facade

Here is the part that surprises people. Lumen — a service with Command/Query
Responsibility Segregation, event sourcing, a saga, JWT security, scheduling, and
an actuator surface — declares exactly one Firefly dependency. This is the shape
of its real `Cargo.toml`:

```toml
[dependencies]
# The whole framework AND every `#[derive(...)]` / `#[...]` macro. The `admin`
# feature pulls in the self-hosted admin dashboard the management port mounts.
firefly = { version = "26.6.28", features = ["admin"] }

# The two ecosystem crates a Firefly service still writes against directly:
# axum (you author the controller handlers) and serde (your messages and
# event payloads are Serialize/Deserialize); serde_json encodes the event
# payloads.
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

What just happened, block by block:

- The first line — `firefly = { version = "26.6.28", features = ["admin"] }` — is
  the *entire framework*. Every capability and every macro arrives through it.
- The `axum` / `serde` / `serde_json` block is the small surface you still write
  *against directly*: you author the controller handler functions on `axum`, and
  your messages and event payloads derive `serde`'s `Serialize`/`Deserialize`.
- The `tokio` / `uuid` / `chrono` block is the runtime and the id/clock crates the
  wallet domain reaches for — wallet ids and event timestamps.
- `async-trait` backs the `async fn` methods on the domain's port traits.

Notice what is *not* there: no `firefly-web`, no `firefly-cqrs`, no
`firefly-security`. You never list a `firefly-*` sub-crate by hand.

> **Note** **Key term — prelude glob.** A *prelude* is a module of the most-used
> items that a crate invites you to import all at once with a glob (`use … ::*`).
> Firefly's high-frequency surface — plus every macro — comes in through a single
> line:
>
> ```rust,ignore
> use firefly::prelude::*;
> ```
>
> That one import gives Lumen the CQRS `Bus`, the dependency-injection `Container`,
> the `Scheduler`, the `Saga`/`Step` orchestration types, the lifecycle
> `Application`, the reactive `Mono`/`Flux`, the `WebResult`/`WebError` web types,
> the `FireflyError` kernel error, and every `#[derive(...)]` / `#[...]` macro the
> service uses. Spring developers will recognize the move: one import instead of a
> page of them.

Lumen takes the discipline one step further. Even its typed error enums —
`MoneyError`, `DomainError`, and the `CqrsError` mapping — hand-write `Display`
and `std::error::Error` instead of reaching for `thiserror`. The one-dependency
promise holds end to end, and the chapters point it out where it matters.

> **Design note.** The `firefly` facade is a single front-door crate: one
> coordinate on your dependency list pulls in a curated, calendar-version-aligned
> stack, and `use firefly::prelude::*;` brings the whole high-frequency surface and
> every macro into scope at once. Many frameworks make you assemble a constellation
> of starter or plugin artifacts and keep their versions aligned by hand. Firefly
> collapses all of that into one line: there is no starter to forget and no version
> skew between subsystems like `firefly-web` and `firefly-cqrs`, because every
> `firefly-*` crate ships as one calendar-versioned release — here `26.6.28` — and
> you depend on the facade.

> **Tip** **Checkpoint.** You can point at the single Firefly line in a real
> `Cargo.toml` and explain the other entries as the handful of ecosystem crates a
> Firefly service writes against directly. Exercise 1 has you confirm this against
> `samples/lumen/Cargo.toml` yourself.

## Step 4 — Map the tiers behind the facade

Behind that single crate the framework is organized into strictly-layered tiers,
with a left-to-right dependency direction. Each tier may depend on the tiers to
its left, never to its right; the Cargo crate graph enforces the layering. You
rarely name these crates directly — the facade re-exports them — but knowing the
shape tells you where each capability lives, and *which* book chapter unlocks it.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 420" role="img"
     aria-label="Four-tier architecture: the firefly facade is the front door; below it Foundational, Platform, Adapters and Starters tiers build left to right, each depending on the tiers to its left, all resting on the firefly-reactive Mono/Flux core"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<rect x="120.0" y="18.5" width="320.0" height="46.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="120.0" y="16.0" width="320.0" height="46.0" rx="9" fill="#fff1dd" stroke="#855414" stroke-width="1.5"/><text x="280.0" y="36.0" text-anchor="middle" font-size="14" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">firefly + firefly-macros</text><text x="280.0" y="50.0" text-anchor="middle" font-size="10.0" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">one dependency · use firefly::prelude::*;</text>
<rect x="24" y="82" width="124" height="206" rx="11" fill="#e9e7df" stroke="#d8d4ca" stroke-width="1.2"/>
<rect x="24" y="82" width="124" height="34" rx="11" fill="#ffb34a" opacity="0.30"/>
<text x="86.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#855414" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 1</text>
<text x="86.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#272820" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Foundational</text>
<text x="86.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">kernel</text>
<text x="86.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">reactive</text>
<text x="86.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">web</text>
<text x="86.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">config</text>
<text x="86.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">container</text>
<text x="86.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">i18n</text>
<line x1="86.0" y1="62.0" x2="86.0" y2="72.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="86.0,80.0 81.5,72.0 90.5,72.0" fill="#855414"/>
<line x1="148.0" y1="200.0" x2="152.0" y2="200.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="160.0,200.0 152.0,204.5 152.0,195.5" fill="#855414"/>
<rect x="160" y="82" width="124" height="206" rx="11" fill="#e9e7df" stroke="#d8d4ca" stroke-width="1.2"/>
<rect x="160" y="82" width="124" height="34" rx="11" fill="#ffb34a" opacity="0.30"/>
<text x="222.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#855414" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 2</text>
<text x="222.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#272820" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Platform</text>
<text x="222.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cqrs</text>
<text x="222.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">eda</text>
<text x="222.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">event-sourcing</text>
<text x="222.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">orchestration</text>
<text x="222.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cache</text>
<text x="222.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">security</text>
<line x1="222.0" y1="62.0" x2="222.0" y2="72.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="222.0,80.0 217.5,72.0 226.5,72.0" fill="#855414"/>
<line x1="284.0" y1="200.0" x2="288.0" y2="200.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="296.0,200.0 288.0,204.5 288.0,195.5" fill="#855414"/>
<rect x="296" y="82" width="124" height="206" rx="11" fill="#e9e7df" stroke="#d8d4ca" stroke-width="1.2"/>
<rect x="296" y="82" width="124" height="34" rx="11" fill="#ffb34a" opacity="0.30"/>
<text x="358.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#855414" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 3</text>
<text x="358.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#272820" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Adapters</text>
<text x="358.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">data-sqlx</text>
<text x="358.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">data-mongodb</text>
<text x="358.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">eda-kafka</text>
<text x="358.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cache-redis</text>
<text x="358.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">idp-*</text>
<text x="358.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">notif-*</text>
<line x1="358.0" y1="62.0" x2="358.0" y2="72.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="358.0,80.0 353.5,72.0 362.5,72.0" fill="#855414"/>
<line x1="420.0" y1="200.0" x2="424.0" y2="200.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="432.0,200.0 424.0,204.5 424.0,195.5" fill="#855414"/>
<rect x="432" y="82" width="124" height="206" rx="11" fill="#e9e7df" stroke="#d8d4ca" stroke-width="1.2"/>
<rect x="432" y="82" width="124" height="34" rx="11" fill="#ffb34a" opacity="0.30"/>
<text x="494.0" y="100.0" text-anchor="middle" font-size="10.5" font-weight="800" fill="#855414" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Tier 4</text>
<text x="494.0" y="132.0" text-anchor="middle" font-size="12" font-weight="700" fill="#272820" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Starters</text>
<text x="494.0" y="152.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-core</text>
<text x="494.0" y="173.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-web</text>
<text x="494.0" y="194.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-domain</text>
<text x="494.0" y="215.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">starter-data</text>
<text x="494.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">admin</text>
<text x="494.0" y="257.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#62645b" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">cli</text>
<line x1="494.0" y1="62.0" x2="494.0" y2="72.0" stroke="#ffb34a" stroke-width="2.5" stroke-linecap="round"/><polygon points="494.0,80.0 489.5,72.0 498.5,72.0" fill="#855414"/>
<rect x="80.0" y="306.5" width="400.0" height="44.0" rx="9" fill="#bfb8ab" opacity="0.22"/><rect x="80.0" y="304.0" width="400.0" height="44.0" rx="9" fill="#f3f1eb" stroke="#d8d4ca" stroke-width="1.5"/><text x="280.0" y="323.0" text-anchor="middle" font-size="14" font-weight="700" fill="#10110f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">firefly-reactive</text><text x="280.0" y="337.0" text-anchor="middle" font-size="10.0" fill="#62645b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">the Mono / Flux core every tier rests on (tokio · axum)</text>
<svg x="480.0" y="364.0" width="72" height="52" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 335.52 242"><title>rsfly - Rust framework by Firefly</title><g fill="#10110f" transform="translate(20 130) scale(0.07 -0.07)"><g transform="translate(0 0)"><path d="M140 0L140 1080L299 1080L299 908Q310 927 323 945Q353 987 391 1014Q433 1048 486 1066Q539 1084 594 1087.5Q649 1091 697 1080L697 913Q641 928 574 921Q507 914 450 872Q398 835 369.5 782Q341 729 330 667.5Q319 606 319 542L319 0Z"/></g><g transform="translate(707 0)"><path d="M548 -29Q355 -29 231.5 55.5Q108 140 80 292L262 321Q284 233 363 181.5Q442 130 559 130Q670 130 733 176Q796 222 796 301Q796 347 775 375.5Q754 404 688.5 429Q623 454 491 489Q348 527 266 568Q184 609 149 663Q114 717 114 794Q114 889 166 960.5Q218 1032 311.5 1071Q405 1110 529 1110Q652 1110 749 1071Q846 1032 905.5 960.5Q965 889 976 794L794 761Q781 848 710.5 898Q640 948 529 951Q424 955 358.5 914Q293 873 293 803Q293 764 317.5 736Q342 708 408.5 683Q475 658 602 626Q745 590 827.5 547.5Q910 505 945 447.5Q980 390 980 306Q980 150 865 60.5Q750 -29 548 -29Z"/></g><g transform="translate(1768 0)"><path d="M243 0L243 933L60 933L60 1080L243 1080L243 1139Q243 1181 247 1224.5Q251 1268 265 1309.5Q279 1351 311 1386Q348 1427 392 1444.5Q436 1462 481.5 1466Q527 1470 568 1470L705 1470L705 1322L578 1322Q499 1322 460 1283.5Q421 1245 421 1167L421 1080L954 1080L954 1470L1132 1470L1132 0L954 0L954 933L421 933L421 0Z"/></g><g transform="translate(3060 0)"><g data-family-terminal="trail-y"><defs><linearGradient id="01-why-firefly-rsfly-family-rsfly-20-130-140" gradientUnits="userSpaceOnUse" x1="30" y1="1080" x2="554" y2="230"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M290 -480L468 4L30 1080L217 1080L554 230L870 1080L1051 1080L463 -480Z" fill="url(#01-why-firefly-rsfly-family-rsfly-20-130-140)"/></g></g></g><g fill="#62645b" transform="translate(25 210) scale(0.0155 -0.0155)"><path d="M625 -30Q476 -30 373 46Q314 89 271 150L271 0L139 0L139 1440L287 1440L287 953Q325 1001 375 1036Q479 1110 631 1110Q779 1110 885 1035Q991 960 1047.5 831Q1104 702 1104 541Q1104 379 1047 250Q990 121 883 45.5Q776 -30 625 -30ZM610 107Q723 107 798 164Q873 221 910.5 319.5Q948 418 948 541Q948 663 911 761Q874 859 798.5 916Q723 973 607 973Q494 973 419.5 918Q345 863 308 765.5Q271 668 271 541Q271 416 308 317.5Q345 219 420 163Q495 107 610 107Z" transform="translate(0 0)"/><path d="M294 -480L472 3L33 1080L189 1080L544 192L871 1080L1021 1080L441 -480Z" transform="translate(1131 0)"/></g><svg x="76" y="183" width="143" height="45.48" xmlns="http://www.w3.org/2000/svg" viewBox="5 -148 619.4 197"><defs><linearGradient id="01-why-firefly-rsfly-firefly-trail" gradientUnits="userSpaceOnUse" x1="567.4" y1="-108" x2="623.4" y2="-54"><stop offset="0" stop-color="#ffb34a" stop-opacity="0.2"/><stop offset="0.8" stop-color="#ffb34a" stop-opacity="1"/></linearGradient></defs><path d="M24.3 0L24.3 -93.3L6 -93.3L6 -108L24.3 -108L24.3 -113.9Q24.3 -118.1 24.7 -122.45Q25.1 -126.8 26.5 -130.95Q27.9 -135.1 31.1 -138.6Q34.8 -142.7 39.2 -144.45Q43.6 -146.2 48.15 -146.6Q52.7 -147 56.8 -147L70.5 -147L70.5 -132.2L57.8 -132.2Q49.9 -132.2 46 -128.35Q42.1 -124.5 42.1 -116.7L42.1 -108L70.5 -108L70.5 -93.3L42.1 -93.3L42.1 0ZM91.5 0L91.5 -108L109.3 -108L109.3 0ZM134.8 0L134.8 -108L150.7 -108L150.7 -90.8Q151.8 -92.7 153.1 -94.5Q156.1 -98.7 159.9 -101.4Q164.1 -104.8 169.4 -106.6Q174.7 -108.4 180.2 -108.75Q185.7 -109.1 190.5 -108L190.5 -91.3Q184.9 -92.8 178.2 -92.1Q171.5 -91.4 165.8 -87.2Q160.6 -83.5 157.75 -78.2Q154.9 -72.9 153.8 -66.75Q152.7 -60.6 152.7 -54.2L152.7 0ZM247.3 3Q231.4 3 219.55 -4.05Q207.7 -11.1 201.1 -23.75Q194.5 -36.4 194.5 -53.3Q194.5 -71 201 -83.95Q207.5 -96.9 219.15 -103.95Q230.8 -111 246.5 -111Q262.8 -111 274.25 -103.5Q285.7 -96 291.45 -82.1Q297.2 -68.2 296.4 -49L213.6 -49Q214.6 -33.5 222 -24.4Q230.6 -13.9 246.5 -13.9Q257.2 -13.9 265.1 -18.8Q273 -23.7 277.5 -32.9L294.6 -27Q288.3 -12.7 275.65 -4.85Q263 3 247.3 3ZM277.9 -63.3Q276.5 -77.4 270.3 -85.2Q262.5 -95 247.1 -95Q230.6 -95 222 -84.4Q215.5 -76.4 213.9 -63.3ZM324.4 0L324.4 -93.3L306.1 -93.3L306.1 -108L324.4 -108L324.4 -113.9Q324.4 -118.1 324.8 -122.45Q325.2 -126.8 326.6 -130.95Q328 -135.1 331.2 -138.6Q334.9 -142.7 339.3 -144.45Q343.7 -146.2 348.25 -146.6Q352.8 -147 356.9 -147L370.6 -147L370.6 -132.2L357.9 -132.2Q350 -132.2 346.1 -128.35Q342.2 -124.5 342.2 -116.7L342.2 -108L395.5 -108L395.5 -147L413.3 -147L413.3 0L395.5 0L395.5 -93.3L342.2 -93.3L342.2 0ZM452.8 48L470.6 -0.4L426.8 -108L445.5 -108L479.2 -23L510.8 -108L528.9 -108L470.1 48Z" fill="#10110f"/><circle cx="100.4" cy="-136.3" r="9.97" fill="#ffb34a"/><path d="M567.4 -108L623.4 -54L603.4 -54L547.4 -108Z" fill="url(#01-why-firefly-rsfly-firefly-trail)"/><path d="M623.4 -54L567.4 0L547.4 0L603.4 -54Z" fill="#ffb34a"/></svg></svg></svg>
<figcaption>The four tiers. A service depends only on the <code>firefly</code> facade (the front door). The tiers build left to right — <strong>Foundational</strong> vocabulary, <strong>Platform</strong> engines that define ports, <strong>Adapters</strong> that implement them, <strong>Starters</strong> that compose and ship — each depending only on the tiers to its left, all resting on the <code>firefly-reactive</code> core.</figcaption>
</figure>

A service depends only on the `firefly` facade (the front door). The four tiers
build left to right — each depending only on the tiers to its left — all resting
on the `firefly-reactive` `Mono`/`Flux` core.

- **Foundational** crates are the vocabulary: `firefly-kernel` (errors, clock,
  correlation scopes, the DDD kit), `firefly-reactive` (`Mono`/`Flux`),
  `firefly-web` (middleware), `firefly-config`, `firefly-validators`,
  `firefly-i18n`, and `firefly-container` — a full dependency-injection engine
  with component scanning and stereotype derives, covered in depth in
  [Dependency Wiring](./04-dependency-wiring.md).
- **Platform** crates are the capabilities: caching, Command/Query Responsibility
  Segregation, event-driven architecture, event sourcing, orchestration,
  scheduling, resilience, security, observability. Lumen reaches for
  `firefly::cqrs`, `firefly::eventsourcing`, `firefly::orchestration`,
  `firefly::scheduling`, and `firefly::security`. Crucially, this tier *defines
  the ports* — the `EventStore`, `Broker`, `cache::Adapter`, and
  `security::Verifier` traits — that the next tier implements.
- **Adapters** are the concrete integrations: the REST/reactive HTTP client, the
  identity-provider vendors, content stores, notifications, the event transports
  (Kafka, RabbitMQ, Postgres outbox, Redis Streams), and the persistence adapters
  — `firefly-data-sqlx` for relational stores, `firefly-data-mongodb` for
  documents. This is a pluggable multi-database story that
  [Persistence](./07-persistence.md) builds on. Lumen ships on the in-memory
  adapters and points at the production swaps in callouts.
- **Starters** bundle a sensible default stack so a service depends on one crate.
  Lumen's web tier is `firefly::starter_web::WebStack`, which wires the core
  (`firefly::starter_core`) plus the web middleware — the stack `FireflyApplication`
  builds for you at boot.

> **Note** **Key term — actuator / management surface.** The *management surface*
> is a set of operational HTTP endpoints — health checks, build info, metrics,
> configuration and bean introspection — that exist for operators and tooling, not
> for end users. Firefly serves them on a *separate* port from your business API,
> so operational endpoints never leak onto the public network. This mirrors Spring
> Boot Actuator, and you reach it for the first time in
> [Quickstart](./02-quickstart.md).

For the full per-crate catalogue see the
[Module Index](./91-appendix-modules.md).

> **Tip** **Checkpoint.** Given a capability — "where does event storage live?" —
> you can place it in a tier (it is a *platform* port, implemented by an *adapter*)
> and name the chapter that introduces it. The tiers are a map; the rest of the
> book is a tour of it.

## Step 5 — Understand the adapter swap

This is the single design choice that the whole book turns on, so it earns its own
step. Lumen runs with **zero external infrastructure** — that is what makes it a
good teaching baseline and a fast test target. It boots on the in-process
`MemoryEventStore` and the in-process broker, so `cargo run` and `cargo test` need
nothing but the crate. No Postgres to start, no Kafka to provision.

When you are ready for production, you change the *wiring*, not the handlers. Each
of the swaps below is a one-place edit at the seam where the `Arc<dyn Port>` is
constructed:

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

> **Note** **Key term — `#[bean]` factory.** A `#[bean]` factory is a function the
> framework calls at startup to *construct* a bean — and where you decide which
> concrete adapter satisfies a port. It is the single place the swap above happens:
> the function body returns `Arc::new(MemoryEventStore::new())` in development and
> `Arc::new(SqlEventStore::new(pool))` in production, and nothing downstream
> notices. The Spring analog is an `@Bean` method on a `@Configuration` class. You
> write your first one in [Dependency Wiring](./04-dependency-wiring.md).

What just happened: you saw why the in-memory baseline is not a toy. Because Lumen
codes against ports, the in-memory build and the production deployment differ
*only* in a `#[bean]` factory — the wiring the framework scans, not the business
code. This is the thread that runs through the whole book.

> **Tip** **Checkpoint.** You can finish this sentence: *to take Lumen to
> production you change a `#[bean]` factory, not a handler.* If that lands, the
> book's "Lumen ships on in-memory; here is the production swap" callouts will read
> as routine rather than magical. Exercise 4 has you locate the three port traits
> behind these swaps.

## The road ahead: Lumen, chapter by chapter

The rest of the book is Lumen's growth, additive and in order. The early chapters
introduce the framework with small standalone snippets; **Lumen proper begins in
[Your First HTTP API](./06-first-http-api.md)**.

- **Foundations** — scaffold and boot Lumen, bind its configuration and profiles,
  understand how `FireflyApplication` wires the beans it scans, master
  `Mono`/`Flux`, and expose the first validated REST endpoints.
- **Modeling and persisting** — a read model behind a repository, the `Money`
  value object and the `Wallet` aggregate, and the CQRS command/query split on a
  bus.
- **Event-driven** — domain events, a projection that keeps the read model
  current, and the event-sourced ledger that folds its stream.
- **Into microservices** — an HTTP-client sketch and the compensating transfer
  saga.
- **Secure, observe, ship** — JWT bearer auth and role-based access control, the
  actuator surface, caching, a scheduled task, the test suite, and the production
  entry point with graceful shutdown and a reactive streaming endpoint.

By the last page, Lumen is the complete `samples/lumen` crate — and you have
written every line of it.

## Recap — what changed in Lumen

Nothing in code yet. This chapter framed the journey and stocked your vocabulary:

- The **cohesion problem** Firefly exists to solve — Rust offers infinite choice
  but no built-in cohesion — and the framework-vs-library inversion that lets one
  opinionated framework supply it.
- What **Firefly is** (a cohesive, reactive, async-native framework) and what it
  *delegates to* (tokio, axum/tower, serde, tracing, RustCrypto), with you
  depending on its **ports** and selecting **adapters** at wiring time.
- The **one-dependency facade** — Lumen depends on a single
  `firefly = { version = "26.6.28", features = ["admin"] }`, and
  `use firefly::prelude::*;` brings in the whole high-frequency surface and every
  macro. Even the typed errors avoid `thiserror`, so the promise holds end to end.
- The **four tiers** behind that facade (foundational → platform → adapters →
  starters) resting on the `firefly-reactive` core, and where each capability
  lives.
- The **adapter swap** that Lumen is built to make — moving from the in-memory
  baseline to production by changing a single `#[bean]` factory, never a handler.

## Exercises

1. **Confirm the one dependency.** Open `samples/lumen/Cargo.toml` and confirm the
   dependency list: one `firefly` (with the `admin` feature), plus
   `axum`/`serde`/`serde_json`/`tokio`/`uuid`/`chrono`/`async-trait`. Note that no
   `firefly-*` sub-crate is listed directly.
2. **Find the single-line `main`.** Skim `samples/lumen/src/main.rs` — the
   single-binary crate root. List the ten modules it declares (`commands`,
   `compliance`, `domain`, `housekeeping`, `ledger`, `money`, `security`,
   `tcc_transfer`, `transfer`, `web`) and predict which book part introduces each.
   Confirm that `main` is genuinely one line over `FireflyApplication::new("lumen")`.
3. **Read the crate docs.** Run `cargo doc -p firefly-sample-lumen --open` and read
   the crate-level documentation. It contains the same "building block → module →
   Firefly surface" table the book is organized around.
4. **Locate the port traits.** For each of these production swaps, find the port
   trait it would implement in the facade: a Postgres event store, a Kafka broker,
   a Redis cache. (Hint: `firefly::eventsourcing::EventStore`, `firefly::eda::Broker`,
   `firefly::cache::Adapter`.) These are the seams Step 5 described.
5. **Trace the prelude.** Open the `firefly` facade's `prelude` module (or its
   docs) and find five types you will use repeatedly: the CQRS `Bus`, the
   `Container`, `Mono`/`Flux`, `WebResult`, and `FireflyError`. Confirm they all
   arrive through the single `use firefly::prelude::*;` glob.

## Where to go next

- Get Lumen running for the first time in **[Quickstart](./02-quickstart.md)** —
  scaffold the crate, write the one-line `main`, and reach its two ports.
- Add typed, layered, profile-aware configuration in
  **[Configuration](./03-configuration.md)**.
- Learn how the framework wires the object graph it scans — including your first
  `#[bean]` factory — in **[Dependency Wiring](./04-dependency-wiring.md)**.
