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

The Firefly Framework provides the cross-cutting machinery that every
non-trivial business service needs — RFC 7807 error envelopes,
idempotency, correlation propagation, CQRS, event-driven messaging,
event sourcing, sagas, configuration servers, identity adapters,
document management, notifications, callbacks, webhooks — behind a
single, opinionated composition pattern.

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
- **Honest about boundaries.** Every public method either runs real
  code or returns a typed `…NotImplemented`-style error with an
  actionable message documenting why the underlying provider is not yet
  wired. There are no silent stubs. See [`MODULES.md`](MODULES.md) for
  per-crate status.

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
│  config        │   │  eda             │   │  callbacks     │   │  backoffice          │
│  i18n          │   │  eventsourcing   │   │  webhooks      │   │                      │
│                │   │  orchestration   │   │  config-server │   │                      │
│                │   │  rule-engine     │   │                │   │                      │
│                │   │  plugins         │   │                │   │                      │
│                │   │  lifecycle       │   │                │   │                      │
│                │   │  actuator        │   │                │   │                      │
│                │   │  scheduling      │   │                │   │                      │
│                │   │  resilience      │   │                │   │                      │
│                │   │  security        │   │                │   │                      │
│                │   │  migrations      │   │                │   │                      │
│                │   │  openapi         │   │                │   │                      │
│                │   │  sse             │   │                │   │                      │
│                │   │  transactional   │   │                │   │                      │
│                │   │  testkit         │   │                │   │                      │
└────────────────┘   └──────────────────┘   └────────────────┘   └──────────────────────┘
```

Each tier may depend on the tiers to its left, never to its right. The
Cargo crate graph enforces the layering — every internal dependency is
declared once in `[workspace.dependencies]` and there is no path that
bypasses it.

See [`MODULES.md`](MODULES.md) for the full per-crate catalogue and
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design rationale.

---

## Workspace layout

One Cargo workspace, 52 members — the same shape as the Go port's
`go.work`:

```
fireflyframework-rust/
├── crates/             # 50 framework crates (firefly-<name>), one per Go module
│   ├── kernel/         #   each with its own README.md + test suite
│   ├── web/
│   ├── cqrs/
│   ├── …
│   └── backoffice/
├── tests/integration/  # cross-crate integration suite
├── samples/orders/     # reference service (firefly-sample-orders)
├── docs/               # ARCHITECTURE, CONFIGURATION, MIGRATION-GUIDE, DESIGN
└── Cargo.toml          # workspace root — version 26.6.1, edition 2021, MSRV 1.85
```

---

## Quickstart

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

The framework ships **52 workspace members** across the four tiers
(50 crates under `crates/` plus the integration suite and the Orders
sample). The workspace quality gate is `make ci`: `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo build --workspace`, `cargo test --workspace`.

Foundational, platform and starter tiers are fully implemented.
Adapter-tier integrations against external SaaS providers (Keycloak
admin REST, Azure Graph, AWS Cognito, DocuSign / Adobe Sign / Logalty,
SendGrid / Resend / Twilio / Firebase, S3 / Azure Blob, Kafka,
RabbitMQ) ship in this release as port-only stubs returning typed
not-implemented errors — the contract is locked, the wire is in scope
for a follow-up release, matching the Go port's adapter status.

See [`MODULES.md`](MODULES.md) for the per-crate Full / Stub status.

---

## License

Apache 2.0 — see [`LICENSE`](LICENSE).
