```
  _____.__                _____.__
_/ ____\__|______   _____/ ____\  | ___.__.
\   __\|  \_  __ \_/ __ \   __\|  |<   |  |
 |  |  |  ||  | \/\  ___/|  |  |  |_\___  |
 |__|  |__||__|    \___  >__|  |____/ ____|
                       \/           \/   rs
```

# Firefly Framework for Rust

**Spring Boot for Rust — a production-grade platform for building
*reactive* (WebFlux-style), event-driven, resilient microservices on
Rust 1.85+ (tokio + axum).**

[![Apache 2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Version 26.6.2](https://img.shields.io/badge/version-26.6.2-orange.svg)](CHANGELOG.md)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-93450a.svg)](https://www.rust-lang.org)
[![Reactive: Mono / Flux](https://img.shields.io/badge/reactive-Mono%20%2F%20Flux-success.svg)](docs/book/src/05-reactive-model.md)
[![Real-infra tested](https://img.shields.io/badge/tests-real%20infra%20(Docker)-2496ed.svg)](#real-infrastructure-testing)

> 📖 **Read the book — [Firefly Framework for Rust](docs/book/)** — the canonical,
> best-in-class guide: a punchy [Quickstart](docs/book/src/02-quickstart.md)
> (zero to a running reactive endpoint in minutes), the keystone
> [Reactive Model](docs/book/src/05-reactive-model.md) chapter (`Mono`/`Flux`),
> and full chapters on configuration, persistence, DDD, CQRS, EDA, event
> sourcing, sagas, HTTP clients, security, observability, testing, and
> production. Build it locally with `mdbook build docs/book` and open
> `docs/book/book/index.html`.

At its heart is a **WebFlux-style reactive core** — [`firefly-reactive`](crates/reactive/README.md)
gives you `Mono<T>` (0-or-1) and `Flux<T>` (0..N) over `tokio`
futures/streams, the faithful Rust analog of Project Reactor. That core
threads through the whole framework: reactive HTTP responders that stream
NDJSON/SSE with real backpressure, a `ReactiveCrudRepository` (in-memory
or real Postgres), a reactive `WebClient`, reactive EDA subscriptions,
and a reactive CQRS bus. If you have written Spring WebFlux, you already
know the shape.

Around that core, the framework provides the cross-cutting machinery
that every non-trivial business service needs — RFC 7807 error
envelopes, idempotency, correlation propagation, CQRS, event-driven
messaging, event sourcing, sagas, configuration servers, identity
adapters, document management, notifications, callbacks, webhooks —
behind a single, opinionated composition pattern. On top of that core it
ships a **full PyFly-parity layer**: a domain-driven kernel, an opt-in
DI container, aspect-oriented advice, server-side HTTP sessions, a
Spring-Shell-style CLI framework, WebSocket server support, a
Spring-Boot-Admin-style dashboard, a `firefly` developer CLI, and
**real, fully-wired** vendor adapters — Keycloak/Azure/Cognito IDP,
S3/Blob/e-sign ECM, SMTP/SendGrid/Resend/Twilio/Firebase notifications,
Redis/Postgres cache, and Kafka/RabbitMQ/Postgres/Redis-Streams event
transports. **No stubs remain** — every adapter drives its real
provider.

This repository is the official Rust port of the Java/Spring Boot
[`org.fireflyframework`](https://fireflyframework.org) platform — the
fourth sibling port, joining the
[.NET](../fireflyframework-dotnet),
[Go](https://github.com/fireflyframework/fireflyframework-go), and
[Python (PyFly)](https://github.com/fireflyframework/fireflyframework-pyfly)
ports. It preserves every public contract, configuration key and wire
format from the Java release line, re-implemented with idiomatic Rust
tooling (`tokio`, `axum`, `tower`, `serde`, `thiserror`, `async-trait`,
RustCrypto, `tracing`). A service running version *X* on Java, .NET,
Go, Python, or Rust consumes the same contracts and emits the same
wire format.

The compiled-language core (foundational + platform + starter tiers)
holds module-for-module parity with the Go port and is kept
wire-stable; the **PyFly-parity layer is purely additive** — every
extension layers onto the existing crates without changing a single
Go-parity wire format.

---

## Why Rust

Modern back-office systems aren't bottlenecked by writing the next
handler. They're bottlenecked by getting **the same** handler,
**the same** error response, **the same** correlation id, **the same**
saga compensation, **the same** observability story across every service
in the platform. Every team that re-invents these picks slightly
different conventions and the platform fragments.

Firefly Framework treats those concerns as solved problems on Rust too:

- **Reactive by design.** `firefly-reactive` brings Reactor's `Mono` /
  `Flux` to Rust: lazy, composable, `FireflyError`-typed publishers that
  drop straight into an axum handler. A handler can return a `Mono<T>`
  (rendered as JSON, with `Ok(None)` → 404) or a `Flux<T>` (streamed as
  `application/x-ndjson` or SSE with **true backpressure** — a million
  rows never land in memory). The same two types back the repositories,
  the `WebClient`, EDA subscriptions, and the CQRS bus.
- **Composed, not constructed.** A single `Core::new(CoreConfig { .. })`
  call wires the whole infrastructure tier — middleware chain, cache,
  CQRS bus, event broker, health composite, metrics, scheduler,
  lifecycle. Authors write commands, queries, handlers, and routes —
  nothing more.
- **Symmetric across runtimes.** The wire contract, the
  `application/problem+json` shape, the `Idempotency-Key` semantics, the
  saga step definitions, the event envelopes, the HMAC webhook
  signatures — all identical to the Java, .NET, Go, and Python sides.
- **Pluggable at the adapter layer.** Each integration point (IDP, ECM,
  storage, e-signature, notification channel, message broker) is an
  `async_trait` object-safe port with multiple adapter implementations
  selected at wiring time (`Arc<dyn Adapter>`).
- **Observable by default.** `tracing` structured logging with
  correlation-id enrichment, actuator health/metrics endpoints,
  RFC 7807 error envelopes, and a startup banner that names the
  application, version and runtime are all on out of the box.
- **Real adapters, no stubs.** Every infrastructure adapter ships fully
  wired: `firefly-cache-redis` speaks RESP and `firefly-cache-postgres`
  speaks SQL, `firefly-eda-{kafka,rabbitmq,postgres,redis}` drive
  `rdkafka` / `lapin` / `tokio-postgres` / Redis Streams,
  `firefly-notifications-smtp` delivers MIME over `lettre`, and every
  IDP / ECM / notification vendor adapter — Keycloak, Azure AD, Cognito,
  S3, Azure Blob, DocuSign, Adobe Sign, Logalty, SendGrid, Resend,
  Twilio, Firebase — calls its real provider over `reqwest`. There are
  no `NotImplemented` sentinels left in the adapter tier. See
  [`MODULES.md`](MODULES.md) for the per-crate catalogue.

---

## Feature matrix

| Capability | Crate(s) | Spring / Reactor analog | Status |
|------------|----------|-------------------------|:------:|
| **Reactive core (`Mono` / `Flux`)** | `firefly-reactive` | Project Reactor | ✅ Full |
| **Reactive HTTP responders** (NDJSON / SSE streaming, backpressure) | `firefly-web` | WebFlux `@RestController` returning `Mono`/`Flux` | ✅ Full |
| **Reactive repositories** (in-memory + real Postgres) | `firefly-data` | R2DBC `ReactiveCrudRepository` | ✅ Full |
| **Reactive HTTP client** (`WebClient`, `body_to_mono`/`body_to_flux`) | `firefly-client` | WebFlux `WebClient` | ✅ Full |
| **Reactive CQRS bus** (`send_mono` / `query_mono`) | `firefly-cqrs` | Axon / reactive command bus | ✅ Full |
| **Reactive EDA** (`subscribe_reactive` → `Flux<Event>`) | `firefly-eda` | reactive Kafka/AMQP listener | ✅ Full |
| RFC 7807 errors, correlation, idempotency, PII masking | `firefly-web`, `firefly-kernel` | `@ControllerAdvice` ProblemDetail | ✅ Full |
| Typed config (YAML + env + flags + profiles, `${...}`, refresh) | `firefly-config` | `@ConfigurationProperties` | ✅ Full |
| Event sourcing (aggregates, snapshots, projections, outbox, tenancy) | `firefly-eventsourcing` | Axon | ✅ Full |
| Sagas / Workflows (DAG) / TCC, compensation, retry | `firefly-orchestration` | Temporal / Camunda | ✅ Full |
| Security (JWT, JWKS, RBAC, OAuth2 login + authorization server, CSRF) | `firefly-security` | Spring Security | ✅ Full |
| Actuator (`health`/`info`/`metrics`/`env`/`tasks`/`version`, probes) | `firefly-actuator` | spring-boot-actuator | ✅ Full |
| Observability (`tracing`, W3C trace-context, metrics, banner) | `firefly-observability` | Micrometer + OTel | ✅ Full |
| Cache (`Adapter` port + Memory / NoOp / Fallback / **Redis** / **Postgres**) | `firefly-cache`, `-redis`, `-postgres` | spring-data cache | ✅ Full |
| Event transports (**Kafka / RabbitMQ / Postgres outbox / Redis Streams**) | `firefly-eda-*` | Spring Kafka / AMQP | ✅ Full |
| Identity providers (**Keycloak / Azure AD / Cognito / internal-db**) | `firefly-idp-*` | Spring Security OIDC | ✅ Full |
| Content + e-signature (**S3 / Blob / DocuSign / Adobe Sign / Logalty**) | `firefly-ecm-*` | — | ✅ Full |
| Notifications (**SMTP / SendGrid / Resend / Twilio / Firebase**) | `firefly-notifications-*` | — | ✅ Full |
| DI container / AOP / sessions / shell / WebSockets | `firefly-container`, `-aop`, `-session`, `-shell`, `-websocket` | Spring DI / AOP / Session / Shell | ✅ Full |
| Admin dashboard + `firefly` developer CLI | `firefly-admin`, `firefly-cli` | spring-boot-admin / Spring Boot CLI | ✅ Full |

Every entry is real and wired — there are no stub adapters in this
release.

---

## Architecture at a glance

The framework is organised into four strictly-layered tiers, with a
left-to-right dependency direction:

```
┌────────────────┐   ┌──────────────────┐   ┌──────────────────────┐   ┌──────────────────────┐
│  FOUNDATIONAL  │ → │     PLATFORM     │ → │       ADAPTERS       │ → │       STARTERS       │
│                │   │                  │   │                      │   │                      │
│  reactive      │   │  cache           │   │  client (WebClient)  │   │  starter-core        │
│  kernel        │   │  observability   │   │  idp-*               │   │  starter-application │
│  utils         │   │  data            │   │  ecm-*               │   │  starter-domain      │
│  validators    │   │  cqrs            │   │  notifications-*     │   │  starter-data        │
│  web           │   │  eda  · eda-*    │   │  callbacks           │   │  starter-web         │
│  config        │   │  eventsourcing   │   │  webhooks            │   │  backoffice          │
│  i18n          │   │  orchestration   │   │  config-server       │   │                      │
│  session       │   │  rule-engine     │   │  cache-redis         │   │  ── Operations ──    │
│                │   │  plugins         │   │  cache-postgres      │   │  admin               │
│                │   │  container · aop │   │  notifications-smtp  │   │                      │
│                │   │  lifecycle       │   │                      │   │  ── Tooling ──       │
│                │   │  actuator        │   │                      │   │  cli                 │
│                │   │  scheduling      │   │                      │   │                      │
│                │   │  resilience      │   │                      │   │                      │
│                │   │  security        │   │                      │   │                      │
│                │   │  migrations      │   │                      │   │                      │
│                │   │  openapi         │   │                      │   │                      │
│                │   │  sse · websocket │   │                      │   │                      │
│                │   │  shell           │   │                      │   │                      │
│                │   │  transactional   │   │                      │   │                      │
│                │   │  testkit         │   │                      │   │                      │
└────────────────┘   └──────────────────┘   └──────────────────────┘   └──────────────────────┘
```

Each tier may depend on the tiers to its left, never to its right. The
Cargo crate graph enforces the layering — every internal dependency is
declared once in `[workspace.dependencies]` and there is no path that
bypasses it. The reactive core, `firefly-reactive`, sits at the
foundational base: every reactive surface above it (`firefly-web`'s
`MonoJson`/`NdJson`/`Sse` responders, `firefly-data`'s
`ReactiveCrudRepository`, `firefly-client`'s `WebClient`, the reactive
EDA/CQRS APIs) is built on its `Mono`/`Flux`.

The infrastructure adapters (`cache-redis`, `cache-postgres`,
`eda-{kafka,rabbitmq,postgres,redis}`, `notifications-smtp`) are
*optional* leaf crates: they implement the platform ports
(`cache::Adapter`, `eda::Broker`, the notifications `Channel`) so a
service pulls in `rdkafka` / `lapin` / `redis` / `tokio-postgres` /
`lettre` only when it actually selects that backend. `firefly-starter-web`
is a ready-made web-stack starter (`Core` + CORS + security headers +
request metrics/logging) — all real and wired.

See [`MODULES.md`](MODULES.md) for the full per-crate catalogue and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design rationale.

---

## Workspace layout

One Cargo workspace, **69 members** — 66 framework crates plus the
integration suite and two reference samples — spanning the Go-parity
core (foundational, platform, adapter, starter tiers) and the
PyFly-parity layer:

```
fireflyframework-rust/
├── crates/                       # 66 framework crates (firefly-<name>)
│   ├── reactive/                 #   the Mono/Flux reactive core (keystone)
│   ├── kernel/                   #   each with its own README.md + test suite
│   ├── web/  cqrs/  eda/  …       #   Go-parity core (+ reactive surfaces)
│   │
│   ├── container/  aop/           #   PyFly: DI container + aspect advice
│   ├── session/  shell/  websocket/  #   PyFly: sessions, CLI framework, WS server
│   ├── cli/                       #   PyFly: the `firefly` developer CLI binary
│   ├── admin/                     #   PyFly: Spring-Boot-Admin-style dashboard
│   │
│   ├── cache-redis/  cache-postgres/  #   adapters: Redis + Postgres cache
│   ├── eda-kafka/  eda-rabbitmq/  #   adapter: event transports
│   ├── eda-postgres/  eda-redis/  #     (Kafka / RabbitMQ / Postgres outbox / Redis Streams)
│   ├── notifications-smtp/        #   adapter: SMTP e-mail
│   ├── idp-*/  ecm-*/             #   adapters: identity + content vendors (all real)
│   ├── starter-web/              #   starter: ready-made web-stack bundle
│   └── backoffice/
├── tests/integration/            # cross-crate integration suite
├── samples/orders/               # reference service (firefly-sample-orders)
├── samples/reactive-banking/     # end-to-end reactive service (firefly-sample-reactive-banking)
├── docs/                         # ARCHITECTURE, CONFIGURATION, MIGRATION-GUIDE, DESIGN
├── docs/book/                    # the mdBook guide (mdbook build docs/book)
├── docker-compose.yml            # real backing services for integration tests
└── Cargo.toml                    # workspace root — version 26.6.2, edition 2021, MSRV 1.85
```

### Choosing your tier / optional adapters

Start from a **starter** and add only the adapters you need:

- **Default, zero infrastructure** — `firefly-starter-core` boots with
  the in-process `MemoryAdapter` cache and `InMemoryBroker` event bus.
  Nothing external is required; a service runs against pure-Rust defaults.
- **Pick a cache backend** — drop in `firefly-cache-redis`
  (`RedisAdapter`) wherever an `Arc<dyn cache::Adapter>` is expected.
- **Pick an event transport** — `firefly-eda-kafka`,
  `-rabbitmq`, `-postgres` (durable outbox), or `-redis` (Streams) each
  implement the same `Broker` port; swap the constructor, keep your
  handlers.
- **Pick notification channels / IDP / ECM vendors** — code against the
  parent-port trait (`notifications::Channel`, `idp::Adapter`,
  `ecm::ContentStore`) and pull in the concrete adapter crate at wiring
  time, so the heavy vendor SDKs stay out of services that don't use them.
- **Add operations** — `firefly-admin` mounts the dashboard;
  `firefly-cli` installs the `firefly` developer binary.

---

## Quickstart

> For the full walkthrough — including the `firefly` CLI scaffold, the
> actuator, and graceful shutdown — see the book's
> [Quickstart chapter](docs/book/src/02-quickstart.md).

Add the starter and the reactive core to a binary crate:

```toml
[dependencies]
firefly-starter-core = "26.6.2"
firefly-reactive = "26.6.2"
firefly-web = "26.6.2"
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
serde_json = "1"
```

Boot a service — one `Core::new` wires the problem renderer,
correlation propagation, idempotency replay, cache, CQRS bus, event
broker, health, metrics and scheduler — then mount a plain route, a
reactive `Mono` route, and a streaming `Flux` (NDJSON) route:

```rust
use axum::{routing::get, Router};
use firefly_reactive::{Flux, Mono};
use firefly_starter_core::{Core, CoreConfig};
use firefly_web::{MonoJson, NdJson};

// A reactive Mono → 200 application/json (Ok(None) → 404 problem+json).
async fn one_order() -> MonoJson<serde_json::Value> {
    MonoJson(Mono::just(serde_json::json!({ "id": "o1", "customer": "alice" })))
}

// A streaming Flux → application/x-ndjson, one line per element, backpressured.
async fn stream_orders() -> NdJson<i64> {
    NdJson(Flux::range(1, 3))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::new(CoreConfig {
        app_name: "orders".into(),
        ..CoreConfig::default()
    });
    core.init_logging()?;
    core.print_banner();

    let api = core.apply_middleware(
        Router::new()
            .route("/orders", get(|| async { "[]" }))
            .route("/orders/one", get(one_order))
            .route("/orders/stream", get(stream_orders)),
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, api).await?;
    Ok(())
}
```

`curl -N localhost:8080/orders/stream` streams `1`, `2`, `3` as NDJSON,
flushed incrementally with real backpressure. Every `POST`/`PUT`/`PATCH`
carrying an `Idempotency-Key` header is recorded; repeating the request
replays the stored response with `Idempotent-Replay: true`. Every
response echoes an `X-Correlation-Id`. Any handler error renders as
`application/problem+json`. Add `core.actuator_router(..)` on a second
listener for the `/actuator/{health,info,metrics,env,tasks,version}`
management surface, and `core.new_application()` for signal-aware
graceful shutdown — see
[`crates/starter-core/README.md`](crates/starter-core/README.md) and the
[Reactive Model](docs/book/src/05-reactive-model.md) chapter.

Two reference services ship in the workspace: a minimal idempotent
[`samples/orders/`](samples/orders), and the end-to-end reactive
[`samples/reactive-banking/`](samples/reactive-banking) — reactive CQRS
(`Bus::send_mono` / `query_mono`), event sourcing, a saga-backed money
transfer, a `Flux<AccountEvent>` NDJSON/SSE stream, and a `WebClient`
SDK, running against in-memory defaults or real Postgres/Kafka.

---

## Build, test, ship

```bash
make ci          # cargo fmt --check + clippy -D warnings + build + test
make build       # cargo build --workspace
make test        # cargo test --workspace
make sample      # run the Orders sample
make cli ARGS="doctor"   # run the firefly developer CLI
make book        # build the mdBook guide (docs/book)
```

Or plain cargo — the whole repository is a single standard workspace:

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Requires Rust 1.85+ (edition 2021).

---

## Real-infrastructure testing

Beyond the hermetic `cargo test --workspace` suite — which is green on a
bare machine with no services running — the framework ships a
**real-infrastructure** test path. A `docker-compose.yml` brings up
Postgres, Redis, RabbitMQ, Redpanda (Kafka API), Keycloak, LocalStack
(S3), Azurite (Blob), and MailHog (SMTP); the env-gated integration
tests then run the adapters against those **real** services rather than
mocks:

```bash
make infra-up           # start the docker-compose stack (waits for health)
make test-integration   # run the env-gated tests against the live services
make infra-down         # tear it all down
```

Each integration test reads a connection URL/addr from the environment
and skips when it is unset, so `cargo test` stays green offline while
`make test-integration` exercises the genuine Redis RESP, Kafka
protocol, RabbitMQ AMQP, Postgres SQL, S3/Blob object stores, Keycloak
OIDC, and SMTP delivery paths. This covers the cache, EDA, IDP, ECM,
notification, and reactive-Postgres surfaces — and the reactive-banking
sample — end to end.

---

## Status

The framework ships **69 workspace members** — **66 framework crates**
under `crates/` plus the cross-crate integration suite and two reference
samples (`samples/orders`, `samples/reactive-banking`). The workspace
quality gate is `make ci`: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --workspace`, `cargo test --workspace`.

**Every tier is fully implemented and wired.** The reactive core
(`firefly-reactive`) and its integrations (reactive web responders,
reactive repositories incl. real Postgres, the reactive `WebClient`,
reactive EDA and CQRS), the foundational/platform/starter tiers, and the
PyFly-parity layer (`firefly-container`, `firefly-aop`,
`firefly-session`, `firefly-shell`, `firefly-websocket`, `firefly-cli`,
`firefly-admin`) are all complete.

The infrastructure and vendor adapters ship **real and wired, with no
stubs**: `firefly-cache-redis` (RESP), `firefly-cache-postgres` (SQL),
`firefly-eda-{kafka,rabbitmq,postgres,redis}`, `firefly-notifications-smtp`
(`lettre` MIME), the IDP adapters (Keycloak OIDC + admin REST, Azure AD
Microsoft Graph, AWS Cognito JSON API + SigV4, internal-db), the ECM
adapters (S3, Azure Blob, DocuSign, Adobe Sign, Logalty), and the
notification channels (SendGrid v3, Resend, Twilio, Firebase FCM) all
call their real backends. `firefly-starter-web` is a ready-made
web-stack starter (`Core` + CORS + security headers + request
metrics/logging). The only `NotImplemented` errors that remain are
legitimate runtime conditions (e.g. a missing notification template),
not unimplemented adapters.

See [`MODULES.md`](MODULES.md) for the per-crate catalogue.

---

## Documentation

- **[The Book](docs/book/)** — the canonical guide; build with
  `mdbook build docs/book` and open `docs/book/book/index.html`. Start
  with the [Quickstart](docs/book/src/02-quickstart.md) and the keystone
  [Reactive Model](docs/book/src/05-reactive-model.md) chapter.
- **[`MODULES.md`](MODULES.md)** — the per-crate module index, tier by tier.
- **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)** — tiering, the EDA
  transport-adapter pattern, the reactive translation, build waves.
- **[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md)** — the typed config
  loader and the full Java-key → Rust-wiring mapping.
- **[`docs/MIGRATION-GUIDE.md`](docs/MIGRATION-GUIDE.md)** — porting a
  Java/Spring (or .NET/Go/Python) service to the Rust port.
- Every crate ships its own `README.md` with its public surface and a
  runnable quick-start.

---

## License & contributing

Apache 2.0 — see [`LICENSE`](LICENSE). Every source file carries the
Apache 2.0 header (Firefly Software Foundation, 2026).

Contributions are welcome. Before opening a PR, run `make ci` (format,
clippy with `-D warnings`, build, and test must all pass). New public
surface should ship with crate-level docs and tests, and keep the
Go-parity wire contract byte-stable.
