```
  _____.__                _____.__
_/ ____\__|______   _____/ ____\  | ___.__.
\   __\|  \_  __ \_/ __ \   __\|  |<   |  |
 |  |  |  ||  | \/\  ___/|  |  |  |_\___  |
 |__|  |__||__|    \___  >__|  |____/ ____|
                       \/           \/   rs
```

# Firefly Framework for Rust

**A production-grade platform for building reactive, event-driven, resilient
microservices on Rust 1.85+ (tokio + axum).**

> 📖 **Read the book — [Firefly Framework for Rust](docs/book/)** — the canonical,
> best-in-class guide: a punchy [Quickstart](docs/book/src/02-quickstart.md)
> (zero to a running reactive endpoint in minutes), the keystone
> [Reactive Model](docs/book/src/05-reactive-model.md) chapter (`Mono`/`Flux`),
> and full chapters on configuration, persistence, DDD, CQRS, EDA, event
> sourcing, sagas, HTTP clients, security, observability, testing, and
> production. Build it locally with `mdbook build docs/book` and open
> `docs/book/book/index.html`.

The Firefly Framework provides the cross-cutting machinery that every
non-trivial business service needs — RFC 7807 error envelopes,
idempotency, correlation propagation, CQRS, event-driven messaging,
event sourcing, sagas, configuration servers, identity adapters,
document management, notifications, callbacks, webhooks — behind a
single, opinionated composition pattern. On top of that core it ships a
**full PyFly-parity layer**: a domain-driven kernel, an opt-in DI
container, aspect-oriented advice, server-side HTTP sessions, a
Spring-Shell-style CLI framework, WebSocket server support, a
Spring-Boot-Admin-style dashboard, a `firefly` developer CLI, and
**real** vendor adapters — Keycloak/Azure/Cognito IDP, S3/Blob/e-sign
ECM, SMTP/SendGrid/Resend/Twilio/Firebase notifications, Redis cache,
and Kafka/RabbitMQ/Postgres/Redis-Streams event transports.

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
- **Real adapters, not promises.** The infrastructure adapters ship
  wired: `firefly-cache-redis` speaks RESP, `firefly-eda-{kafka,rabbitmq,
  postgres,redis}` drive `rdkafka` / `lapin` / `tokio-postgres` / Redis
  Streams, `firefly-notifications-smtp` delivers MIME over `lettre`, and
  the IDP / ECM vendor adapters carry their real provider flows. Where a
  provider genuinely isn't wired yet, the method returns a typed
  `…NotImplemented`-style error with an actionable message — never a
  silent stub. See [`MODULES.md`](MODULES.md) for per-crate Full / Stub
  status.

---

## Architecture at a glance

The framework is organised into four strictly-layered tiers, with a
left-to-right dependency direction:

```
┌────────────────┐   ┌──────────────────┐   ┌────────────────┐   ┌──────────────────────┐
│  FOUNDATIONAL  │ → │     PLATFORM     │ → │    ADAPTERS    │ → │       STARTERS       │
│                │   │                  │   │                │   │                      │
│  kernel        │   │  cache           │   │  client        │   │  starter-core        │
│  utils         │   │  observability   │   │  idp-*         │   │  starter-application │
│  validators    │   │  data            │   │  ecm-*         │   │  starter-domain      │
│  web           │   │  cqrs            │   │  notifications*│   │  starter-data        │
│  config        │   │  eda  · eda-*    │   │  callbacks     │   │  backoffice          │
│  i18n          │   │  eventsourcing   │   │  webhooks      │   │                      │
│  session       │   │  orchestration   │   │  config-server │   │                      │
│                │   │  rule-engine     │   │  cache-redis   │   │  ── Operations ──    │
│                │   │  plugins         │   │  notif.-smtp   │   │  admin               │
│                │   │  container · aop │   │                │   │                      │
│                │   │  lifecycle       │   │                │   │  ── Tooling ──       │
│                │   │  actuator        │   │                │   │  cli                 │
│                │   │  scheduling      │   │                │   │                      │
│                │   │  resilience      │   │                │   │                      │
│                │   │  security        │   │                │   │                      │
│                │   │  migrations      │   │                │   │                      │
│                │   │  openapi         │   │                │   │                      │
│                │   │  sse · websocket │   │                │   │                      │
│                │   │  shell           │   │                │   │                      │
│                │   │  transactional   │   │                │   │                      │
│                │   │  testkit         │   │                │   │                      │
└────────────────┘   └──────────────────┘   └────────────────┘   └──────────────────────┘
```

Each tier may depend on the tiers to its left, never to its right. The
Cargo crate graph enforces the layering — every internal dependency is
declared once in `[workspace.dependencies]` and there is no path that
bypasses it. The infrastructure adapters (`cache-redis`,
`eda-{kafka,rabbitmq,postgres,redis}`, `notifications-smtp`) are
*optional* leaf crates: they implement the platform ports
(`cache::Adapter`, `eda::Broker`, the notifications `Channel`) so a
service pulls in `rdkafka` / `lapin` / `redis` / `lettre` only when it
actually selects that backend. Two further adapter/starter crates —
`firefly-cache-postgres` (a Postgres `cache::Adapter`) and
`firefly-starter-web` (a web-stack starter) — are reserved as
port-pending placeholders for the next wave.

See [`MODULES.md`](MODULES.md) for the full per-crate catalogue and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design rationale.

---

## Workspace layout

One Cargo workspace, 67 members — the Go-parity core (foundational,
platform, adapter, starter tiers) plus the PyFly-parity layer:

```
fireflyframework-rust/
├── crates/                       # 65 framework crates (firefly-<name>)
│   ├── kernel/                   #   each with its own README.md + test suite
│   ├── web/  cqrs/  eda/  …       #   Go-parity core
│   │
│   ├── container/  aop/           #   PyFly: DI container + aspect advice
│   ├── session/  shell/  websocket/  #   PyFly: sessions, CLI framework, WS server
│   ├── cli/                       #   PyFly: the `firefly` developer CLI binary
│   ├── admin/                     #   PyFly: Spring-Boot-Admin-style dashboard
│   │
│   ├── cache-redis/               #   adapter: Redis cache
│   ├── cache-postgres/            #   adapter: Postgres cache (port pending)
│   ├── eda-kafka/  eda-rabbitmq/  #   adapter: event transports
│   ├── eda-postgres/  eda-redis/  #     (Kafka / RabbitMQ / Postgres outbox / Redis Streams)
│   ├── notifications-smtp/        #   adapter: SMTP e-mail
│   ├── idp-*/  ecm-*/             #   adapters: identity + content vendors
│   ├── starter-web/              #   starter: web stack bundle (port pending)
│   └── backoffice/
├── tests/integration/            # cross-crate integration suite
├── samples/orders/               # reference service (firefly-sample-orders)
├── docs/                         # ARCHITECTURE, CONFIGURATION, MIGRATION-GUIDE, DESIGN
└── Cargo.toml                    # workspace root — version 26.6.1, edition 2021, MSRV 1.85
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

> For the full walkthrough — including a streaming reactive endpoint, the
> actuator, and graceful shutdown — see the book's
> [Quickstart chapter](docs/book/src/02-quickstart.md).

Add the starter to a binary crate:

```toml
[dependencies]
firefly-starter-core = "26.6.1"
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
```

Boot a service — one `Core::new` wires the problem renderer,
correlation propagation, idempotency replay, cache, CQRS bus, event
broker, health, metrics and scheduler:

```rust
use axum::{routing::get, Router};
use firefly_starter_core::{Core, CoreConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let core = Core::new(CoreConfig {
        app_name: "orders".into(),
        ..CoreConfig::default()
    });
    core.init_logging()?;
    core.print_banner();

    let api = core.apply_middleware(
        Router::new().route("/orders", get(|| async { "[]" })),
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, api).await?;
    Ok(())
}
```

Every `POST`/`PUT`/`PATCH` carrying an `Idempotency-Key` header is
recorded; repeating the request replays the stored response with
`Idempotent-Replay: true`. Every response echoes an `X-Correlation-Id`.
Any handler error renders as `application/problem+json`. Add
`core.actuator_router(..)` on a second listener for the
`/actuator/{health,info,metrics,env,tasks,version}` management surface,
and `core.new_application()` for signal-aware graceful shutdown — see
[`crates/starter-core/README.md`](crates/starter-core/README.md).

A reference Orders service lives at [`samples/orders/`](samples/orders).

---

## Build, test, ship

```bash
make ci          # cargo fmt --check + clippy -D warnings + build + test
make build       # cargo build --workspace
make test        # cargo test --workspace
make sample      # run the Orders sample
```

Or plain cargo — the whole repository is a single standard workspace:

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Requires Rust 1.85+ (edition 2021).

---

## Status

The framework ships **67 workspace members** across the four tiers
(65 crates under `crates/` plus the integration suite and the Orders
sample). The workspace quality gate is `make ci`: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --workspace`, `cargo test --workspace`.

The foundational, platform and starter tiers are fully implemented,
including the PyFly-parity layer — `firefly-container` (DI),
`firefly-aop` (aspect advice), `firefly-session`, `firefly-shell`,
`firefly-websocket`, `firefly-cli`, and the extensions to
`firefly-web` / `firefly-security` / `firefly-observability` /
`firefly-actuator` / `firefly-config` / `firefly-orchestration`.

The infrastructure adapters ship **real and wired**:
`firefly-cache-redis`, `firefly-eda-{kafka,rabbitmq,postgres,redis}`,
and `firefly-notifications-smtp` each drive their backing library and
pass an in-process test suite (live-broker round-trips gated behind
`#[ignore]`). The vendor adapters are likewise mostly real now —
Keycloak (OIDC + admin REST), Azure AD (Microsoft Graph), AWS Cognito
(JSON API + SigV4), DocuSign / Adobe Sign / Logalty (real REST), S3 /
Azure Blob (real object stores), and Twilio / Firebase (real providers).
The remaining SaaS channels (SendGrid, Resend) carry their locked ports
and fail loud with typed not-implemented errors until wired.

Two crates ship as **port-pending placeholders** reserved on the
workspace graph for the next wave — `firefly-cache-postgres` (a
Postgres-backed `cache::Adapter`) and `firefly-starter-web` (a
web-stack starter bundling `starter-core` + web middleware + security +
actuator). Each compiles and carries its locked dependency set; the
implementation lands without disturbing the established wire contract.

See [`MODULES.md`](MODULES.md) for the per-crate Full / Stub status.

---

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
