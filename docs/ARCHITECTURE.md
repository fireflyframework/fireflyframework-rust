# Architecture

The Rust port of Firefly Framework mirrors the layering enforced by the
Java reactor, the .NET solution, and the Go workspace: four tiers,
left-to-right dependency direction, each tier strictly above the one to
its right.

> **Spring / Go parity.** The Rust port matches the conceptual surface
> of the Spring Boot stack — and module-for-module the Go port, which
> is the canonical compiled-language reference — where it makes sense
> in idiomatic Rust: typed configuration binding (`firefly-config`),
> application orchestration (`firefly-lifecycle`), management endpoints
> (`firefly-actuator`), task scheduling (`firefly-scheduling`),
> resilience patterns (`firefly-resilience`), HTTP-layer authn/authz
> (`firefly-security`), SQL migrations (`firefly-migrations`), OpenAPI
> generation (`firefly-openapi`), internationalization
> (`firefly-i18n`), Server-Sent Events (`firefly-sse`), transactional
> helpers (`firefly-transactional`), and a shared testing toolkit
> (`firefly-testkit`). Rust's lack of a JVM container means we express
> dependency injection as explicit construction (`Arc<dyn Trait>`
> handles passed to constructors) and `tower` middleware composition
> rather than a runtime bean factory — but the public contract on the
> wire is identical to Java, .NET, Go, and Python.

```
FOUNDATIONAL → PLATFORM → ADAPTERS → STARTERS
```

A crate never depends on a crate from a tier to its right. The Cargo
crate graph enforces this — every internal dependency is declared once
in the root `[workspace.dependencies]` table, member crates reference
only `{ workspace = true }`, and there is no patch or path override
that bypasses the layering.

## Workspace of crates

Where the Go port is a `go.work` of 52 modules, the Rust port is a
single Cargo workspace of 52 members: 50 crates under `crates/` (named
`firefly-<dir>`, hyphenation following the Java repo names), plus
`tests/integration` and `samples/orders`. One version (`26.6.1`), one
edition (2021), one MSRV (1.85) — set once in `[workspace.package]`
and inherited by every member.

The runtime stack is deliberate and small:

| Concern        | Crate(s)                                          |
|----------------|---------------------------------------------------|
| Async runtime  | `tokio` (multi-thread, signal-aware)              |
| HTTP server    | `axum` 0.7 + `tower` layers                       |
| HTTP client    | `reqwest`                                         |
| Serialization  | `serde` / `serde_json` / `serde_yaml`             |
| Errors         | `thiserror`                                       |
| Async ports    | `async-trait` (object-safe `dyn` traits)          |
| Crypto         | RustCrypto (`sha2`, `hmac`, `aes-gcm`), `bcrypt`, `jsonwebtoken` |
| Logging        | `tracing` / `tracing-subscriber` (JSON)           |
| SQL (dev/test) | `rusqlite` (bundled) — the role Go gave `modernc.org/sqlite` |

## Foundational tier

Primitives every service uses, no transitive infrastructure dependencies.

| Crate                | Purpose                                                                               |
|----------------------|----------------------------------------------------------------------------------------|
| `firefly-kernel`     | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, `FireflyError` hierarchy, task-local correlation |
| `firefly-utils`      | Try helpers, retry with exponential backoff + jitter, slug, AES-256-GCM crypto, template rendering |
| `firefly-validators` | IBAN (mod-97), BIC, Luhn, credit card, E.164 phone, currency (ISO 4217), email, password strength, sort code, VAT, Spanish DNI/NIE/NIF |
| `firefly-web`        | Problem-Details renderer, correlation layer, idempotency layer (pluggable store), PII masking |
| `firefly-config`     | Layered Static / YAML / Env / Flag sources, profile selection, serde-driven binder    |
| `firefly-i18n`       | Locale-aware message bundles, Accept-Language picker, region→language fallback        |

## Platform tier

The infrastructure layer.

| Crate                  | Purpose                                                                       |
|------------------------|-------------------------------------------------------------------------------|
| `firefly-cache`        | `Adapter` trait port + Memory / NoOp / Fallback implementations + typed `Typed<T>` |
| `firefly-observability`| `tracing` + correlation enrichment, health composite, startup banner          |
| `firefly-data`         | Generic `Filter` DSL, `Page<T>` envelope, `Repository<T, K>` + memory impl    |
| `firefly-cqrs`         | Generic command/query `Bus`, `TypeId`-dispatched handlers, validation + caching middleware |
| `firefly-eda`          | `Event` envelope, `Publisher`/`Subscriber`, in-memory broker, Kafka/RabbitMQ scaffolds |
| `firefly-eventsourcing`| Aggregate roots + event store (in-memory), snapshots, projection runner       |
| `firefly-orchestration`| `Saga` (sequential + reverse-order compensation), `Workflow` (DAG), `Tcc`     |
| `firefly-rule-engine`  | YAML DSL → AST → recursive evaluator (interfaces / models / core / web / sdk sub-modules) |
| `firefly-plugins`      | Lifecycle SPI + composite registry                                            |
| `firefly-lifecycle`    | `Application::run()` orchestrator with ordered hooks + signal-based drain     |
| `firefly-actuator`     | `/actuator/{health,info,metrics,env,tasks,version}` endpoints; counter / gauge registry |
| `firefly-scheduling`   | Cron parser + `Scheduler` with FixedRate, FixedDelay, Cron triggers           |
| `firefly-resilience`   | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, composable `Chain`    |
| `firefly-security`     | `Authentication` extension, `BearerLayer`, path-pattern `FilterChain` RBAC    |
| `firefly-migrations`   | Flyway-style versioned SQL migration runner over a `Database` port            |
| `firefly-openapi`      | OAS 3.1 spec generator from route descriptors, Swagger-UI shim                |
| `firefly-sse`          | Server-Sent Events writer with heartbeat + Last-Event-Id resumption           |
| `firefly-transactional`| `with_tx` over pluggable `Database` / `Transaction` / `Executor` ports, nested-tx participation |
| `firefly-testkit`      | HMAC signers (Stripe / GitHub / HMAC / Twilio), `SpyBroker`, JSON test helpers |

## Adapter tier

Pluggable integrations. Each port lives in a parent crate; concrete
provider adapters live in dedicated crates so consumers only pull in
the vendor SDKs they actually use. Ports are `async_trait` object-safe
traits, injected as `Arc<dyn Trait>` at wiring time.

| Parent / port            | Default impl in crate                                | Provider stubs                                        |
|--------------------------|------------------------------------------------------|-------------------------------------------------------|
| `firefly-client`         | REST builder (reqwest, retry, problem decode)        | SOAP, gRPC, WebSocket placeholders                    |
| `firefly-config-server`  | Spring-Cloud-Config-compatible handler + memory store| —                                                     |
| `firefly-idp`            | `firefly-idp-internal-db` (bcrypt + HS256 JWT)       | `idp-keycloak`, `idp-azure-ad`, `idp-aws-cognito`     |
| `firefly-ecm`            | local-fs `ContentStore` + in-memory document service | `ecm-storage-aws`, `ecm-storage-azure`, `ecm-esignature-docusign`, `ecm-esignature-adobe-sign`, `ecm-esignature-logalty` |
| `firefly-notifications`  | Memory channel + dispatcher                          | `notifications-sendgrid`, `notifications-resend`, `notifications-twilio`, `notifications-firebase` |
| `firefly-callbacks`      | Full impl (HMAC-signing dispatcher + audit + REST admin + SDK) | —                                           |
| `firefly-webhooks`       | Full impl (HMAC / Stripe / GitHub / Twilio validators + pipeline + DLQ + ingest endpoint + SDK) | —          |

## Starter tier

One-call composition.

| Starter                      | Bundles                                                            |
|------------------------------|--------------------------------------------------------------------|
| `firefly-starter-core`       | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| `firefly-starter-application`| starter-core + plugins registry                                    |
| `firefly-starter-domain`     | starter-core + in-memory event-sourcing stores                     |
| `firefly-starter-data`       | starter-core (consumer supplies its own DB)                        |
| `firefly-backoffice`         | starter-application + back-office context middleware               |

Each starter ships an embedded banner printed at startup (via
`Core::print_banner`) naming the active starter, the application name
and the resolved Rust runtime — mirroring the Spring Boot
`banner-on-start` behavior and the Go port's `observability.PrintBanner`.

## Context propagation

Go threads correlation ids, tenants and transactions through
`context.Context`. Rust has no ambient context, so the port uses two
explicit mechanisms:

- **Task-local scopes** for ambient request metadata:
  `firefly_kernel::with_correlation_id(id, fut)` scopes a correlation
  id over a future; `correlation_id()` reads it anywhere downstream.
  Nested scopes shadow like child contexts. HTTP propagation stays
  header-based (`X-Correlation-Id`), applied by `CorrelationLayer`.
- **Explicit handle types** where the value is load-bearing:
  `firefly_transactional::TxContext` carries the active transaction,
  `firefly_orchestration::CancellationToken` carries cooperative
  cancellation — the Rust shape of `ctx.Done()`.

## Error model

`firefly-kernel` defines a `thiserror`-derived `FireflyError` with code,
title, HTTP status, detail, structured fields, and an optional source
chain — the Rust analog of Go's `FireflyError` + `errors.Is/As`
traversal. Each crate layers its own `thiserror` enum on top
(`CqrsError`, `EdaError`, `CallbackError`, …) with `Display` strings
kept bytes-equal to the Go sentinels where wire or log parity matters.
`firefly_kernel::as_problem` renders any `std::error::Error` as an RFC
7807 `ProblemDetail`; `firefly-web`'s `WebResult<T>` lets handlers `?`
their way to a correct `application/problem+json` response.

## Reactive ↔ Rust translation

The Java framework is built on Project Reactor (`Mono`, `Flux`); the
.NET port uses `Task`/`IAsyncEnumerable`; the Go port uses
`(T, error)` + channels. Async Rust is Reactor's most natural analog —
the translation rules are:

| Java (Reactor)               | Rust idiom                                                     |
|------------------------------|----------------------------------------------------------------|
| `Mono<T>`                    | `async fn(..) -> FireflyResult<T>`                             |
| `Flux<T>`                    | `impl Stream<Item = T>` (`futures` / `tokio-stream`)           |
| `Mono.error(...)`            | `Err(FireflyError::...)`                                       |
| `Mono.deferContextual(...)`  | Task-local read (`correlation_id()`) or explicit handle        |
| Subscribers                  | Spawned tasks (`tokio::spawn`)                                 |
| `Mono.timeout(...)`          | `tokio::time::timeout(d, fut)`                                 |
| Backpressure (`Flux.onBackpressureBuffer`) | Bounded `mpsc` channels                          |
| Cancellation                 | Future drop + `CancellationToken` for cooperative engines      |

## Dependency waves (build order)

The 52 members build in four waves; each wave depends only on the
waves before it:

```
Wave 1 (26) ── zero internal deps:
  kernel, utils, validators, config, i18n, cache, data, cqrs,
  eventsourcing, orchestration, rule-engine, plugins, lifecycle,
  actuator, scheduling, resilience, security, migrations, openapi,
  sse, transactional, testkit, config-server, idp, ecm, notifications
        │
Wave 2 (4) ── kernel-dependent:
  web, observability, eda, client
        │
Wave 3 (16) ── adapters + aggregate:
  callbacks, webhooks            (→ client)
  idp-internal-db, idp-keycloak,
  idp-azure-ad, idp-aws-cognito  (→ idp)
  ecm-storage-*, ecm-esignature-* (→ ecm)
  notifications-*                (→ notifications)
  starter-core                   (→ wave-2 set)
        │
Wave 4 (6) ── composition:
  starter-application, starter-domain, starter-data, backoffice,
  tests/integration, samples/orders
```

## Versioning

Calendar-versioned, expressed as valid semver (`YY.M.PATCH`) — kept in
lock-step with the Java, .NET, Go, and Python releases. The current
version is exposed as `firefly_kernel::VERSION = "26.6.1"` and set once
in the workspace `Cargo.toml`.
