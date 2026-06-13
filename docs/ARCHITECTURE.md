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

> **Parity philosophy: byte-stable core + additive PyFly layer.** The
> Go-parity core (foundational, platform, starter tiers) is the
> framework's wire contract, and it is kept **byte-stable** — every JSON
> shape, header name, and signature stays identical across the sibling
> ports. On top of that, the Rust port carries a **PyFly-parity layer**
> that is *purely additive*: new crates (`firefly-container`,
> `firefly-aop`, `firefly-session`, `firefly-shell`, `firefly-websocket`,
> `firefly-cli`, `firefly-admin`), the ergonomic front door (`firefly`
> facade + `firefly-macros`), the real infrastructure adapters
> (`firefly-data-sqlx`, `firefly-data-mongodb`, `firefly-cache-redis`,
> `firefly-eda-{kafka,rabbitmq,postgres,redis}`, `firefly-notifications-smtp`,
> `firefly-session-{redis,postgres}`), and extensions to existing crates
> (`firefly-web`, `firefly-security`, `firefly-observability`,
> `firefly-actuator`, `firefly-config`, `firefly-orchestration`,
> `firefly-eda`). None of these change an established wire format; each
> existing crate's README has a "pyfly parity" section delimiting the
> additive surface from the Go-parity surface. Where pyfly relies on
> Python runtime reflection (decorators, autowiring, monkey-patching),
> the Rust port substitutes an explicit, type-safe equivalent
> (builders, factory closures, call-site weaving) documented per crate.

```
FOUNDATIONAL → PLATFORM → ADAPTERS → STARTERS
```

A crate never depends on a crate from a tier to its right. The Cargo
crate graph enforces this — every internal dependency is declared once
in the root `[workspace.dependencies]` table, member crates reference
only `{ workspace = true }`, and there is no patch or path override
that bypasses the layering.

## Workspace of crates

The Rust port is a single Cargo workspace of **76 members**: **72
crates** under `crates/` (named `firefly-<dir>`, hyphenation following
the Java repo names), plus `tests/integration`, `samples/orders`,
`samples/reactive-banking`, and `samples/macro-quickstart`. The Go-parity
core matches the Go port's module set one-for-one; the remaining crates
are the reactive core (`firefly-reactive`), the ergonomic front door (the
`firefly` facade + `firefly-macros`), and the PyFly-parity layer (DI / AOP /
sessions / shell / websockets / CLI / admin and the real infrastructure
adapters — including the hexagonal database adapters `firefly-data-sqlx` /
`firefly-data-mongodb` and the distributed session registries
`firefly-session-redis` / `firefly-session-postgres`). One version
(`26.6.3`), one edition (2021), one MSRV (1.85) — set once in
`[workspace.package]` and inherited by every member.

The runtime stack is deliberate and small:

| Concern        | Crate(s)                                          |
|----------------|---------------------------------------------------|
| Async runtime  | `tokio` (multi-thread, signal-aware)              |
| HTTP server    | `axum` 0.7 (`ws` feature) + `tower` layers + `axum-server` (TLS) |
| HTTP client    | `reqwest`                                         |
| Serialization  | `serde` / `serde_json` / `serde_yaml` / `quick-xml` |
| Errors         | `thiserror`                                       |
| Async ports    | `async-trait` (object-safe `dyn` traits)          |
| Crypto / TLS   | RustCrypto (`sha2`, `hmac`, `aes-gcm`), `bcrypt`, `jsonwebtoken`, `rustls` |
| Logging        | `tracing` / `tracing-subscriber` (JSON)           |
| SQL (dev/test) | `rusqlite` (bundled) — the role Go gave `modernc.org/sqlite` |
| Declarative macros | `syn`, `quote`, `proc-macro2`, `darling` (in `firefly-macros`) |
| CLI / templates| `clap`, `minijinja`, `include_dir`                |
| Infra adapters (optional) | `sqlx` (pg/mysql/sqlite), `mongodb`, `redis`, `rdkafka`, `lapin`, `tokio-postgres`, `lettre` — pulled in only by the adapter crate that uses them |

## Foundational tier

Primitives every service uses, no transitive infrastructure dependencies.

| Crate                | Purpose                                                                               |
|----------------------|----------------------------------------------------------------------------------------|
| `firefly-reactive`   | The `Mono<T>` / `Flux<T>` reactive core — the Project Reactor / WebFlux analog: lazy `FireflyError`-typed publishers, `Scheduler`, `FluxSink`, `Backoff`, the full operator surface. Every reactive surface above is built on it |
| `firefly-kernel`     | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, `FireflyError` hierarchy, task-local correlation |
| `firefly-utils`      | Try helpers, retry with exponential backoff + jitter, slug, AES-256-GCM crypto, template rendering |
| `firefly-validators` | IBAN (mod-97), BIC, Luhn, credit card, E.164 phone, currency (ISO 4217), email, password strength, sort code, VAT, Spanish DNI/NIE/NIF |
| `firefly-web`        | Problem-Details renderer, correlation layer, idempotency layer (pluggable store), PII masking, and the reactive `MonoJson`/`NdJson`/`Sse`/`SseEvents` responders (NDJSON/SSE streaming with backpressure) |
| `firefly-config`     | Layered Static / YAML / Env / Flag sources, profile selection, serde-driven binder; `${...}` placeholders, reload/refresh, masked property sources, config-server client |
| `firefly-i18n`       | Locale-aware message bundles, Accept-Language picker, region→language fallback        |
| `firefly-session`    | Server-side HTTP `Session` + `SessionStore` + `SessionLayer` (cookie load/save, rotation, HMAC signing, concurrency control) |

## Platform tier

The infrastructure layer.

| Crate                  | Purpose                                                                       |
|------------------------|-------------------------------------------------------------------------------|
| `firefly-cache`        | `Adapter` trait port + Memory / NoOp / Fallback implementations + typed `Typed<T>` |
| `firefly-observability`| `tracing` + correlation enrichment, health composite, startup banner          |
| `firefly-data`         | The storage-agnostic persistence **ports**: `Filter` DSL + composable `Specification`, `SqlDialect` (`Postgres`/`MySql`/`Sqlite`) + `Specification::to_mongo()`, `Page<T>` envelope, `Repository<T, K>` + memory impl, auditing + soft-delete, and the reactive `ReactiveCrudRepository<T, ID>` / `ReactiveSpecificationRepository` (memory + real `PostgresReactiveRepository`). Adapters live in `data-sqlx` / `data-mongodb` |
| `firefly-cqrs`         | Generic command/query `Bus`, `TypeId`-dispatched handlers, validation + caching middleware, reactive `send_mono`/`query_mono` |
| `firefly-eda`          | `Event` envelope, `Publisher`/`Subscriber`, in-memory broker, reactive `subscribe_reactive` → `Flux<Event>`, real transports in `eda-*` |
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
| `firefly-websocket`    | WebSocket server over axum: `WsSession`, `WebSocketHandler`, `ws_route`, topic `BroadcastHub` |
| `firefly-transactional`| `with_tx` over pluggable `Database` / `Transaction` / `Executor` ports, nested-tx participation |
| `firefly-testkit`      | HMAC signers (Stripe / GitHub / HMAC / Twilio), `SpyBroker`, JSON test helpers |
| `firefly-container`    | Opt-in `TypeId`-keyed DI container (service locator): factory closures, scopes, trait-object bindings, providers |
| `firefly-aop`          | Aspect-oriented advice: `Pointcut` matcher, `JoinPoint`, `Aspect`, `intercept` chain with explicit call-site weaving |
| `firefly-shell`        | Spring-Shell-style CLI framework: `CommandSpec`, `StdShell` parser + REPL, `CommandLineRunner` / `ApplicationRunner` |

## Adapter tier

Pluggable integrations. Each port lives in a parent crate; concrete
provider adapters live in dedicated crates so consumers only pull in
the vendor SDKs they actually use. Ports are `async_trait` object-safe
traits, injected as `Arc<dyn Trait>` at wiring time.

| Parent / port            | Default impl in crate                                | Provider adapters                                        |
|--------------------------|------------------------------------------------------|---------------------------------------------------------|
| `firefly-data`           | in-memory `Repository` / `ReactiveMemoryRepository` + real `PostgresReactiveRepository` | **real:** `data-sqlx` (Postgres / MySQL / SQLite over `sqlx`), `data-mongodb` (MongoDB) — same ports |
| `firefly-session`        | `MemorySessionRegistry` (in-process)                 | **real:** `session-redis` (`RedisSessionRegistry`), `session-postgres` (`PostgresSessionRegistry`) — distributed |
| `firefly-client`         | REST builder (reqwest, retry, problem decode) + reactive `WebClient` (`body_to_mono`/`body_to_flux`) | SOAP, gRPC, WebSocket scaffolds                          |
| `firefly-config-server`  | Spring-Cloud-Config-compatible handler + memory store| —                                                       |
| `firefly-idp`            | `firefly-idp-internal-db` (bcrypt + HS256 JWT)       | **real:** `idp-keycloak`, `idp-azure-ad`, `idp-aws-cognito` |
| `firefly-ecm`            | local-fs `ContentStore` + in-memory document service | **real:** `ecm-storage-aws` (S3), `ecm-storage-azure` (Blob), `ecm-esignature-docusign`, `ecm-esignature-adobe-sign`, `ecm-esignature-logalty` |
| `firefly-notifications`  | Memory channel + dispatcher                          | **real (all):** `notifications-smtp`, `-sendgrid`, `-resend`, `-twilio`, `-firebase` |
| `firefly-cache`          | `MemoryAdapter` / `NoOpAdapter` / `FallbackAdapter`  | **real:** `cache-redis` (`RedisAdapter`), `cache-postgres` (`PostgresCacheAdapter`) |
| `firefly-eda`            | `InMemoryBroker`                                     | **real:** `eda-kafka`, `eda-rabbitmq`, `eda-postgres` (outbox), `eda-redis` (Streams) |
| `firefly-callbacks`      | Full impl (HMAC-signing dispatcher + audit + REST admin + SDK) | —                                             |
| `firefly-webhooks`       | Full impl (HMAC / Stripe / GitHub / Twilio validators + pipeline + DLQ + ingest endpoint + SDK) | —            |

### EDA transport-adapter pattern

`firefly-eda` defines the `Publisher` / `Subscriber` / `Broker` ports
and ships only the in-process `InMemoryBroker`. Each production
transport is an *independent leaf crate* implementing the same ports
over a real broker library:

```
                    firefly_eda::Broker  (port)
                            ▲
        ┌───────────┬───────┴───────┬──────────────┬─────────────┐
   InMemoryBroker  KafkaBroker  RabbitMqBroker  PostgresBroker  RedisStreamsBroker
   (in eda)        (rdkafka)    (lapin)         (tokio-postgres) (redis Streams)
```

A service codes against `Arc<dyn Broker>` and selects the backend at
wiring time. The `firefly-eda` `new_kafka_broker` / `new_rabbitmq_broker`
factories return typed `EdaError::{KafkaUnavailable, RabbitMqUnavailable}`
sentinels when no transport crate is linked, so a misconfigured
deployment fails loud rather than silently using in-memory. The
`Event` JSON envelope is byte-identical across every transport (and
across the sibling ports), so producers and consumers interoperate
regardless of broker. The same pattern backs `firefly-cache` →
`firefly-cache-redis` and the `firefly-notifications` `Channel` →
`firefly-notifications-smtp`. Because each adapter is a leaf crate, its
heavy SDK dependency stays out of services that don't select it.

### Hexagonal database-adapter pattern

The persistence layer is the clearest expression of the port/adapter
("hexagonal") split. `firefly-data` is the **ports** crate — it owns no driver
and implies no SQL engine. It defines:

- the storage-agnostic query model: the `Filter` DSL and the composable
  `Specification` (`Pred` / `And` / `Or` / `Not`);
- the repository traits: `Repository<T, K>`, `ReactiveCrudRepository<T, ID>`,
  and `ReactiveSpecificationRepository`;
- the lowering surface: a `SqlDialect` trait with three shipped impls
  (`PostgresDialect` / `MySqlDialect` / `SqliteDialect`) that render the *same*
  query tree per backend (`Filter::to_sql_with` / `Specification::to_sql_with`,
  with placeholder style, identifier quoting, `IN`-list shape, and
  case-insensitive `LIKE` all dialect-correct), plus `Specification::to_mongo()`
  / `Filter::to_mongo()` for document stores;
- the cross-cutting policies every backend reuses: auditing (`Auditor` /
  `AuditStamps` / `UserProvider`) and soft-delete (`SoftDeletePolicy`).

```
                     firefly_data  (ports: Specification / Repository / SqlDialect)
                            ▲
        ┌───────────────────┼───────────────────────────┐
   PostgresReactive    firefly-data-sqlx           firefly-data-mongodb
   Repository          SqlxRepository /             MongoRepository<T, ID>
   (tokio-postgres,    SqlxReactiveRepository       (mongodb driver;
    in firefly-data)   (sqlx: pg / mysql / sqlite;   Specification::to_mongo())
                        SqlDialect picked at runtime
                        from the pool's Backend)
```

A service codes once against the `firefly-data` repository traits and the
`Specification` tree; swapping Postgres for MySQL, SQLite, or MongoDB is a
swap of the adapter constructor, with no change to the call sites. Adding a
**new** database is therefore "write an adapter that implements the ports",
not "rewrite the data layer": `firefly-data-sqlx` proves it for relational
backends (one codebase, three dialects) and `firefly-data-mongodb` proves it
for a document store (the very same `Specification` lowered via `to_mongo()`).
Both adapters auto-apply the auditing and soft-delete policies, so those
semantics are identical regardless of backend, and both stream reads lazily as
a `Flux<T>`. Each adapter's heavy driver (`sqlx`, the `mongodb` crate) stays
out of services that don't select it.

The `firefly-session` `SessionRegistry` port follows the same shape: the
in-process `MemorySessionRegistry` is the default, and `firefly-session-redis`
/ `firefly-session-postgres` are distributed adapters that make the
per-principal concurrency cap hold cluster-wide.

## Operations: the admin dashboard

`firefly-admin` is the Spring-Boot-Admin-style embedded dashboard — the
Rust rendering of pyfly's `admin` package. Architecturally it is a thin
**read-mostly aggregation layer** over the management primitives already
in the framework, plus a static single-page app:

- **No new data plane.** The dashboard's JSON API reads from
  `firefly-actuator` (health composites, the `MetricRegistry`, loggers,
  scheduled tasks, caches, HTTP exchanges), `firefly-cqrs` (bus
  introspection), and `firefly-orchestration` (execution / transaction
  state). It owns only two small in-process buffers of its own — a ring
  buffer of recent traces and a captured-log buffer.
- **SPA + JSON API + SSE.** The static assets (embedded with
  `include_dir`) render the views — overview, health, metrics, loggers,
  mappings, caches, scheduled tasks, traces, CQRS, transactions, beans,
  config, instances — driven by a `/admin/api/*` JSON surface and
  `firefly-sse` live streams on the configured `refresh_interval`.
- **Server / client modes.** In *server mode* an `InstanceRegistry`
  aggregates several downstream services seeded from
  `firefly.admin.server.instances`; in *client mode* a service
  self-registers with a remote admin server on lifecycle start and
  deregisters on stop (`firefly.admin.client.*`).
- **Mounting + auth.** Mounted under `firefly.admin.path` (default
  `/admin`); when `firefly.admin.require_auth` is on, every
  `/admin/api/*` route is guarded by a `firefly-security`
  `Authentication` carrying one of `firefly.admin.allowed_roles`. The
  `AdminConfig` / `AdminServerConfig` / `AdminClientConfig` structs bind
  straight from a `firefly-config` document.

## Starter tier

One-call composition.

| Starter                      | Bundles                                                            |
|------------------------------|--------------------------------------------------------------------|
| `firefly-starter-core`       | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| `firefly-starter-application`| starter-core + plugins registry                                    |
| `firefly-starter-domain`     | starter-core + in-memory event-sourcing stores                     |
| `firefly-starter-data`       | starter-core (consumer supplies its own DB)                        |
| `firefly-starter-web`        | `WebStack` — `Core` + CORS + security headers + request metrics + access log (web batteries on by default), optional `FilterChain` security |
| `firefly-backoffice`         | starter-application + back-office context middleware               |

Each starter ships an embedded banner printed at startup (via
`Core::print_banner`) naming the active starter, the application name
and the resolved Rust runtime — mirroring the Spring Boot
`banner-on-start` behavior and the Go port's `observability.PrintBanner`.

## The front door: facade + declarative macros

On top of the tiers above sits the **ergonomics layer** — the
Spring-Boot-starter developer experience. It is two crates that add no
runtime types of their own; they make the existing crates pleasant to consume.

**`firefly` — the one-dependency facade.** Rather than list ten-to-fifteen
`firefly-*` crates and import from each, a service depends on `firefly` alone
and writes `use firefly::prelude::*;`. The facade is a pure re-export crate:

- the **prelude** surfaces the high-frequency types (`Bus`, `Container`,
  `Scheduler`, `Saga`/`Step`, `Application`/`ShutdownHandle`, `Core`/`CoreConfig`,
  `WebResult`/`WebError`/`problem_response`, `FireflyError`/`FireflyResult`,
  `Mono`/`Flux`) plus every macro;
- ergonomic **per-crate aliases** (`firefly::cqrs`, `firefly::web`, …) drop the
  `firefly_` prefix;
- the hidden **`__rt` contract path** re-exports every runtime crate under its
  exact crate name (`firefly::__rt::firefly_cqrs`, …) — a proc-macro crate
  cannot re-export runtime types, so generated code needs one stable absolute
  path it can always reach. This is the contract between the macros and the
  runtime;
- heavy adapters (`data-sqlx`, `data-mongodb`, `eda-*`, `cache-*`, `admin`,
  `full`) are **opt-in cargo features**. The default build compiles only the
  framework's lean port crates, so the front-door dependency never drags in a
  database or broker driver you did not ask for. Each optional alias and its
  `__rt` entry are gated behind the matching feature.

**`firefly-macros` — the declarative layer.** A `proc-macro` crate of
derive/attribute macros (`syn` / `quote` / `darling`) that collapse the
framework's closure/builder wiring into declarations next to the code they
describe:

| Macro | On | Generates |
|-------|----|-----------|
| `#[derive(Command)]` / `#[derive(Query)]` | a message struct | `impl firefly_cqrs::Message` (`#[firefly(validate)]` / `#[firefly(cache_ttl)]`) |
| `#[command_handler]` / `#[query_handler]` | `async fn(Msg) -> Result<R, CqrsError>` | a `register_<fn>(bus)` helper |
| `#[derive(Component/Service/Repository)]` + `register_all!` | a struct with `#[autowired]` fields | a `firefly_register(container)` method |
| `#[scheduled]` | a zero-arg `async fn` | a `schedule_<fn>(scheduler)` helper |
| `#[rest_controller]` + `#[get/post/put/delete/patch]` | an `impl` block | a `routes(state) -> axum::Router` |
| `#[derive(DomainEvent)]` / `#[derive(AggregateRoot)]` | a struct | event-type / aggregate ergonomics |
| `#[event_listener]` | `async fn(Event) -> FireflyResult<()>` | a `subscribe_<fn>(broker)` helper |

Because generated code addresses runtime types through `::firefly::__rt::…`
(overridable per macro with `#[firefly(crate = "…")]`), a service that depends
only on `firefly` — plus the `axum`/`serde` it writes against anyway — compiles
whatever a macro expands to. Rust has no package scanning or reflective
autowiring, so DI registration is still explicit (`register_all!` lists the
components) and free-fn handlers publish their wiring state explicitly; the
macros remove the *mechanical* boilerplate, not the explicitness. The
`samples/macro-quickstart` service is the reference: the orders behaviour in
376 source lines vs the builder-style `orders` sample's 1022.

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

## Dependency injection (`firefly-container`)

The framework's default composition idiom is **explicit construction** —
`Arc<dyn Trait>` handles threaded through constructors, the same shape
the rest of this document describes. `firefly-container` is an
**opt-in** addition for teams that prefer a service-locator surface
(porting a pyfly/Spring service that leans on a DI container), never a
requirement: nothing in the Go-parity core or the starters depends on
it.

It is a `TypeId`-keyed registry behind `RwLock`s (so a `Container` is
`Send + Sync` and shares as `Arc<Container>`). The half of pyfly's
container that has a faithful Rust analog — the service locator — is
ported directly: `register_factory::<T>(scope, f)`, `resolve::<T>()`,
`resolve_all::<T>()`, named beans, `Provider<T>` deferred handles,
primary/order, and `Scope` (Singleton / Prototype / Request / Session /
custom via the `ScopeHandler` SPI). The half that depends on Python
runtime reflection is *adapted, not faked*:

- **Reflective autowiring → explicit factory closures.** A
  `register_factory` closure resolves its own dependencies by calling
  `resolve` — Rust has no constructor type-hint introspection.
- **Package scanning + stereotype decorators → dropped.** Registration
  is explicit; there is no `importlib`-style auto-discovery.
- **Trait-object bindings** work because `TypeId::of::<dyn Trait>()` is a
  valid key: `bind::<dyn Trait, Impl>(coerce)` registers an impl under
  the trait's id with a caster, so `resolve::<dyn Trait>()` and
  `resolve_all::<dyn Trait>()` behave as in pyfly.
- **Circular dependencies** are caught by a thread-local resolution
  stack (mirroring pyfly's `_resolving`).

This is deliberately the *explicit* end of the DI spectrum: no runtime
magic, every wiring visible in source.

## Aspect-oriented programming (`firefly-aop`)

`firefly-aop` ports pyfly's `aop` package — Spring-style advice
(`before` / `around` / `after_returning` / `after_throwing` / `after`)
over a `Pointcut` glob matcher on dot-segmented qualified names
(`service.OrderService.create`). An `AspectRegistry` holds ordered
`AdviceBinding`s; `intercept(&registry, type, method, args, invocation)`
runs the advice chain around the captured original call.

The key architectural decision is **explicit weaving at the call site**.
pyfly's weaver monkey-patches live bean methods via `setattr`, driven by
an `AspectBeanPostProcessor` over the DI container — none of which has a
Rust analog (no runtime method mutation, no descriptor protocol, no bean
container to post-process). Instead the call site wraps the original
call in an `Invocation` and routes it through `intercept`. Non-matching
methods cost nothing: if no binding matches the qualified name,
`intercept` runs the invocation with zero advice overhead. Args and
results are type-erased to `Arc<dyn Any + Send + Sync>` (advice
downcasts when it needs the concrete type) — the Rust equivalent of
pyfly's dynamic typing. For HTTP-edge and bus-dispatch cross-cutting
concerns, the framework still prefers `firefly-web`'s tower layers and
`firefly-cqrs`'s `Middleware`; `firefly-aop` targets pattern-matched
advice over arbitrary service methods.

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
`(T, error)` + channels. The Rust port ships a **first-class reactive
core** — `firefly-reactive`'s `Mono<T>` / `Flux<T>` — that is the
faithful Reactor analog, *and* it interoperates with plain async Rust,
so authors pick the level that fits.

The **reactive translation** (first-class types):

| Java (Reactor)               | firefly-reactive                                              |
|------------------------------|---------------------------------------------------------------|
| `Mono<T>`                    | `Mono<T>` (lazy, `FireflyError`-typed)                        |
| `Flux<T>`                    | `Flux<T>` (lazy, terminal error)                             |
| `Mono.empty()` / `onComplete`| `Ok(None)` from a `Mono`                                      |
| `Mono.error(...)`            | `Mono::error(FireflyError::...)` / a terminal `Err`           |
| `Mono.block()`               | `Mono::block` — `async`, never parks a Tokio worker          |
| `Schedulers.{immediate,parallel,boundedElastic}` | `Scheduler::{Immediate,Parallel,BoundedElastic}` |
| `Retry.backoff(..)`          | `Backoff` + `*::retry_backoff`                               |
| `Mono.timeout(...)`          | `Mono::timeout` / `Flux::timeout` (→ 504 `FireflyError`)      |
| `Flux.onBackpressureBuffer`  | `Flux::on_backpressure_buffer` (bounded channels underneath)  |
| `FluxSink` / `Flux.create`   | `FluxSink` / `Flux::create`                                   |
| `Mono.toFuture()` / `Flux.toStream()` | `Mono::into_future` / `Flux::into_stream` (escape hatches) |

The **plain-async translation** still applies wherever a service prefers
ordinary `async`/`await` over the reactive types (most internal code):

| Java (Reactor)               | Rust idiom                                                     |
|------------------------------|----------------------------------------------------------------|
| `Mono<T>`                    | `async fn(..) -> FireflyResult<T>`                             |
| `Flux<T>`                    | `impl Stream<Item = T>` (`futures` / `tokio-stream`)           |
| `Mono.deferContextual(...)`  | Task-local read (`correlation_id()`) or explicit handle        |
| Subscribers                  | Spawned tasks (`tokio::spawn`)                                 |
| Cancellation                 | Future drop + `CancellationToken` for cooperative engines      |

The reactive types convert to and from raw `Stream` / `Future` at the
edges (`Flux::from_stream` / `Mono::from_future` in, `into_stream` /
`into_future` out), so the two styles compose freely.

## Dependency waves (build order)

The members build in dependency-ordered waves; each wave depends only on the
waves before it:

```
Wave 1 ── zero internal deps:
  kernel, utils, validators, config, i18n, cache, data, cqrs,
  eventsourcing, orchestration, rule-engine, plugins, lifecycle,
  actuator, scheduling, resilience, security, migrations, openapi,
  sse, transactional, testkit, config-server, idp, ecm, notifications,
  container, aop, shell, macros                 (PyFly: stand-alone)
        │
Wave 2 ── kernel-dependent:
  web, observability, eda, client, session, websocket
        │
Wave 3 ── adapters + aggregate:
  callbacks, webhooks            (→ client)
  idp-internal-db, idp-keycloak,
  idp-azure-ad, idp-aws-cognito  (→ idp)
  ecm-storage-*, ecm-esignature-* (→ ecm)
  notifications-smtp,
  notifications-*                (→ notifications)
  cache-redis, cache-postgres    (→ cache)
  eda-kafka, eda-rabbitmq,
  eda-postgres, eda-redis        (→ eda)
  data-sqlx, data-mongodb        (→ data + reactive)
  session-redis, session-postgres (→ session)
  cli                            (→ openapi/templates)
  starter-core                   (→ wave-2 set)
        │
Wave 4 ── composition + front door:
  starter-application, starter-domain, starter-data, starter-web,
  backoffice,
  admin                          (→ actuator + cqrs + orchestration + sse + security),
  firefly                        (facade → every runtime crate + macros + feature-gated adapters),
  tests/integration, samples/orders, samples/reactive-banking,
  samples/macro-quickstart       (→ firefly)
```

## Versioning

Calendar-versioned, expressed as valid semver (`YY.M.PATCH`) — kept in
lock-step with the Java, .NET, Go, and Python releases. The current
version is exposed as `firefly_kernel::VERSION = "26.6.3"` and set once
in the workspace `Cargo.toml`.
