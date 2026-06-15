<!--
Copyright 2026 Firefly Software Foundation.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0
-->

# Lumen — the chapter-by-chapter build arc

**Lumen** is the digital-wallet & ledger service the book grows one chapter at
a time — the Rust analog of pyfly's "Lumen". Every book listing is a slice of
the real, compiling, tested crate at [`samples/lumen`](../../samples/lumen),
so prose is verified against code (pyfly's guarantee). This document is the map
the chapter authors follow: for each chapter it says **what code is added**,
**which Lumen files** it lands in, and **the key macros / APIs** it exercises.

> **One dependency.** Lumen depends on exactly one Firefly crate — the
> [`firefly`](../../crates/firefly) facade — plus the two ecosystem crates any
> Rust service writes against directly (`axum`, `serde`/`serde_json`). The
> whole framework and every `#[derive(...)]` / `#[...]` macro arrive through
> `use firefly::prelude::*;`. Even the typed error enums avoid `thiserror` (they
> hand-write `Display` + `std::error::Error`) so the promise holds end to end.
> The chapters make a point of this.

## The finished shape (what every chapter is building toward)

```
samples/lumen/
├── Cargo.toml            # one dep: firefly; + axum/serde/tokio/uuid/chrono; feature `streaming`
├── src/
│   ├── main.rs           # ONE-LINE entry point: `FireflyApplication::new("lumen").run().await`
│   ├── money.rs          # Money value object (immutable, cents, exact)
│   ├── domain.rs         # Wallet aggregate + DomainEvent payloads + #[derive(Schema)] WalletView/WalletEvent
│   ├── ledger.rs         # event-sourced Ledger service + ReadModel + WalletProjection bean (#[derive(Service)] + #[handlers]/#[event_listener])
│   ├── commands.rs       # CQRS messages (#[derive(Command/Query/Schema)]) + WalletHandlers bean (#[derive(Service)] + #[handlers]/#[command_handler]/#[query_handler])
│   ├── transfer.rs       # Transfer saga (#[saga]) + #[derive(Schema)] TransferRequest/TransferResult
│   ├── compliance.rs     # Compliance workflow (#[workflow] / #[workflow_step])
│   ├── tcc_transfer.rs   # Two-phase transfer (#[tcc] / #[participant]) + #[derive(Schema)] TccTransferResult
│   ├── security.rs       # JWT mint/verify + BearerLayer + RBAC FilterChain (declared as #[bean]s in web.rs)
│   ├── web.rs            # declarative beans (LumenBeans #[derive(Configuration)] + #[bean]) +
│   │                     #   the #[rest_controller(tag="Wallets")] WalletApi + a test-only build_router()
│   └── housekeeping.rs   # #[scheduled] heartbeat task
└── (HTTP/streaming tests live in `src/http_test.rs` / `src/streaming_test.rs`,
    driving the framework-assembled `build_router()` in-process — no socket)
```

> **No composition root.** `main.rs` is **one line** over `FireflyApplication`.
> `web.rs` is *not* a `build_app`/`build_router` composition root any more: it is
> a `LumenBeans` `#[derive(Configuration)]` whose `#[bean]` factories declare the
> domain beans, an annotated `#[rest_controller]` `WalletApi`, and a single
> `#[cfg(test)] build_router()` that just calls
> `FireflyApplication::new(..).bootstrap().await.api_router` so the HTTP tests
> drive the same app `main` serves. The framework component-scans the beans,
> auto-mounts the controller, auto-discovers security, drains the inventory
> handlers/listeners/tasks, and serves OpenAPI + admin — see
> [`04b-bootstrap.md`](src/04b-bootstrap.md).

## Endpoints (the contract the chapters converge on)

| Method & path                          | Handler                       | Engine                       |
|----------------------------------------|-------------------------------|------------------------------|
| `POST /api/v1/wallets`                 | `WalletApi::open`             | CQRS `OpenWallet` → 201       |
| `GET  /api/v1/wallets/:id`             | `WalletApi::get`              | CQRS `GetWallet` (cached 30s) |
| `POST /api/v1/wallets/:id/deposit`     | `WalletApi::deposit`          | CQRS `Deposit`                |
| `POST /api/v1/wallets/:id/withdraw`    | `WalletApi::withdraw`         | CQRS `Withdraw`               |
| `POST /api/v1/transfers`               | `WalletApi::transfer`         | Saga (debit → credit)         |
| `POST /api/v1/transfers/compliance`    | `WalletApi::transfer_compliance` | Workflow (parallel checks) |
| `POST /api/v1/transfers/2pc`           | `WalletApi::transfer_2pc`     | TCC (reserve → capture)       |
| `GET  /api/v1/wallets/:id/events`      | `stream_events`               | reactive `Flux` → NDJSON/SSE (feature `streaming`) |
| `GET  /v3/api-docs` · `/swagger-ui` · `/redoc` | OpenAPI generator     | live spec from the inventory (auto-served) |
| `GET  /actuator/*` · `/admin/`         | actuator + admin router       | health / info / metrics / mappings / beans + dashboard |

The mutating routes require a `CUSTOMER` JWT; the `GET` reads, the OpenAPI docs,
and the actuator/admin surface are public. The API serves on
`FIREFLY_SERVER_ADDR` (default `0.0.0.0:8080`); the actuator + admin serve on
`FIREFLY_MANAGEMENT_ADDR` (default `0.0.0.0:8081`).

---

## The arc

Chapter numbers below are the **book's** chapter numbers (see
`docs/book/src/SUMMARY.md`). Early chapters (1–5) introduce the framework with
tiny standalone snippets; **Lumen proper begins in chapter 6** and grows from
there. Each chapter is *additive* — it never rewrites what an earlier chapter
shipped, it extends it — so the final state is exactly the `samples/lumen`
crate.

### Ch 1 — Why Firefly for Rust (`01-why-firefly.md`)
- **Adds:** nothing in code yet; frames the one-dependency facade and the
  "declaration next to the code" philosophy Lumen embodies.
- **Files:** —
- **APIs/macros:** the `firefly` facade pitch; `firefly = { ... }` as the sole
  Firefly dependency.

### Ch 2 — Quickstart (`02-quickstart.md`)
- **Adds:** the Lumen **scaffold** — `Cargo.toml` with the single `firefly`
  dependency, the module tree, and the **one-line `main`**:
  `firefly::FireflyApplication::new("lumen").run().await`. No composition root,
  no banner plumbing — the framework prints the banner, the docs URLs, and the
  startup report.
- **Files:** `Cargo.toml`, `src/main.rs` (the one-line `main` + module decls).
- **APIs/macros:** `firefly::FireflyApplication::{new, run}`, `firefly::BoxError`,
  `firefly::VERSION`, `use firefly::prelude::*;`.

### Ch 2.5 — Bootstrapping with FireflyApplication (`04b-bootstrap.md`)
- **Adds:** the explanation of the one-line `main` — the full `run` pipeline
  (build web stack → scan container → auto-configure CQRS bus → discover security
  → auto-mount controllers + route contributors → drain inventory
  handlers/listeners/`#[scheduled]` → apply middleware + W3C trace → serve
  OpenAPI + admin → print startup report → serve both ports with graceful
  shutdown), the builder knobs, the line-by-line startup report, the
  env-overridable binds, and the default RFC 9457 404.
- **Files:** `src/main.rs` (one-line `main`), `src/web.rs` (`build_router` for
  the in-process tests).
- **APIs/macros:** `FireflyApplication::{new, version, configure, security,
  on_ready, extra_routes, info_contributor, api_addr, management_addr, bootstrap,
  run}`, `Bootstrapped::{api_router, serve}`, `FIREFLY_SERVER_ADDR` /
  `FIREFLY_MANAGEMENT_ADDR`.

### Ch 3 — Configuration (`03-configuration.md`)
- **Adds:** `CoreConfig` fields (`app_name`, `app_version`) that name Lumen and
  feed `/actuator/info` — set via `FireflyApplication::new("lumen").version(..)`
  (or `.configure(|cfg| ..)`); and the env-overridable bind addresses
  `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` that `FireflyApplication`
  honours (default `0.0.0.0:8080` / `0.0.0.0:8081`).
- **Files:** `src/web.rs` (`APP_NAME` / `VERSION` consts), `src/main.rs`.
- **APIs/macros:** `FireflyApplication::{version, configure, api_addr,
  management_addr}`, `CoreConfig`, profile/property knobs; (forward-reference to
  `#[derive(ConfigProperties)]` for a reader exercise).

### Ch 4 — Dependency Wiring (`04-dependency-wiring.md`) + DI & Auto-Configuration (`04a-dependency-injection.md`)
- **Adds:** Lumen's real wiring model — **declarative beans**, not a composition
  root. `LumenBeans` (`#[derive(Configuration)]`) declares the domain beans
  (event store, read model, query cache, JWT service, `FilterChain`,
  `BearerLayer`, ledger) as `#[bean]` factories; the `WalletApi` controller is a
  `#[derive(Controller)]` bean whose `Arc<Bus>` / `Arc<Ledger>` /
  `Arc<QueryCache>` collaborators are `#[autowired]`. The framework
  `container.scan()`s them — no `register_arc`, no `build_app`. `04a` walks the
  full DI surface (stereotypes, `#[autowired]`, `#[bean]`, primary, profiles,
  conditions, lifecycle) and the auto-configuration the bootstrap performs.
- **Files:** `src/web.rs` (`LumenBeans` + `#[bean]` factories, the autowired
  `WalletApi`).
- **APIs/macros:** `Container::{scan, resolve, beans}`,
  `#[derive(Configuration/Controller/Component/Service/Repository)]`, `#[bean]`,
  `#[autowired]`, `#[firefly(provides = "dyn ..")]` (the `RouteContributor`
  bean).

### Ch 5 — The Reactive Model — Mono & Flux (`05-reactive-model.md`)
- **Adds:** the reactive primitives Lumen leans on later — `Mono`/`Flux`. No
  Lumen file yet; the chapter previews how `Flux::just`/`Flux::from_stream`
  back the streaming endpoint and how `Bus::send_mono` returns a `Mono`.
- **Files:** — (forward-references `src/web.rs` streaming).
- **APIs/macros:** `firefly::reactive::{Mono, Flux}`.

### Ch 6 — Your First HTTP API (`06-first-http-api.md`)
- **Adds:** the **first real endpoints**. The `WalletApi` controller with
  `POST /api/v1/wallets` and `GET /api/v1/wallets/:id`, returning a
  `WalletView`. The controller is a `#[rest_controller]` macro target — the
  framework **auto-mounts** it (no hand-built router); each handler is a plain
  axum handler returning `WebResult<T>`.
- **Files:** `src/web.rs` (`WalletApi`, `#[rest_controller]`, `open`/`get`),
  early `WalletView`, `src/http_test.rs` (first `oneshot` round-trip over
  `build_router()`).
- **APIs/macros:** `#[rest_controller(path = "/api/v1", tag = "Wallets")]`,
  `#[get]`/`#[post]`, `WebResult<T>`, `WebError`,
  `FireflyError::{not_found, validation}`, RFC 9457 problem rendering;
  `tower::ServiceExt::oneshot`.

### Ch 6.5 — OpenAPI, Swagger UI & ReDoc (`06a-openapi.md`)
- **Adds:** how the OpenAPI 3.1 spec is generated from the **live inventory**
  (every `#[rest_controller]` route + every `#[derive(Schema)]` DTO) and served
  with **no app code** at `/v3/api-docs` (+ `/openapi.json`), `/swagger-ui`,
  `/redoc`. Request/response models are **inferred** from the handler signature
  (the `Json<T>` parameter / return); `#[derive(Schema)]` computes the JSON
  schema at compile time, honouring serde `rename`/`rename_all`/`skip`;
  per-operation `summary`/`description`/`tags`/`status`/`deprecated`/`request=`/
  `response=` and `#[rest_controller(tag)]` enrich the operations. Uses the real
  `WalletApi` as the worked example; covers the `firefly openapi` CLI export.
- **Files:** `src/web.rs` (the annotated `WalletApi` + its `#[derive(Schema)]`
  DTOs), `src/domain.rs` / `src/commands.rs` / `src/transfer.rs` /
  `src/tcc_transfer.rs` (the `#[derive(Schema)]` types).
- **APIs/macros:** `#[derive(Schema)]`, `#[rest_controller(tag = ..)]`, the verb
  `summary`/`description`/`tags`/`status`/`deprecated`/`request=`/`response=`
  args; `firefly_openapi::{Builder, Info, DocsConfig}`, `from_inventory`,
  `docs_router`; `firefly openapi --format json|yaml`.

### Ch 7 — Persistence & Reactive Repositories (`07-persistence.md`)
- **Adds:** the **read model** — `ReadModel` as the query-side store the
  `GetWallet` query reads from, framed as a repository. Shows the in-memory
  baseline and the reactive-repository / SQL upgrade path (the book's
  "swap the adapter" callout).
- **Files:** `src/ledger.rs` (`ReadModel`).
- **APIs/macros:** repository pattern; `firefly::data` reactive repos as the
  production upgrade; in-memory `Mutex<HashMap>` baseline.

### Ch 8 — Domain-Driven Design (`08-domain-driven-design.md`)
- **Adds:** the **DDD core** — the `Money` **value object** (immutable, cents,
  exact arithmetic) and the `Wallet` **aggregate** with `open` / `deposit` /
  `withdraw` enforcing invariants (positive amounts, sufficient funds, owner
  required), and the typed `DomainError` family.
- **Files:** `src/money.rs` (whole file), `src/domain.rs` (`Wallet`,
  `DomainError`, command methods, `view()`).
- **APIs/macros:** `#[derive(AggregateRoot)]` (+ `AGGREGATE_TYPE`,
  `aggregate()`/`aggregate_mut()`), the embedded `AggregateRoot`; hand-written
  `Display`/`Error` (the no-`thiserror` callout).

### Ch 9 — CQRS (`09-cqrs.md`)
- **Adds:** the **command/query split**. The `OpenWallet` / `Deposit` /
  `Withdraw` commands and the `GetWallet` query, plus the **handler bean** —
  `WalletHandlers`, a `#[derive(Service)]` whose `Ledger` + `ReadModel`
  collaborators are `#[autowired]`. `#[handlers]` (an impl-level attribute on the
  bean's `impl`) registers each `#[command_handler]` / `#[query_handler]` method
  on the bus: the macro submits a `BeanHandlerRegistration` to the inventory that
  resolves the bean from the container, and `FireflyApplication` **drains the
  registry** (`register_discovered_handlers`) at boot. So the handler reaches its
  collaborators through `self` — **no process-global, no `OnceLock`/`bind`, no
  composition root** (the Rust analog of a Spring `@Component` command handler).
  Adds the read-after-write cache invalidation. The `QueryCache` bean's read-cache
  middleware is auto-configured onto the bus whenever the bean is present.
- **Files:** `src/commands.rs` (whole file — messages + the `WalletHandlers`
  bean), `src/web.rs` (the controller dispatches through the autowired `Bus`,
  `query_cache.invalidate_type::<GetWallet>()`).
- **APIs/macros:** `#[derive(Command)]` / `#[derive(Query)]` (with
  `#[firefly(validate)]` and `#[firefly(cache_ttl = "30s")]`), `#[derive(Service)]`
  + `#[autowired]` (the handler bean), `#[handlers]` + `#[command_handler]` /
  `#[query_handler]` method markers (drained from the inventory at boot),
  `Bus::{send, query}`, `ValidationMiddleware`, `QueryCache`, `CqrsError`.

### Ch 10 — Event-Driven Architecture & Messaging (`10-eda-messaging.md`)
- **Adds:** the **domain events on the wire** — the `Ledger` publishes each
  persisted event to the framework-provided `Broker`, and the **read-model
  projection bean** consumes them to keep `ReadModel` current (the CQRS loop
  closes here). The projection is `WalletProjection`, a `#[derive(Service)]` that
  `#[autowired]`s the `Ledger` (for the event store it replays) and the
  `ReadModel` it feeds; `#[handlers]` subscribes its `#[event_listener]` method to
  the topic. It is **not** subscribed by a hand-written `subscribe(&broker)` call,
  nor seeded inside the `ledger` `#[bean]` — `#[handlers]` submits a
  `BeanListenerRegistration` to the inventory that resolves the bean from the
  container, and `FireflyApplication` **drains the registry**
  (`subscribe_discovered_listeners`) at boot. So the projection is autowired and
  wired entirely through the DI container — **no `OnceLock`/`bind`, no
  composition-root seeding**. Idempotent rebuild-from-stream projection.
- **Files:** `src/ledger.rs` (`Ledger::commit` publish step, `to_envelope`, the
  `WalletProjection` bean + its `#[handlers]`/`#[event_listener]` `project`
  method).
- **APIs/macros:** `#[derive(Service)]` + `#[autowired]` (the projection bean),
  `#[handlers]` + `#[event_listener(topic = "wallets.events")]` method marker
  (drained from the inventory at boot), `firefly::eda::{Broker, Event, handler,
  InMemoryBroker}`, `Event::{new, with_key, with_header}`, the Kafka/RabbitMQ
  adapter swap callout.

### Ch 11 — Event Sourcing (`11-event-sourcing.md`)
- **Adds:** the **event-sourced ledger** proper — the `EventStore`, optimistic
  concurrency on append, rehydration by folding the stream, and the
  `#[derive(DomainEvent)]` payloads (`WalletOpened` / `MoneyDeposited` /
  `MoneyWithdrawn`). This is where `Ledger::{open, deposit, withdraw, load,
  commit}` get their real (event-store-backed) bodies.
- **Files:** `src/domain.rs` (`#[derive(DomainEvent)]` payloads, `rehydrate`,
  `apply`), `src/ledger.rs` (`Ledger` over `EventStore`).
- **APIs/macros:** `#[derive(DomainEvent)]` (+ `EVENT_TYPE`,
  `to_domain_event`), `firefly::eventsourcing::{AggregateRoot, DomainEvent,
  EventStore, MemoryEventStore, EventSourcingError}`, optimistic-concurrency
  `append(id, expected_version, events)`.

### Ch 12 — Sagas, Workflows & TCC (`12-sagas.md`)
- **Adds:** the **Transfer saga** — a two-step distributed transaction
  (debit → credit) with compensation that refunds the debit when the credit
  leg fails. The `POST /api/v1/transfers` endpoint drives it. Shows the happy
  path, the overdraft short-circuit, and the credit-failure refund.
- **Files:** `src/transfer.rs` (whole file), `src/web.rs` (`transfer` handler).
- **APIs/macros:** `firefly::orchestration::{Saga, Step, SagaStatus, BoxError}`,
  `Step::with_compensation`, `Saga::run`, `Outcome::{steps_executed,
  steps_rolled}`; the Workflow/TCC engines as further-reading callouts.

### Ch 13 — HTTP Clients (`13-http-clients.md`)
- **Adds:** (callout-level for Lumen) how a wallet would call an external
  payments/FX provider with the `firefly::client` builder (REST, circuit
  breaker, retry). Lumen stays self-contained, so this is presented as the
  "next adapter you would add", with a sketch wired into the transfer flow.
- **Files:** — (sketch only; optional reader exercise extends `src/transfer.rs`).
- **APIs/macros:** `firefly::client` REST builder, resilience decorators.

### Ch 14 — Security (`14-security.md`)
- **Adds:** **JWT-secured endpoints**. HS256 mint + verify via the framework's
  `JwtService`, a `BearerLayer` resource-server verifier, and a path-based RBAC
  `FilterChain` that requires `CUSTOMER` on the mutating routes while leaving
  reads + the OpenAPI docs + actuator public. Both the `FilterChain` and the
  `BearerLayer` are declared as **`#[bean]`s** in `LumenBeans` — `FireflyApplication`
  **auto-discovers and applies** them at boot (no `.security(...)` call, no
  `router()` layering by hand).
- **Files:** `src/security.rs` (whole file), `src/web.rs` (`security_filter_chain`
  + `bearer_layer` `#[bean]` factories), `src/http_test.rs` (401/422 problem
  assertions).
- **APIs/macros:** `firefly::security::{JwtService, Verifier, VerifierFn,
  Authentication, SecurityError, BearerLayer, BearerConfig, FilterChain}` as
  scanned beans, `claims → Authentication`, RFC 9457 401 rendering;
  `JwksVerifier` as the production IdP swap.

### Ch 15 — Observability (`15-observability.md`)
- **Adds:** the **actuator + self-hosted admin surface**, served on the
  management port (`FIREFLY_MANAGEMENT_ADDR`) with **no app code** —
  `FireflyApplication` mounts `/actuator/*`
  (`health`/`info`/`metrics`/`loggers`/`mappings`/`beans`/`conditions`/`env`)
  and the `/admin/` dashboard, wired to the live components (health, metrics,
  the bus, the scheduler, the container, the environment snapshot, the trace +
  log buffers). Structured logging is initialised by the bootstrap (and teed
  into the admin log buffer); request metrics, correlation id, and originated
  W3C `traceparent` are on by default.
- **Files:** — (the surface is framework-provided; an optional `info_contributor`
  builder knob adds an `/actuator/info` block).
- **APIs/macros:** `FireflyApplication::info_contributor`,
  `firefly_starter_core::InfoContributor`; the self-hosted `firefly-admin`
  dashboard; `/admin/api/mappings` (the live route table shared with OpenAPI).

### Ch 16 — Scheduling & Notifications (`16-scheduling-notifications.md`)
- **Adds:** the **scheduled housekeeping** task — a `#[scheduled]` heartbeat. It
  is **not** registered or started from `main.rs` — it submits to the inventory,
  and `FireflyApplication` **drains the registry**
  (`register_discovered_scheduled`) and starts the scheduler on a background task
  at boot. Framed as where a daily-statement notification
  (`firefly::notifications`) would hang.
- **Files:** `src/housekeeping.rs` (whole file).
- **APIs/macros:** `#[scheduled(fixed_rate = "60s", initial_delay = "5s")]`
  (drained from the inventory at boot), `firefly::prelude::Scheduler` (started by
  `Bootstrapped::serve`); notifications adapters as a callout.

### Ch 17 — Caching (`17-caching.md`)
- **Adds:** deepens the **read-side cache** introduced with CQRS — the
  `GetWallet` `#[firefly(cache_ttl = "30s")]` and the `QueryCache` middleware,
  plus the read-after-write **invalidation** on every mutation
  (`invalidate_type::<GetWallet>()`). Discusses the unified cache abstraction
  and the Redis/Caffeine backends.
- **Files:** `src/commands.rs` (`GetWallet` ttl), `src/web.rs` (cache
  middleware + invalidation).
- **APIs/macros:** `firefly::cqrs::QueryCache`, `Message::cache_ttl`,
  `firefly::cache` adapters (Redis/Postgres) as backend swaps.

### Ch 18 — Testing (`18-testing.md`)
- **Adds:** the **test strategy** — unit tests per module (domain, money,
  ledger, saga, security, commands) and the end-to-end `tower::oneshot` HTTP
  suite covering the full flow (open → get → deposit/withdraw → transfer happy
  + compensation → projection convergence → auth/validation problems). Explains
  the in-process, no-socket testing model: each test boots **one** app context
  with `build_router()` and drives every request against it (Spring Boot's
  `@SpringBootTest` model). Because the handlers (`WalletHandlers`) and projection
  (`WalletProjection`) are autowired beans, one container's singletons stay
  consistent across a test's requests, so the wallet a command opens is the wallet
  a later query reads — no `OnceLock`/shared-global caveat.
- **Files:** `src/http_test.rs`, every `#[cfg(test)] mod tests` in `src/`. The
  HTTP suite drives the framework-assembled `build_router()`
  (`FireflyApplication::bootstrap().api_router`) in-process — the same app
  `main` serves, no socket. Helpers thread `&axum::Router` and `clone` it per
  request (`app.clone().oneshot(req)`).
- **APIs/macros:** `FireflyApplication::bootstrap` → `Bootstrapped::api_router`,
  `tower::ServiceExt::oneshot`, `http_body_util::BodyExt`, `firefly::testkit`
  helpers; `StepVerifier`/Testcontainers as production-grade callouts.

### Ch 19 — The CLI (`19-cli.md`)
- **Adds:** (tooling-level) how the `firefly` CLI scaffolds and runs a service
  like Lumen; not a Lumen source change. References `cargo run --bin lumen`, the
  `FIREFLY_SERVER_ADDR` / `FIREFLY_MANAGEMENT_ADDR` overrides, and `firefly
  openapi` (skeleton export; the live spec is served at `/v3/api-docs`).
- **Files:** — (operational).
- **APIs/macros:** `firefly-cli` commands.

### Ch 20 — Production & Deployment (`20-production.md`)
- **Adds:** the **production entry point** as the **one-line `main`** —
  `firefly::FireflyApplication::new("lumen").run().await` boots and serves both
  ports through the framework lifecycle with graceful SIGINT/SIGTERM shutdown,
  no hand-written `Application` wiring. Adds the optional **reactive streaming
  endpoint** with the `streaming` feature
  (`GET /api/v1/wallets/:id/events` → NDJSON / SSE), contributed as a
  `RouteContributor` **bean** the framework auto-merges. Discusses the in-memory
  → real-infra (Postgres event store + Kafka broker) swap.
- **Files:** `src/main.rs` (the one-line `main`), `src/web.rs` (`StreamingRoutes`
  `RouteContributor` bean + `streaming_router` / `stream_events`, feature-gated),
  `Cargo.toml` (`[features] streaming`), `src/streaming_test.rs`.
- **APIs/macros:** `FireflyApplication::{new, run}` (graceful shutdown built in),
  `firefly::web::RouteContributor` (as a `#[firefly(provides = "dyn ..")]` bean),
  `firefly::web::{NdJson, Sse}`, `firefly::reactive::Flux`.

### Ch 21 — Declarative Services with Macros (`21-declarative-macros.md`)
- **Adds:** the **capstone retrospective** — re-reads Lumen through the
  declarative-macro lens, cataloguing every `#[derive(...)]` / `#[...]` the
  service uses and how each collapses framework wiring into a declaration next
  to the code. The "one facade + macros" thesis, proven by the running crate.
- **Files:** all of `src/` (as a guided tour).
- **APIs/macros:** the full set — `#[derive(Command/Query/Component/Service/
  Repository/Configuration/Controller/ConfigProperties/DomainEvent/
  AggregateRoot)]`, `#[handlers]` (the handler-bean impl attribute) +
  `#[command_handler]`/`#[query_handler]`/`#[event_listener]` method markers,
  `#[rest_controller]` + verbs, `#[scheduled]`, `#[bean]`, `#[autowired]`,
  `scan` / `register_all!`.

---

## Appendices (reference, not Lumen growth)

- **Module Index** (`91-appendix-modules.md`) — the crates behind the facade
  surfaces Lumen touches.
- **Glossary** (`92-glossary.md`) — value object, aggregate, projection, saga
  compensation, RFC 9457, etc.

## Verification contract (CI gate the listings must keep green)

From the workspace root, with `export PATH="/opt/homebrew/bin:$PATH"`:

```sh
cargo build  -p firefly-sample-lumen
cargo test   -p firefly-sample-lumen                       # 42 unit + 12 HTTP = 54
cargo test   -p firefly-sample-lumen --features streaming  # + 3 streaming = 57
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
cargo clippy -p firefly-sample-lumen --all-targets --features streaming -- -D warnings
cargo fmt    -p firefly-sample-lumen -- --check
```

All green as of this writing. A chapter author who changes a Lumen listing must
re-run this gate; if a snippet in the prose drifts from the file, the build or a
test fails and the drift is caught.
