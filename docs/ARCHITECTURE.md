# Architecture

Firefly Framework is organized into four tiers with a strict
left-to-right dependency direction: each tier sits strictly above the
one to its right.

> **Design note.** Firefly is a batteries-included framework: it ships
> typed configuration binding (`firefly-config`), application
> orchestration (`firefly-lifecycle`), management endpoints
> (`firefly-actuator`), task scheduling (`firefly-scheduling`),
> resilience patterns (`firefly-resilience`), HTTP-layer authn/authz
> (`firefly-security`), SQL migrations (`firefly-migrations`), OpenAPI
> generation (`firefly-openapi`), internationalization
> (`firefly-i18n`), Server-Sent Events (`firefly-sse`), declarative
> transactions (`firefly-transactional`), and a shared testing toolkit
> (`firefly-testkit`) as first-class features. Dependency injection is
> expressed as explicit construction (`Arc<dyn Trait>` handles passed to
> constructors) and `tower` middleware composition rather than a runtime
> bean factory — the type system, not a reflective container, wires the
> graph.

> **Design note: stable core + additive capability crates.** Firefly's
> core (foundational, platform, starter tiers) defines the **wire
> contract** — stable JSON shapes, header names, and signatures that
> services and clients can rely on across releases. Around that core sit
> *additive* capability crates: `firefly-container`, `firefly-aop`,
> `firefly-session`, `firefly-shell`, `firefly-websocket`,
> `firefly-cli`, `firefly-admin`; the ergonomic front door (`firefly`
> facade + `firefly-macros`); the real infrastructure adapters
> (`firefly-data-sqlx`, `firefly-data-mongodb`, `firefly-cache-redis`,
> `firefly-eda-{kafka,rabbitmq,postgres,redis}`, `firefly-notifications-smtp`,
> `firefly-session-{redis,postgres}`); and extensions to existing crates
> (`firefly-web`, `firefly-security`, `firefly-observability`,
> `firefly-actuator`, `firefly-config`, `firefly-orchestration`,
> `firefly-eda`). None of these change an established wire format; each
> crate's README carries an additive-capabilities section delimiting that
> surface from the stable core. Where other ecosystems lean on runtime
> reflection (autowiring, live method rewriting), Firefly substitutes an
> explicit, type-safe equivalent — builders, factory closures, call-site
> weaving — documented per crate.

```
FOUNDATIONAL → PLATFORM → ADAPTERS → STARTERS
```

A crate never depends on a crate from a tier to its right. The Cargo
crate graph enforces this — every internal dependency is declared once
in the root `[workspace.dependencies]` table, member crates reference
only `{ workspace = true }`, and there is no patch or path override
that bypasses the layering.

## Workspace of crates

Firefly is a single Cargo workspace of **78 members**: **73
crates** under `crates/` (named `firefly-<dir>`), plus `tests/integration`
and the four samples — `samples/lumen` (the canonical end-to-end service the
book builds), `samples/orders`, `samples/reactive-banking`, and
`samples/macro-quickstart`.
The crates divide into the core (foundational / platform / starter tiers),
the reactive core (`firefly-reactive`), the ergonomic front door (the
`firefly` facade + `firefly-macros`), and the capability/adapter crates
(DI / AOP / sessions / shell / websockets / CLI / admin and the real
infrastructure adapters — including the hexagonal database adapters
`firefly-data-sqlx` / `firefly-data-mongodb` and the distributed session
registries `firefly-session-redis` / `firefly-session-postgres` /
`firefly-session-mongodb`). One
version (`26.6.4`), one edition (2021), one MSRV (1.85) — set once in
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
| SQL (dev/test) | `rusqlite` (bundled) — the embedded SQLite engine used by dev/test migrations and the transactional/migrations test suites |
| Declarative macros | `syn`, `quote`, `proc-macro2`, `darling` (in `firefly-macros`) |
| CLI / templates| `clap`, `minijinja`, `include_dir`                |
| Infra adapters (optional) | `sqlx` (pg/mysql/sqlite), `mongodb`, `redis`, `rdkafka`, `lapin`, `tokio-postgres`, `lettre` — pulled in only by the adapter crate that uses them |

## Foundational tier

Primitives every service uses, no transitive infrastructure dependencies.

| Crate                | Purpose                                                                               |
|----------------------|----------------------------------------------------------------------------------------|
| `firefly-reactive`   | The `Mono<T>` / `Flux<T>` reactive core: lazy `FireflyError`-typed publishers, `Scheduler`, `FluxSink`, `Backoff`, the full operator surface. Every reactive surface above is built on it |
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
| `firefly-transactional`| Async, declarative transactions: the object-safe `TransactionManager` port + `Propagation`/`Isolation`/`TxOptions` + the `transactional(...)` orchestrator (commit on `Ok`, rollback on `Err`); `with_tx` over pluggable `Database` / `Transaction` / `Executor` ports, nested-tx participation |
| `firefly-testkit`      | HMAC signers (Stripe / GitHub / HMAC / Twilio), `SpyBroker`, JSON test helpers |
| `firefly-container`    | Opt-in `TypeId`-keyed DI container (service locator): factory closures, scopes, trait-object bindings, providers |
| `firefly-aop`          | Aspect-oriented advice: `Pointcut` matcher, `JoinPoint`, `Aspect`, `intercept` chain with explicit call-site weaving |
| `firefly-shell`        | Interactive CLI framework: `CommandSpec`, `StdShell` parser + REPL, `CommandLineRunner` / `ApplicationRunner` |

## Adapter tier

Pluggable integrations. Each port lives in a parent crate; concrete
provider adapters live in dedicated crates so consumers only pull in
the vendor SDKs they actually use. Ports are `async_trait` object-safe
traits, injected as `Arc<dyn Trait>` at wiring time.

| Parent / port            | Default impl in crate                                | Provider adapters                                        |
|--------------------------|------------------------------------------------------|---------------------------------------------------------|
| `firefly-data`           | in-memory `Repository` / `ReactiveMemoryRepository` + real `PostgresReactiveRepository` | **real:** `data-sqlx` (Postgres / MySQL / SQLite over `sqlx`), `data-mongodb` (MongoDB) — same ports |
| `firefly-session`        | `MemorySessionRegistry` (in-process)                 | **real:** `session-redis` (`RedisSessionRegistry`), `session-postgres` (`PostgresSessionRegistry`), `session-mongodb` (`MongoSessionRegistry`) — distributed |
| `firefly-client`         | REST builder (reqwest, retry, problem decode) + reactive `WebClient` (`body_to_mono`/`body_to_flux`) | SOAP, gRPC, WebSocket scaffolds                          |
| `firefly-config-server`  | Centralized config-server handler (HTTP) + memory store| —                                                       |
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
`Event` JSON envelope is byte-identical across every transport, so
producers and consumers interoperate regardless of broker. The same
pattern backs `firefly-cache` →
`firefly-cache-redis` and the `firefly-notifications` `Channel` →
`firefly-notifications-smtp`. Because each adapter is a leaf crate, its
heavy SDK dependency stays out of services that don't select it.

### Hexagonal database-adapter pattern

The persistence layer is the clearest expression of the port/adapter
("hexagonal") split. `firefly-data` is the **ports** crate — it owns no driver
and implies no SQL engine. It defines:

- the storage-agnostic query model: the `Filter` DSL and the composable
  `Specification` (`Pred` / `And` / `Or` / `Not`);
- the repository traits, laid out as a layered hierarchy:
  the awaited `Repository<T, K>`, and the reactive tier —
  `ReactiveCrudRepository<T, ID>` (find/save/delete returning `Mono`/`Flux`),
  `ReactiveSpecificationRepository<T>` (spec + page streaming), and
  `ReactiveSortingRepository<T, ID>` (`find_all(Sort)` / `find_all(Pageable)`)
  — the last of which is a **blanket impl** over any store that is both Crud
  and Specification, so every adapter gains sorting/paging for free;
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
/ `firefly-session-postgres` / `firefly-session-mongodb` are distributed
adapters that make the per-principal concurrency cap hold cluster-wide.

### The declarative transaction layer

Transactions follow the same port/adapter split. `firefly-transactional` is
**driver-agnostic**: it owns the policy types (`Propagation`, `Isolation`,
`TxOptions`) and the object-safe `TransactionManager` port. Its single
`async fn execute` establishes the ambient transaction context (a task-local
transaction stack), applies the requested propagation/isolation, runs the
operation, and **commits on `Ok` / rolls back on `Err`**. A concrete adapter
implements that port once — the
relational adapter is `firefly-data-sqlx`'s `SqlxTransactionManager`, wired over
the same `Db` pool the repositories use — and is registered process-wide with
`register_transaction_manager(...)` at startup (typically by a data starter or
auto-configuration). The same abstraction backs relational, document, and
saga-step boundaries; "build the boundary once" is enforced by the one port.

```
                firefly_transactional::TransactionManager  (port)
                            ▲
        ┌───────────────────┴──────────────────┐
   SqlxTransactionManager              (other adapters: Mongo session, …)
   (in firefly-data-sqlx, over Db)
```

The programmatic entry point is `firefly_transactional::transactional(opts, f)`
(and its `*_with` / `*_on` variants for explicit rollback rules and explicit
managers). The `#[transactional]` attribute macro is the declarative front: it
wraps an `async fn`'s body in that call under
the attribute's `propagation`/`isolation`/`read_only`/`timeout`, so a method
body of `repo.save(a).await?; repo.save(b).await?;` is atomic with no explicit
boundary code. If no manager is registered (e.g. a unit test with no
datasource), `transactional` degrades gracefully to a plain call, so the same
code runs in and out of an infrastructure context. Propagation/isolation are
*wired at the adapter*: the manager translates the policy to the backend's
`BEGIN` / `SAVEPOINT` / `SET TRANSACTION ISOLATION LEVEL` semantics, keeping the
call sites portable across databases.

## Operations: the admin dashboard

`firefly-admin` is an embedded operations dashboard. Architecturally it
is a thin **read-mostly aggregation layer** over the management
primitives already in the framework, plus a static single-page app:

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
| `firefly-starter-experience` | `ExperienceStack` — `WebStack` plus the Backend-for-Frontend (BFF) building blocks: a `DomainClients` registry of named `RestClient`s, `@WaitForSignal`-style `SignalService` gates, Redis-capable `WorkflowState`, the `WorkflowQueryService` journey-status surface, and `ChildWorkflowService` |
| `firefly-backoffice`         | starter-application + back-office context middleware               |

Beyond the crate-graph tiers above, services themselves are layered into
**three service tiers** with a strict `channel → experience → domain → core`
dependency direction: a **core** service owns the database; a **domain**
service owns sagas, CQRS, event sourcing, and third-party adapters over core
SDKs; and an **experience (BFF)** service composes several domain SDKs into
journey-specific, atomic REST endpoints, owning no database of its own.
`firefly-starter-experience` is the starter for that experience tier;
`firefly-starter-core` / `-data` and `firefly-starter-domain` back the others.

Each starter ships an embedded banner printed at startup (via
`Core::print_banner`) naming the active starter, the application name
and the resolved Rust runtime.

## The front door: facade + declarative macros

On top of the tiers above sits the **ergonomics layer** — the
one-dependency developer experience. It is two crates that add no
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
| `#[transactional]` | an `async fn -> Result<T, E>` (`E: From<TxError>`) | a body wrapped in `firefly_transactional::transactional` under the requested `propagation`/`isolation`/`read_only`/`timeout` |
| `#[derive(Builder)]` | a struct | a fluent `T::builder()…build()` |
| `#[derive(Mapper)]` | a struct with `#[firefly(from = "Source")]` | compile-time `From<Source>` conversions |
| `#[repository]` | an `impl` block of `find_by_…`/`count_by_…`/`exists_by_…`/`delete_by_…` stubs | derived-query method bodies over `firefly-data`'s query engine |

Because generated code addresses runtime types through `::firefly::__rt::…`
(overridable per macro with `#[firefly(crate = "…")]`), a service that depends
only on `firefly` — plus the `axum`/`serde` it writes against anyway — compiles
whatever a macro expands to. There is no package scanning or reflective
autowiring, so DI registration is explicit (`register_all!` lists the
components) and free-fn handlers publish their wiring state explicitly; the
macros remove the *mechanical* boilerplate, not the explicitness. The
`samples/macro-quickstart` service is the reference: the orders behaviour in
376 source lines vs the builder-style `orders` sample's 1022.

## The developer CLI (`firefly-cli`)

`firefly-cli` ships the `firefly` binary — the project scaffolding and
introspection tool. It is build-time/dev tooling, not a runtime tier crate, so
it depends on `firefly-openapi` and `firefly-migrations` for the artifacts it
emits but is never linked into a service. Its commands:

- `firefly new <name>` scaffolds a project that compiles out of the box;
  `firefly generate <kind> <name>` (alias `g`) emits code artifacts into the
  current project (templates rendered with `minijinja`);
- `firefly run` launches the app via Cargo, mapping `--profile` / `-D` /
  `--env` / `--debug` onto `FIREFLY_*` environment variables;
- `firefly build info|image` stamps `build-info.json` for `/actuator/info` or
  builds an OCI image;
- `firefly db init|migrate|upgrade|downgrade|status` drives SQLite migrations
  through `firefly-migrations`; `firefly openapi` exports an OpenAPI document;
- `firefly info` / `firefly doctor` report framework, environment, and project
  health, with shell-completion generation alongside.

## Context propagation

Rust has no ambient request context, so Firefly uses two explicit
mechanisms to thread correlation ids, tenants and transactions through a
call graph:

- **Task-local scopes** for ambient request metadata:
  `firefly_kernel::with_correlation_id(id, fut)` scopes a correlation
  id over a future; `correlation_id()` reads it anywhere downstream.
  Nested scopes shadow like child contexts. HTTP propagation stays
  header-based (`X-Correlation-Id`), applied by `CorrelationLayer`.
- **Explicit handle types** where the value is load-bearing:
  `firefly_transactional::TxContext` carries the active transaction,
  `firefly_orchestration::CancellationToken` carries cooperative
  cancellation through the orchestration engines.

## Dependency injection (`firefly-container`)

The framework's default composition idiom is **explicit construction** —
`Arc<dyn Trait>` handles threaded through constructors, the same shape
the rest of this document describes. `firefly-container` is an
**opt-in** service-locator surface for teams that prefer that style,
never a requirement: nothing in the core or the starters depends on it.

It is a `TypeId`-keyed registry behind `RwLock`s (so a `Container` is
`Send + Sync` and shares as `Arc<Container>`). It provides
`register_factory::<T>(scope, f)`, `resolve::<T>()`,
`resolve_all::<T>()`, named beans, `Provider<T>` deferred handles,
primary/order, and `Scope` (Singleton / Prototype / Request / Session /
custom via the `ScopeHandler` SPI). Everything is explicit and
type-safe:

- **Autowiring is explicit.** A `register_factory` closure resolves its
  own dependencies by calling `resolve` — there is no constructor
  type-hint introspection.
- **Registration is explicit.** There is no package scanning or
  auto-discovery; components register themselves directly.
- **Trait-object bindings** work because `TypeId::of::<dyn Trait>()` is a
  valid key: `bind::<dyn Trait, Impl>(coerce)` registers an impl under
  the trait's id with a caster, so `resolve::<dyn Trait>()` and
  `resolve_all::<dyn Trait>()` return the bound implementation.
- **Circular dependencies** are caught by a thread-local resolution
  stack.

This is deliberately the *explicit* end of the DI spectrum: no runtime
magic, every wiring visible in source.

## Aspect-oriented programming (`firefly-aop`)

`firefly-aop` provides aspect-oriented advice
(`before` / `around` / `after_returning` / `after_throwing` / `after`)
over a `Pointcut` glob matcher on dot-segmented qualified names
(`service.OrderService.create`). An `AspectRegistry` holds ordered
`AdviceBinding`s; `intercept(&registry, type, method, args, invocation)`
runs the advice chain around the captured original call.

The key design decision is **explicit weaving at the call site**. The
call site wraps the original call in an `Invocation` and routes it
through `intercept`, rather than rewriting live methods at runtime.
Non-matching methods cost nothing: if no binding matches the qualified
name, `intercept` runs the invocation with zero advice overhead. Args
and results are type-erased to `Arc<dyn Any + Send + Sync>` and advice
downcasts when it needs the concrete type. For HTTP-edge and
bus-dispatch cross-cutting
concerns, the framework still prefers `firefly-web`'s tower layers and
`firefly-cqrs`'s `Middleware`; `firefly-aop` targets pattern-matched
advice over arbitrary service methods.

## Error model

`firefly-kernel` defines a `thiserror`-derived `FireflyError` with code,
title, HTTP status, detail, structured fields, and an optional source
chain traversable with `std::error::Error::source`. Each crate layers
its own `thiserror` enum on top (`CqrsError`, `EdaError`,
`CallbackError`, …) with `Display` strings kept stable across releases
where wire or log consumers depend on them.
`firefly_kernel::as_problem` renders any `std::error::Error` as an RFC
7807 `ProblemDetail`; `firefly-web`'s `WebResult<T>` lets handlers `?`
their way to a correct `application/problem+json` response.

## The reactive core and plain async

Firefly ships a **first-class reactive core** — `firefly-reactive`'s
`Mono<T>` / `Flux<T>` — *and* it interoperates with plain async Rust, so
authors pick the level that fits. The reactive types will feel familiar
if you have used a reactive-streams library: lazy, composable publishers
with a full operator surface and backpressure-aware streaming.

> **Design note.** The reactive core is not a requirement. Most internal
> code can be written in ordinary `async`/`await`, and the reactive types
> convert to and from raw `Stream` / `Future` at the edges, so the two
> styles compose freely within a single service.

The **reactive surface** (`firefly-reactive`'s first-class types):

| Reactive operation           | firefly-reactive                                              |
|------------------------------|---------------------------------------------------------------|
| single deferred value        | `Mono<T>` (lazy, `FireflyError`-typed)                        |
| stream of values             | `Flux<T>` (lazy, terminal error)                             |
| empty / completion           | `Ok(None)` from a `Mono`                                      |
| error signal                 | `Mono::error(FireflyError::...)` / a terminal `Err`           |
| block for a value            | `Mono::block` — `async`, never parks a Tokio worker          |
| schedulers                   | `Scheduler::{Immediate,Parallel,BoundedElastic}`             |
| retry with backoff           | `Backoff` + `*::retry_backoff`                               |
| timeout                      | `Mono::timeout` / `Flux::timeout` (→ 504 `FireflyError`)      |
| bounded backpressure buffer  | `Flux::on_backpressure_buffer` (bounded channels underneath)  |
| programmatic emission        | `FluxSink` / `Flux::create`                                   |
| convert to future / stream   | `Mono::into_future` / `Flux::into_stream` (escape hatches)   |

When a service prefers ordinary `async`/`await` over the reactive types,
the **plain-async equivalents** apply (most internal code):

| Concept                      | Plain-async Rust idiom                                         |
|------------------------------|----------------------------------------------------------------|
| single deferred value        | `async fn(..) -> FireflyResult<T>`                             |
| stream of values             | `impl Stream<Item = T>` (`futures` / `tokio-stream`)           |
| ambient context read         | Task-local read (`correlation_id()`) or explicit handle        |
| subscribers                  | Spawned tasks (`tokio::spawn`)                                 |
| cancellation                 | Future drop + `CancellationToken` for cooperative engines      |

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
  container, aop, shell, macros                 (stand-alone)
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
  session-redis, session-postgres, session-mongodb (→ session)
  cli                            (→ openapi/templates)
  starter-core                   (→ wave-2 set)
        │
Wave 4 ── composition + front door:
  starter-application, starter-domain, starter-data, starter-web,
  starter-experience             (→ starter-web + client + orchestration),
  backoffice,
  admin                          (→ actuator + cqrs + orchestration + sse + security),
  firefly                        (facade → every runtime crate + macros + feature-gated adapters),
  tests/integration, samples/lumen, samples/orders,
  samples/reactive-banking,
  samples/macro-quickstart       (→ firefly)
```

## Versioning

Calendar-versioned, expressed as valid semver (`YY.M.PATCH`). The
current version is exposed as `firefly_kernel::VERSION = "26.6.4"` and
set once in the workspace `Cargo.toml`.
