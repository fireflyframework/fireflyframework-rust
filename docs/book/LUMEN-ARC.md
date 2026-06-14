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
│   ├── lib.rs            # module wiring + crate-root re-exports + VERSION
│   ├── main.rs           # process entry point: banner, API+admin servers, scheduler, lifecycle
│   ├── money.rs          # Money value object (immutable, cents, exact)
│   ├── domain.rs         # Wallet aggregate + DomainEvent payloads + WalletView/WalletEvent
│   ├── ledger.rs         # event-sourced Ledger service + ReadModel + #[event_listener] projection
│   ├── commands.rs       # CQRS messages (#[derive(Command/Query)]) + #[command_handler]/#[query_handler]
│   ├── transfer.rs       # Transfer saga (debit → credit, with compensation)
│   ├── security.rs       # JWT mint/verify + BearerLayer + RBAC FilterChain
│   ├── web.rs            # #[rest_controller] + composition root (build_app / build_router)
│   └── housekeeping.rs   # #[scheduled] heartbeat task
└── tests/
    ├── http.rs           # tower::oneshot end-to-end: HTTP + CQRS + saga + projection + auth
    └── streaming.rs      # feature-gated NDJSON / SSE streaming endpoint
```

## Endpoints (the contract the chapters converge on)

| Method & path                        | Handler                | Engine                       |
|--------------------------------------|------------------------|------------------------------|
| `POST /api/v1/wallets`               | `WalletApi::open`      | CQRS `OpenWallet` → 201       |
| `GET  /api/v1/wallets/:id`           | `WalletApi::get`       | CQRS `GetWallet` (cached 30s) |
| `POST /api/v1/wallets/:id/deposit`   | `WalletApi::deposit`   | CQRS `Deposit`                |
| `POST /api/v1/wallets/:id/withdraw`  | `WalletApi::withdraw`  | CQRS `Withdraw`               |
| `POST /api/v1/transfers`             | `WalletApi::transfer`  | Saga (debit → credit)         |
| `GET  /api/v1/wallets/:id/events`    | `stream_events`        | reactive `Flux` → NDJSON/SSE (feature `streaming`) |
| `GET  /actuator/*`                   | actuator router        | health / info / metrics / loggers |

The mutating routes require a `CUSTOMER` JWT; the `GET` reads and the actuator
surface are public.

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
  dependency, an empty `lib.rs`/`main.rs`, the workspace member entry. A
  `#[tokio::main]` that builds a `Core`/`WebStack` and prints the banner.
- **Files:** `Cargo.toml`, `src/lib.rs`, `src/main.rs` (banner only).
- **APIs/macros:** `use firefly::prelude::*;`, `CoreConfig`, `WebStack::new`,
  `Core::print_banner`, `firefly::VERSION`.

### Ch 3 — Configuration (`03-configuration.md`)
- **Adds:** `CoreConfig` fields (`app_name`, `app_version`) that name Lumen and
  feed `/actuator/info`; introduces the env-overridable bind addresses
  (`LUMEN_ADDR` / `LUMEN_ADMIN_ADDR`) used by `main.rs`.
- **Files:** `src/web.rs` (`APP_NAME`, `build_app` config), `src/main.rs`.
- **APIs/macros:** `CoreConfig`, profile/property knobs; (forward-reference to
  `#[derive(ConfigProperties)]` for a reader exercise).

### Ch 4 — Dependency Wiring (`04-dependency-wiring.md`)
- **Adds:** the composition-root idea — `build_app()` resolves collaborators
  and hands the controller its state. Introduces the DI vocabulary
  (`Container`, stereotypes) and where Lumen *could* use `#[derive(Component)]`
  / `#[autowired]` (the explicit-wiring vs. scan trade-off; Lumen keeps an
  explicit root for teachability and shows the scan alternative as a callout).
- **Files:** `src/web.rs` (`build_app` skeleton).
- **APIs/macros:** `Container`, `firefly::scan`, `#[derive(Component/Service/Repository)]`,
  `#[autowired]`, `register_all!` (shown; Lumen wires explicitly).

### Ch 5 — The Reactive Model — Mono & Flux (`05-reactive-model.md`)
- **Adds:** the reactive primitives Lumen leans on later — `Mono`/`Flux`. No
  Lumen file yet; the chapter previews how `Flux::just`/`Flux::from_stream`
  back the streaming endpoint and how `Bus::send_mono` returns a `Mono`.
- **Files:** — (forward-references `src/web.rs` streaming).
- **APIs/macros:** `firefly::reactive::{Mono, Flux}`.

### Ch 6 — Your First HTTP API (`06-first-http-api.md`)
- **Adds:** the **first real endpoints**. The `WalletApi` controller with
  `POST /api/v1/wallets` and `GET /api/v1/wallets/:id`, returning a
  `WalletView`. At this stage the store is a simple in-memory map; the
  controller is the `#[rest_controller]` macro target.
- **Files:** `src/web.rs` (`WalletApi`, `#[rest_controller]`, `open`/`get`),
  early `WalletView`, `tests/http.rs` (first `oneshot` round-trip).
- **APIs/macros:** `#[rest_controller(path = "/api/v1")]`, `#[get]`/`#[post]`,
  `WebResult<T>`, `WebError`, `FireflyError::{not_found, validation}`,
  RFC 9457 problem rendering; `tower::ServiceExt::oneshot`.

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
  `Withdraw` commands and the `GetWallet` query, the free-fn handlers, and the
  bus wiring. Introduces the `OnceLock` "publish the resolved collaborators for
  free-fn handlers" pattern and the read-after-write cache invalidation.
- **Files:** `src/commands.rs` (whole file), `src/web.rs` (handlers dispatch
  through `Bus`, `query_cache.invalidate_type::<GetWallet>()`).
- **APIs/macros:** `#[derive(Command)]` / `#[derive(Query)]` (with
  `#[firefly(validate)]` and `#[firefly(cache_ttl = "30s")]`),
  `#[command_handler]` / `#[query_handler]` (→ `register_*`), `Bus::{send,
  query}`, `ValidationMiddleware`, `QueryCache`, `CqrsError`.

### Ch 10 — Event-Driven Architecture & Messaging (`10-eda-messaging.md`)
- **Adds:** the **domain events on the wire** — the `Ledger` publishes each
  persisted event to a `Broker`, and the **read-model projection** consumes
  them to keep `ReadModel` current (the CQRS loop closes here). Idempotent
  rebuild-from-stream projection.
- **Files:** `src/ledger.rs` (`Ledger::commit` publish step, `to_envelope`,
  `project_wallet_event`, `bind_projection`).
- **APIs/macros:** `#[event_listener(topic = "wallets.events")]`
  (→ `subscribe_*`), `firefly::eda::{Broker, Event, handler, InMemoryBroker}`,
  `Event::{new, with_key, with_header}`, the Kafka/RabbitMQ adapter swap
  callout.

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
  reads + actuator public. `build_router` layers bearer auth around the
  controller.
- **Files:** `src/security.rs` (whole file), `src/web.rs` (`router()` applies
  `security_layers()`), `tests/http.rs` (401/422 problem assertions).
- **APIs/macros:** `firefly::security::{JwtService, Verifier, VerifierFn,
  Authentication, SecurityError, BearerLayer, BearerConfig, FilterChain}`,
  `claims → Authentication`, RFC 9457 401 rendering; `JwksVerifier` as the
  production IdP swap.

### Ch 15 — Observability (`15-observability.md`)
- **Adds:** the **actuator + admin surface**. `main.rs` serves
  `actuator_router(...)` on the admin port (`/actuator/health`,
  `/info`, `/metrics`, `/loggers`), with an `InfoContributor` describing the
  Lumen build. Structured logging via `init_logging`. The web stack's request
  metrics + correlation id are already on by default.
- **Files:** `src/main.rs` (`actuator_router`, `InfoContributor`,
  `init_logging`).
- **APIs/macros:** `WebStack::actuator_router`, `firefly::starter_core::InfoContributor`,
  `Core::init_logging`, request-metrics / correlation middleware (on by default
  in `WebStack`).

### Ch 16 — Scheduling & Notifications (`16-scheduling-notifications.md`)
- **Adds:** the **scheduled housekeeping** task — a `#[scheduled]` heartbeat
  registered on a `Scheduler` and started from `main.rs`. Framed as where a
  daily-statement notification (`firefly::notifications`) would hang.
- **Files:** `src/housekeeping.rs` (whole file), `src/main.rs` (build + start
  the scheduler).
- **APIs/macros:** `#[scheduled(fixed_rate = "60s", initial_delay = "5s")]`
  (→ `schedule_*`), `firefly::prelude::Scheduler`, `Scheduler::start`;
  notifications adapters as a callout.

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
  the in-process, no-socket testing model and the shared-global handler caveat.
- **Files:** `tests/http.rs`, every `#[cfg(test)] mod tests` in `src/`.
- **APIs/macros:** `tower::ServiceExt::oneshot`, `http_body_util::BodyExt`,
  `firefly::testkit` helpers; `StepVerifier`/Testcontainers as production-grade
  callouts.

### Ch 19 — The CLI (`19-cli.md`)
- **Adds:** (tooling-level) how the `firefly` CLI scaffolds and runs a service
  like Lumen; not a Lumen source change. References `cargo run --bin lumen` and
  the `LUMEN_ADDR` overrides.
- **Files:** — (operational).
- **APIs/macros:** `firefly-cli` commands.

### Ch 20 — Production & Deployment (`20-production.md`)
- **Adds:** the **production entry point** — `main.rs` end to end: banner,
  public API + admin servers wired through the lifecycle `Application` with
  graceful SIGINT/SIGTERM shutdown, and the optional **reactive streaming
  endpoint** turned on with the `streaming` feature
  (`GET /api/v1/wallets/:id/events` → NDJSON / SSE). Discusses the in-memory →
  real-infra (Postgres event store + Kafka broker) swap.
- **Files:** `src/main.rs` (whole lifecycle wiring), `src/web.rs`
  (`streaming_router` / `stream_events`, feature-gated), `Cargo.toml`
  (`[features] streaming`), `tests/streaming.rs`.
- **APIs/macros:** `firefly::prelude::Application`, `Core::new_application`,
  `on_server`, `ShutdownHandle::wait`, `axum::serve(...).with_graceful_shutdown`,
  `firefly::web::{NdJson, Sse}`, `firefly::reactive::Flux`.

### Ch 21 — Declarative Services with Macros (`21-declarative-macros.md`)
- **Adds:** the **capstone retrospective** — re-reads Lumen through the
  declarative-macro lens, cataloguing every `#[derive(...)]` / `#[...]` the
  service uses and how each collapses framework wiring into a declaration next
  to the code. The "one facade + macros" thesis, proven by the running crate.
- **Files:** all of `src/` (as a guided tour).
- **APIs/macros:** the full set — `#[derive(Command/Query/Component/Service/
  Repository/Configuration/Controller/ConfigProperties/DomainEvent/
  AggregateRoot)]`, `#[command_handler]`/`#[query_handler]`, `#[event_listener]`,
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
cargo test   -p firefly-sample-lumen                       # 34 unit + 7 HTTP + 1 doctest
cargo test   -p firefly-sample-lumen --features streaming  # + 3 streaming tests
cargo clippy -p firefly-sample-lumen --all-targets -- -D warnings
cargo clippy -p firefly-sample-lumen --all-targets --features streaming -- -D warnings
cargo fmt    -p firefly-sample-lumen -- --check
```

All green as of this writing. A chapter author who changes a Lumen listing must
re-run this gate; if a snippet in the prose drifts from the file, the build or a
test fails and the drift is caught.
