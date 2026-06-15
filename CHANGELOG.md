# Changelog

All notable changes to the Firefly Framework for Rust.

## v26.6.6 — 2026-06-15

The **turnkey-bootstrap & auto-generated-API-docs milestone**. A service now
boots from a single line — `firefly::FireflyApplication::new("app").run().await`
— and the framework discovers, wires, and serves everything Spring Boot's
`SpringApplication.run` would: component scan, controller auto-mount, handler /
listener / scheduled draining, security + middleware, the self-hosted admin
dashboard, and now a fully **auto-generated OpenAPI surface** and a transparent
**global exception-advice** layer. No composition root, no `build_app`, no
manual route registration.

### Added

- **`FireflyApplication` — the turnkey bootstrap** (Spring's
  `SpringApplication.run`). `new(name).version(v).run().await` builds the web
  stack, auto-registers the infrastructure beans, component-scans the app's
  beans, drains the inventory-registered CQRS handlers / EDA listeners /
  `#[scheduled]` tasks, auto-mounts every `#[rest_controller]`, auto-discovers
  the security `FilterChain` + `BearerLayer` beans, installs the correlation /
  W3C-trace / read-cache middleware, self-hosts the admin dashboard on the
  management port, prints a pyfly/Spring-style line-by-line startup report, and
  serves the public + management ports with graceful shutdown.
  `bootstrap()` returns the assembled (un-served) app for in-process tests.
- **Auto-generated OpenAPI 3.1 + Swagger UI + ReDoc**, wired automatically into
  every app (the springdoc-openapi model — no application code). The spec is
  built from the live inventory (`#[rest_controller]` routes +
  `#[derive(Schema)]` DTOs) and served at `/v3/api-docs` (+ `/openapi.json`
  alias), with Swagger UI at `/swagger-ui` (+ `/swagger-ui.html`) and ReDoc at
  `/redoc`.
- **`#[derive(Schema)]`** — registers a DTO's OpenAPI component schema
  (springdoc's `@Schema`), computed at compile time (no runtime reflection) by
  walking the struct's fields, honouring serde `rename` / `rename_all` / `skip`,
  and `$ref`-ing nested `#[derive(Schema)]` types. Every registered schema lands
  in the document's `components.schemas`.
- **Request / response model inference** — the `#[rest_controller]` macro infers
  each operation's request and response schema from the handler signature (the
  `Json<T>` parameter and the `Json<T>` in the `WebResult<…>` / tuple return
  type); a `$ref` is emitted only when the type is a registered `Schema`, so an
  unannotated body (e.g. `serde_json::Value`) never dangles.
- **Per-operation OpenAPI metadata on the verb macros** —
  `#[get("/x", summary = "…", description = "…", tags = ["…"], status = 200,
  deprecated, request = T, response = T)]` and a `#[rest_controller(tag = "…")]`
  group tag. `request` / `response` are optional overrides of the inference.
- **Global exception-advice layer** (Spring's `@ControllerAdvice`) — register an
  `ExceptionHandlerRegistry` bean and `FireflyApplication` installs an
  `ExceptionAdviceLayer` at the outermost edge that re-parses every
  `application/problem+json` response and re-renders it through the registry
  (custom status / title / body), preserving existing response headers.
- **Default RFC 9457 `404`** — an unmatched route now returns a proper
  `application/problem+json` not-found document (rendered identically to every
  other framework error) instead of axum's bare empty body.

### Changed

- The Lumen sample is now a single-binary crate with a **one-line `main`**; its
  HTTP surface (`web.rs`) is purely declarative — `#[derive(Configuration)]` +
  `#[bean]` factories, a `#[derive(Controller)]` + `#[autowired]` controller,
  `FilterChain` / `BearerLayer` beans, a feature-gated `RouteContributor` bean,
  and `#[derive(Schema)]` DTOs annotated with per-operation OpenAPI metadata.
- Bind addresses are overridden with `FIREFLY_SERVER_ADDR` /
  `FIREFLY_MANAGEMENT_ADDR` (honoured by `FireflyApplication`).

## v26.6.5 — 2026-06-15

The **declarative-services milestone**. A complete declarative layer lands on top
of the standalone framework: annotation-style orchestration, in-process events
with a transactional/broker bridge, aspect-oriented advice, caching, validation,
and async methods — each a thin macro over a real, tested engine. The book and
all reference docs are brought current.

### Added

- **Declarative orchestration** — `#[saga]` + `#[saga_step]` (DAG `depends_on`,
  compensation, retry/backoff/timeout, argument injection via
  `#[input]`/`#[from_step]`/`#[variable]`/`#[ctx]`), `#[workflow]` +
  `#[workflow_step]` (parallel DAG), and `#[tcc]` + `#[participant]`
  (try/confirm/cancel). The `Saga` engine gained layered topological execution
  (`Step::depends_on`); the Lumen sample now drives its transfer (saga),
  compliance (workflow), and two-phase transfer (TCC) declaratively.
- **In-process application events** — `#[application_event_listener]`
  (Spring `@EventListener`) and `#[transactional_event_listener]`
  (`@TransactionalEventListener`, phases `before_commit` / `after_commit` /
  `after_rollback` / `after_completion`), `publish_event`, an `inventory`-based
  listener registry, and `LocalTransactionManager` (Spring's
  `ResourcelessTransactionManager`) for transactional event semantics without a
  datasource.
- **EDA bridge** — `register_broker` / `broker()`, `publish_to_broker`, and
  `externalize_after_commit::<E>(topic, type)` (Spring Modulith event
  externalization): an in-process event published inside a committed transaction
  is forwarded to the message broker; a rolled-back one publishes nothing.
- **Declarative AOP** — `#[aspect(pointcut, order)]` with `#[before]` /
  `#[after]` / `#[after_returning]` / `#[after_throwing]` / `#[around]` advice
  markers (over the existing `firefly-aop` engine), an `inventory`-discovered
  process-global `AspectRegistry`, and the explicit `advised(...)` weave point.
- **Declarative caching** — `#[cacheable]` / `#[cache_put]` / `#[cache_evict]`
  over `async fn -> Result<V, E>`, around a process-registered cache adapter.
- **JSR-380 bean validation** — `#[derive(Validate)]`
  (`email`/`url`/`not_empty`/`length`/`range`/`pattern`/`custom`, with the
  `pattern` regex compile-checked at macro-expansion) and the `Valid<T>` axum
  extractor (422 on a constraint failure, 400 on malformed JSON).
- **Async methods** — `#[async_method]` rewrites an
  `async fn(self: Arc<Self>, …) -> R` into a non-async `fn -> TaskHandle<R>`
  spawned on a registered `TaskExecutor`.

### Changed

- The book gains an in-process-events + after-commit-externalization section
  (EDA chapter) and declarative catalogue entries for the new macros; ARCHITECTURE,
  the README, and the `transactional` / `eda` / `aop` crate READMEs document the
  new surfaces.
- Content-freshness pass: 69 confirmed documentation corrections across the book,
  top-level docs, and crate READMEs (stale counts, versions, and out-of-date code
  snippets brought in line with the code).

### Fixed

- `#[firefly(lazy)]` beans are no longer eagerly constructed during singleton
  warm-up.
- Declarative orchestration now propagates a step result-encoding failure instead
  of silently substituting null.
- Lumen's compliance endpoint answers 404 for an unknown source wallet (was 422).

## v26.6.4 — 2026-06-14

The **standalone-framework milestone**. New first-class capabilities —
config-driven auto-configuration, method security, richer declarative data
queries, and a configurable JSON mapper — land alongside a full documentation
pass that presents Firefly as the brand-new framework it is.

### Added

- **Method security** — `#[pre_authorize(...)]` (rules: `authenticated`,
  `role`, `any_role`, `authority`, `any_authority`) and
  `#[post_authorize(<expr over result/auth>)]`, backed by an ambient
  `SecurityContextHolder` (`with_authentication_scope`, `current_authentication`,
  `check_access`, `AccessRule`) that `BearerLayer` scopes automatically per
  request — so the macros work on a service method that never sees the request.
- **`@query` + `Pageable` on `#[repository]`** — `#[query("…")]` native SQL and
  `#[query(jpql = "…", entity = "…")]` custom queries (list / count / exists /
  modifying), plus a trailing `Pageable` argument for paged derived queries
  (runtime `SqlxReactiveRepository::find_by_derived_paged`).
- **`ObjectMapper`** (`firefly-web`) — a runtime JSON facade with a
  `PropertyNaming` strategy, an `Inclusion` policy, and pretty-printing, plus
  `MappingJsonConverter` to install the policy into content negotiation.
- **Config-driven auto-configuration** (DI-free, awaited at startup):
  `DataSourceProperties` + `Db::connect` / `Db::connect_with` /
  `auto_configure` (builds the pool and registers a `SqlxTransactionManager`),
  and `SecurityProperties` + `verifier_from_config` / `bearer_layer_from_config`.
- **`firefly-session-mongodb`** — a MongoDB-backed `SessionRegistry`
  (`MongoSessionRegistry`), joining the in-memory, cache-bridge, Postgres, and
  Redis session backends.
- **Application-config logging** — `log_config_from_properties` binds
  `firefly.logging.*` (root + per-logger levels, format, service, and the
  rolling file appender) straight from the main config, completing the
  configure-logging-from-application.yaml story alongside runtime
  `/actuator/loggers` control.

### Changed

- **Documentation presents Firefly as a standalone, brand-new framework.** The
  book (26 chapters plus the preface and conventions), the `docs/` set, and 74
  crate / sample / root READMEs are written in Firefly's own voice; the recurring
  "Spring parity" / "Reactor parity" callouts are now a single **Design note**.
- The default broker topology and the data-layer query metrics now live in the
  Firefly namespace — RabbitMQ defaults `firefly` / `["firefly.events"]` /
  `firefly-default`, and metrics `firefly_db_query_duration_seconds` /
  `firefly_db_queries_total` / `firefly_db_query_errors_total`.
- **Observability is auto-instrumented by default.** `Core` now installs the
  Micrometer-style HTTP server-metrics middleware (`http_server_requests_seconds`
  timer + `…_max` gauge) out of the box; opt out with
  `CoreConfig::disable_request_metrics`. The actuator already ships the
  Kubernetes liveness/readiness probes (`/actuator/health/{liveness,readiness}`),
  a Prometheus scrape target (`/actuator/prometheus`), and configurable endpoint
  exposure.

### Fixed

- **Repository reads can no longer deadlock a small connection pool.** Every
  `firefly-data-sqlx` read (derived, `@query`, and projection paths) now
  **buffers-and-releases** its pooled connection via the transaction-aware
  `*_fetch_all` helpers instead of holding it across the result stream — so a
  read never pins a connection across an `await` (the failure mode that wedged a
  one-connection SQLite pool under load).
- **Adapter connection hardening:** `cache-redis` stores a cloneable
  `MultiplexedConnection` directly (no per-call mutex serialising every command,
  and the `SCAN` loop no longer holds a lock); `eda-redis` / `session-redis`
  publish/register without holding the connection across awaits; `eda-postgres`
  / `eda-rabbitmq` claim start atomically (no auto-start connection leak) and the
  Postgres `LISTEN` channel now reconnects; `eda-kafka` moves the blocking
  `flush()` off the async executor.

### Removed

- The "Migrating from Spring Boot" appendix and the standalone migration guide.

## v26.6.3 — 2026-06-13

The **ergonomics + pluggable-persistence milestone**. Two headline wins: a
Spring-Boot-for-Rust developer experience (one `firefly` dependency, a prelude
glob, and declarative `#[derive(...)]` / `#[...]` macros) and a truly hexagonal
data layer (one set of `firefly-data` ports, real adapters for Postgres / MySQL
/ SQLite / MongoDB). Everything here is additive; the Go-parity wire contract is
unchanged. The workspace grows from 69 to **76 members** (66 → **72** framework
crates).

### Added

**Hexagonal database adapters (a new DB = a new adapter)**

- `firefly-data` — a `SqlDialect` abstraction (`PostgresDialect` /
  `MySqlDialect` / `SqliteDialect`) so the `Filter` DSL and `Specification`
  render the *same* query tree for any relational backend
  (`Filter::to_sql_with` / `Specification::to_sql_with`, with placeholder style
  `$n` vs `?`, identifier quoting, `IN`-list shape, and case-insensitive `LIKE`
  all dialect-correct). `Filter::to_sql` / `Specification::to_sql` stay the
  PostgreSQL default for back-compat. Also `Specification::to_mongo()` /
  `Filter::to_mongo()` lower the same tree to a MongoDB `$`-operator filter
  document, and the `Auditor` gains a `UserProvider` hook.
- `firefly-data-sqlx` — the **relational** repository adapter implementing the
  `firefly-data` ports over `sqlx` for **Postgres, MySQL, and SQLite** from one
  codebase: `SqlxRepository` (blocking-value) and `SqlxReactiveRepository`
  (streaming reads as a `Flux<T>`) pick the right `SqlDialect` at runtime from
  the `Db` pool's `Backend`, build dialect-aware `UPSERT`s
  (`ON CONFLICT … DO UPDATE` for Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for
  MySQL), and auto-apply auditing + soft-delete. Backend-agnostic row decoding
  via `SqlxRowMapper`/`AnyRow`; writes via `ColumnValue`/`RowWriter`.
- `firefly-data-mongodb` — the **document** repository adapter over the official
  `mongodb` crate: `MongoRepository<T, ID>` implements the *same*
  `ReactiveCrudRepository` + `ReactiveSpecificationRepository` ports as the
  relational adapters, lowering `Specification::to_mongo()`, with a
  `BaseDocument` audit/soft-delete mixin and an `Audited` hook, and cursor-based
  streaming reads. A service swaps Postgres for Mongo without touching its call
  sites. All four backends are tested against **real**
  Postgres/MySQL/SQLite/MongoDB.

**Ergonomic declarative layer (one dependency, macros instead of builders)**

- `firefly-macros` — a `proc-macro` crate of derive/attribute macros (the Rust
  answer to Spring annotations / pyfly decorators): `#[derive(Command)]` /
  `#[derive(Query)]` (→ `impl firefly_cqrs::Message`, with `#[firefly(validate)]`
  / `#[firefly(cache_ttl = "…")]`); `#[command_handler]` / `#[query_handler]`
  (→ a `register_<fn>(bus)` helper); `#[derive(Component)]` /
  `#[derive(Service)]` / `#[derive(Repository)]` + the `register_all!` macro
  (→ DI-container registration); `#[scheduled]` (→ `schedule_<fn>(scheduler)`);
  `#[rest_controller]` + `#[get/post/put/delete/patch]` (→ a
  `routes(state) -> axum::Router`); `#[derive(DomainEvent)]` /
  `#[derive(AggregateRoot)]`; and `#[event_listener]`
  (→ a `subscribe_<fn>(broker)` helper).
- `firefly` — the **one-dependency facade**: `use firefly::prelude::*;` pulls in
  the whole framework (`Bus`, `Container`, `Scheduler`, `Saga`/`Step`,
  `Application`, `Core`/`CoreConfig`, `WebResult`/`WebError`/`problem_response`,
  `FireflyError`/`FireflyResult`, `Mono`/`Flux`) plus every macro. Ships
  ergonomic per-crate aliases (`firefly::cqrs`, `firefly::web`, …) and a hidden,
  stable `__rt` contract path that macro-generated code targets — so a service
  depends only on `firefly`. Heavy adapters (`data-sqlx`, `data-mongodb`,
  `eda-*`, `cache-*`, `admin`, `full`) are opt-in cargo features; a default
  build pulls in none of them.
- `samples/macro-quickstart` — `firefly-sample-macro-quickstart`, the same
  orders behaviour as the `orders` sample re-expressed declaratively over the
  single `firefly` facade: 376 source lines vs 1022 (−63%), two modules vs
  seven, with no hand-written `impl Message`, `bus.register(…)`,
  `Router::new().route(…)`, or scheduler builder.

**Distributed session registries**

- `firefly-session-redis` — `RedisSessionRegistry`, a distributed
  `firefly_session::SessionRegistry` backed by a Redis sorted set (score =
  `created_at`, oldest-first via `ZRANGE`; sliding `EXPIRE`), so the
  per-principal session-concurrency cap holds cluster-wide rather than only
  within one process.
- `firefly-session-postgres` — `PostgresSessionRegistry`, a durable, distributed
  `SessionRegistry` over a Postgres table (idempotent `ON CONFLICT` upsert,
  `ORDER BY created_at ASC` oldest-first) for relational-only deployments.

**Testkit + CLI**

- `firefly-testkit` — a `TestClient` / `TestResponse` in-process axum-router
  driver (fluent `assert_status` / `assert_json_eq` / `assert_header` / …),
  `assert_event_published` / `assert_event_published_with` over the `SpyBroker`,
  and DI test `Slice` / `BuiltSlice` helpers (the pyfly `slice_context` /
  `mock_bean` analog, with eager fail-fast resolution).
- `firefly-cli` — `completion` (shell-completion scripts), `sbom` (dependency
  SBOM), and `license` (dependency-license report) commands.

**Documentation**

- The book now renders to offline editions:
  `docs/book/dist/firefly-rust-by-example.pdf` and `.epub` (pandoc + tectonic),
  via `make book-pdf` / `make book-epub`. A new
  "Declarative Services with Macros" chapter covers the facade + macros, and the
  persistence chapter is extended with the MySQL / SQLite / MongoDB adapters.

### Fixed

- **Adversarial-review fixes** (macros + data adapters):
  - `firefly-data` — `Op::Like` / `Op::ILike` now lower to an **anchored**
    MongoDB `$regex` (`^…$`, translating SQL `%`/`_`, regex-escaping the rest),
    so the same `Specification` matches identical rows on Mongo, SQL, and
    in-memory (an unanchored Mongo `$regex` would have made `name LIKE 'A%'`
    silently match `"bAr"`).
  - `firefly-data-sqlx` — `save` resurrects soft-deleted rows (clears
    `deleted_at` on upsert); timestamp coercion is tag-driven, so
    RFC3339-looking text is no longer mis-typed as a timestamp.
  - `firefly-macros` — `#[derive(DomainEvent)]` JSON-encodes through the facade's
    `__rt::serde_json` (preserving the one-dependency contract);
    `#[event_listener]` preserves the consumer `group` when given a positional
    topic; `#[scheduled]` rejects `cron` + `initial_delay` with a compile error.
- **`serde_json` ordering wire-parity** — linking the `mongodb`/`bson` crate
  turned on `serde_json/preserve_order` workspace-wide (Cargo feature
  unification), flipping `serde_json::Map` from sorted-key to insertion-order;
  restored deterministic sorted-key wire output where it is contractually
  required (`config-server`, `openapi`, `callbacks`).
- Stabilized flaky admin SSE timing tests (raised the under-load timeout).

## v26.6.2 — 2026-06-13

The **reactive milestone**. This release adds a WebFlux-style reactive
core and threads it through the framework, makes every vendor adapter
real (no stubs remain), introduces real-infrastructure Docker testing
and an mdBook documentation site, and ships the `firefly` developer CLI
and an end-to-end reactive sample. The Go-parity wire contract is
unchanged; everything here is additive.

### Added

**Reactive core (the keystone)**

- `firefly-reactive` — a faithful Project Reactor / WebFlux analog:
  `Mono<T>` (0-or-1 + error) and `Flux<T>` (0..N + terminal error) over
  `tokio` futures/streams, fixed to `firefly_kernel::FireflyError`. Ships
  a `Scheduler` (`Immediate` / `Parallel` / `BoundedElastic`), a
  `FluxSink` for imperative emission (`Flux::create`), a `Backoff` retry
  policy, and the full operator surface — transform (`map` / `flat_map` /
  `concat_map` / `scan`), combine (`merge` / `concat` / `zip` /
  `combine_latest`), reduce/terminal (`reduce` / `collect_list` /
  `collect_map`), error (`on_error_resume` / `on_error_continue` /
  `retry` / `retry_backoff`), time (`timeout` / `debounce` / `sample` /
  `interval`), backpressure (`on_backpressure_{buffer,drop,latest}` /
  `limit_rate`), and windowing (`buffer` / `window` / `group_by`).

**Reactive integration across the framework**

- `firefly-web` — reactive HTTP responders: `MonoJson<T>` (renders a
  `Mono` as JSON, `Ok(None)` → 404 problem+json, `Err` → RFC 7807),
  `NdJson<T>` and `Sse<T>` (stream a `Flux` as `application/x-ndjson` /
  `text/event-stream` with **true backpressure** — never buffered),
  and `SseEvents` (pre-built `firefly_sse::Event` frames).
- `firefly-data` — the reactive `ReactiveCrudRepository<T, ID>` (with
  `find_all` / `find_by_id` / `save` / `delete_by_id` / `count` returning
  `Mono`/`Flux`), an in-memory `ReactiveMemoryRepository`, a
  `ReactiveSpecificationRepository`, and a real `PostgresReactiveRepository`
  that streams rows out of `find_all()` as a `Flux<T>` over
  `tokio-postgres` (with `RowMapper` / `TableConfig`).
- `firefly-client` — the reactive `WebClient` (`WebClientBuilder` →
  `get`/`post`/`put`/`delete`/`patch` → `RequestSpec` →
  `retrieve()` → `ResponseSpec::body_to_mono::<T>()` /
  `body_to_flux::<T>()` / `exchange()`), the Rust analog of WebFlux's
  `WebClient`.
- `firefly-eda` — reactive subscription: `InMemoryBroker::subscribe_reactive`
  (and `_with_buffer`) yields a `Flux<Event>` with bounded backpressure,
  and `publish_mono` is a cold reactive publish.
- `firefly-cqrs` — reactive bus: `Bus::send_mono` / `query_mono` (and the
  `_with_context` variants) wrap dispatch in a lazy `Mono<R>`, running the
  same handler lookup and validation/authorization/caching middleware;
  `cqrs_error_to_firefly` maps `CqrsError` onto the right HTTP status.

**Real vendor adapters — zero stubs**

- The SendGrid and Resend email channels are now real: `SendGridEmailProvider`
  POSTs to SendGrid v3 `/mail/send`, `ResendEmailProvider` POSTs to Resend
  `/emails`, both over `reqwest`; their Go-parity envelope `Channel`s
  delegate to the real provider. No notification, IDP, or ECM adapter
  ships a `NotImplemented` sentinel any longer.
- `firefly-cache-postgres` is a real `cache::Adapter` (`PostgresCacheAdapter`)
  backed by a Postgres key/value table with TTL over `tokio-postgres`
  (upsert, `set_if_absent`, `delete_prefix`, key scan, health check).
- `firefly-starter-web` is a real web-stack starter: `WebStack` layers
  `Core` with CORS, security headers, request metrics, and an access log
  by default, with optional `FilterChain` security.

**Real-infrastructure testing**

- A `docker-compose.yml` stack (Postgres, Redis, RabbitMQ, Redpanda,
  Keycloak, LocalStack S3, Azurite Blob, MailHog SMTP) plus
  `make infra-up` / `make test-integration` / `make infra-down`. The
  env-gated integration tests run the cache, EDA, IDP, ECM, notification,
  and reactive-Postgres adapters — and the reactive-banking sample —
  against the **real** services, while `cargo test --workspace` stays
  green offline (each test skips when its connection env var is unset).

**Documentation, tooling, and samples**

- `docs/book` — an mdBook guide (builds with mdBook) covering why-Firefly,
  quickstart, configuration, dependency wiring, the keystone reactive
  model, HTTP APIs, persistence, DDD, CQRS, EDA, event sourcing, sagas,
  HTTP clients, security, observability, scheduling/notifications,
  caching, testing, the CLI, production, and appendices (Spring mapping,
  module index, glossary).
- `firefly-cli` — the `firefly` developer binary (`new`, `generate`/`g`,
  `info`, `doctor`, `db`, `openapi`, and remote actuator introspection),
  installable via `make cli-install` / `cargo install --path crates/cli`.
- `samples/reactive-banking` — `firefly-sample-reactive-banking`, an
  end-to-end reactive service: reactive CQRS, event sourcing, a
  saga-backed money transfer, a `Flux<AccountEvent>` NDJSON/SSE stream,
  JWT-secured `starter-web`, and a `WebClient` SDK, running on in-memory
  defaults or real Postgres/Kafka.

### Changed

- Every source file now carries the Apache 2.0 license header (Firefly
  Software Foundation, 2026).
- Documentation refreshed end to end (README, `MODULES.md`, the `docs/`
  guides, and the book): the reactive core and integrations are now
  prominent, all vendor adapters are documented as real/Full, the
  real-infra testing path is described, and the workspace count is
  current (66 framework crates; 69 workspace members).

### Fixed

- Adversarial-review fixes across the reactive surfaces and adapters
  (error mapping, backpressure/termination semantics, and connection
  handling), and corrected documentation that previously described
  SendGrid/Resend, `cache-postgres`, and `starter-web` as port-pending
  stubs.

## v26.6.1 — 2026-06-12

**First public release** of the Rust port at
<https://github.com/fireflyframework/fireflyframework-rust>.

Fourth sibling port of the Java/Spring Boot Firefly Framework, joining
the .NET, Go, and Python (PyFly) ports. Ported with full module parity
against the Go port (the canonical compiled-language reference) **plus a
purely additive PyFly-parity layer**: one Cargo workspace with 67
members — 65 `firefly-*` crates under `crates/`, the cross-crate
integration suite, and the Orders reference sample. Targets Rust 1.85+
(edition 2021) on the tokio + axum + serde stack, with `thiserror`
errors, `async-trait` ports, RustCrypto primitives, and `tracing`
structured logging. Wire-compatible with the sibling ports: RFC 7807
`application/problem+json`, `X-Correlation-Id` propagation,
`Idempotency-Key` semantics, event envelope JSON, HMAC webhook
signatures, Spring-Cloud-Config response shape, and `V###__name.sql`
migration naming.

The Go-parity core (foundational, platform, starter tiers) is kept
byte-stable on the wire; everything in the **PyFly-parity layer** below
layers onto the existing crates without changing any established wire
format.

### Added

**Foundational tier (6 crates)**

- `firefly-kernel` — RFC 7807 `ProblemDetail`, `FireflyResult<T>`,
  `Clock`, `FireflyError` hierarchy, task-local correlation scopes
- `firefly-utils` — try/retry helpers with backoff, slug, AES-256-GCM,
  templates
- `firefly-validators` — IBAN, BIC, Luhn, currency, phone, password,
  sort code, VAT, Spanish IDs
- `firefly-web` — problem renderer, correlation, idempotency, PII
  masking as composable `tower` layers
- `firefly-config` — typed YAML / env / flag binding with profile
  selection
- `firefly-i18n` — locale-aware message bundles + Accept-Language
  resolver

**Platform tier (19 crates)**

- `firefly-cache`, `firefly-observability`, `firefly-data`,
  `firefly-cqrs`, `firefly-eda` (in-memory broker full; Kafka/RabbitMQ
  scaffolds return typed sentinels), `firefly-eventsourcing`,
  `firefly-orchestration` (Saga / Workflow DAG / TCC),
  `firefly-rule-engine`, `firefly-plugins`, `firefly-lifecycle`,
  `firefly-actuator`
  (`/actuator/{health,info,metrics,env,tasks,version}`),
  `firefly-scheduling`, `firefly-resilience`, `firefly-security`,
  `firefly-migrations`, `firefly-openapi`, `firefly-sse`,
  `firefly-transactional`, `firefly-testkit`

**Adapter tier**

- Full: `firefly-client` (REST builder; SOAP/gRPC/WS scaffolds),
  `firefly-config-server`, `firefly-idp` + `firefly-idp-internal-db`,
  `firefly-ecm` (port + LocalStore), `firefly-notifications`
  (dispatcher + memory channel), `firefly-callbacks`,
  `firefly-webhooks`
- Real vendor adapters (PyFly-parity): `firefly-idp-keycloak`
  (OIDC + admin REST), `firefly-idp-azure-ad` (Microsoft Graph + ROPC),
  `firefly-idp-aws-cognito` (JSON API + self-contained SigV4),
  `firefly-ecm-storage-aws` (S3), `firefly-ecm-storage-azure`
  (Blob Storage), `firefly-ecm-esignature-docusign` (REST v2.1),
  `firefly-ecm-esignature-adobe-sign` (REST v6),
  `firefly-ecm-esignature-logalty` (eIDAS REST),
  `firefly-notifications-twilio` (SMS), `firefly-notifications-firebase`
  (FCM push) — each keeps a Go-parity/back-compat stub alongside the
  real provider
- Stub (port-asserting, typed not-implemented errors):
  `firefly-notifications-sendgrid`, `firefly-notifications-resend`

**Starter tier (5 crates)**

- `firefly-starter-core` (one-call `Core::new(CoreConfig)` wiring),
  `firefly-starter-application`, `firefly-starter-domain`,
  `firefly-starter-data`, `firefly-backoffice`

**PyFly-parity layer**

New cross-cutting crates (opt-in; the Go-parity core does not depend on
them):

- `firefly-container` — opt-in `TypeId`-keyed DI container (service
  locator): `register_factory` / `resolve` / `resolve_all` /
  `bind::<dyn Trait>` / `Scope` / `Provider<T>` / `RefreshScope`;
  explicit factory closures (no reflective autowiring)
- `firefly-aop` — Spring-style aspect advice: `Pointcut` glob matcher,
  `JoinPoint`, `Aspect` (before / around / after-returning /
  after-throwing / after), `AspectRegistry`, `intercept` chain executor
  with explicit weaving at the call site
- `firefly-session` — server-side HTTP `Session` (typed serde
  attributes), `SessionStore` (`MemorySessionStore` / `CacheSessionStore`),
  `SessionLayer` (cookie load/save, id rotation, invalidation, HMAC
  signing), `SessionRegistry` + concurrency control
- `firefly-shell` — Spring-Shell-style CLI framework: `CommandSpec`
  builder, typed `CommandArgs`, `StdShell` parser + REPL,
  `ApplicationArguments`, `CommandLineRunner` / `ApplicationRunner` +
  `RunnerRegistry`
- `firefly-websocket` — WebSocket server over axum: `WsSession`,
  `WebSocketHandler`, `ws_route` / `serve_ws`, topic `BroadcastHub`
- `firefly-cli` — the `firefly` developer binary: `new`, `generate`/`g`,
  `info`, `doctor`, `actuator`
- `firefly-admin` — Spring-Boot-Admin-style embedded dashboard (SPA +
  JSON API over `firefly-actuator` + SSE live streams + instance
  registry / client modes; `firefly.admin.*` config)

Real infrastructure transport / cache adapters (implement the existing
platform ports; pull their backing SDK only when selected):

- `firefly-cache-redis` — `cache::Adapter` over Redis (RESP via `redis`)
- `firefly-eda-kafka` — `eda::Broker` over Apache Kafka (`rdkafka`)
- `firefly-eda-rabbitmq` — `eda::Broker` over RabbitMQ (`lapin`,
  durable direct exchange, publisher confirms)
- `firefly-eda-postgres` — `eda::Broker` as a Postgres transactional
  outbox + `LISTEN`/`NOTIFY` (`tokio-postgres`, advisory-lock drain)
- `firefly-eda-redis` — `eda::Broker` over Redis Streams consumer groups
- `firefly-notifications-smtp` — SMTP email channel over `lettre`
  (real MIME, STARTTLS, BCC-not-leaked)

Reserved as port-pending placeholders for the next wave (compile and
carry their locked dependency set; implementation lands without
disturbing the wire contract):

- `firefly-cache-postgres` — Postgres-backed `cache::Adapter` (key/value
  table with TTL over `tokio-postgres`)
- `firefly-starter-web` — web-stack starter bundling `starter-core` +
  web middleware + security + actuator wiring

Additive extensions to existing crates (every Go-parity wire format
unchanged):

- `firefly-web` — CORS, security headers, CSRF (double-submit cookie),
  request access log, HTTP server metrics, extended correlation
  (`X-Request-Id` / `X-Tenant-Id` / `traceparent`), content negotiation
  (JSON/XML), and a `server.*` bootstrap (`ServerProperties` / TLS)
- `firefly-security` — JWKS resource-server `Verifier`, `oauth2`
  (client registrations + login flow with PKCE/OIDC + authorization
  server), `RoleHierarchy`, `guards`, `CsrfLayer`, and persistent token
  stores (in-memory / Redis / Postgres)
- `firefly-observability` — labeled metrics with `timed`/`counted`,
  Prometheus text exposition, and native W3C trace-context propagation
- `firefly-actuator` — Spring-Boot management model: liveness/readiness
  probes, health groups, runtime loggers, scheduled tasks, caches,
  `/actuator/refresh`, `httpexchanges`, Micrometer metric detail,
  Prometheus, custom endpoints, and the `management.endpoints.web`
  exposure model
- `firefly-config` — `${key:default}` / `${ENV}` placeholder
  resolution, runtime reload (`ReloadableConfig` / `Refresher` →
  `/actuator/refresh`), masked property-source introspection,
  multi-profile overlays, and a Spring-Cloud-Config client
- `firefly-orchestration` — workflow step compensation
  (`Node::with_compensation`, reverse-order rollback), `wait_all` /
  `wait_any` join points (`WaitTarget`), child workflows
  (`ChildWorkflowService`), continue-as-new (`ContinueAsNew`),
  conditional + async steps, per-step retry / backoff / timeout
  (`invoke_with_policy`), inter-step data passing (`StepContext`),
  durable execution state, stuck-run recovery, a dead-letter queue,
  signal / timer workflow nodes, an `EventGateway` for broker-driven and
  scheduled saga starts, a ruleset-style `validator`, and a REST admin
  surface (`MemoryPersistence` / `SqlitePersistence` adapters)
- `firefly-eventsourcing` — global cross-aggregate `EventStore::stream_all`
  + cross-aggregate projections, multi-tenancy (tenant-scoped append /
  load / stream), and an `EventSourcedRepository`
- `firefly-rule-engine` — `between` / null / `regex` operators,
  `Rule.otherwise`, `EvaluationMode` (All / FirstMatch), a ruleset
  validator, and pluggable `ActionHandler`s
- `firefly-data` — `Mapper` / `Mapping` / `Projection` object mapper,
  a derived-query parser (`QueryMethodParser` / `ParsedQuery`), and
  `Pageable` / `Sort` / `Order` paging requests
- `firefly-validators` — `national_id` and `tax_id` validators
- `firefly-kernel` — a `ddd` module (`Entity`, `Specification`
  combinators, domain events / `PendingEvents`), task-local request and
  tenant scopes alongside correlation, and a typed `ErrorResponse`
  (`ErrorCategory` / `ErrorSeverity` / `FieldError`)
- `firefly-eda` — `Event.key` routing key, glob topic subscriptions,
  round-robin consumer groups, `EventFilter` chains
  (`HeaderEventFilter` / `PredicateEventFilter`), a queryable
  `EdaDeadLetterStore`, an `EventPublisherHealthIndicator`, and a
  `wrap_listener` retry/DLQ wrapper
- `firefly-cache` — LRU eviction + hit/miss statistics on the in-process
  `MemoryAdapter`

**Tests + samples**

- `tests/integration` — cross-crate suite (CQRS roundtrip, callbacks
  dispatch with HMAC verification by webhooks, saga compensation,
  starter-core boot)
- `samples/orders` — Orders reference service (`firefly-sample-orders`)

**Documentation + tooling**

- Per-crate `README.md` (overview, public surface, quick start),
  cross-linked from `MODULES.md` and the root `README.md`
- `docs/ARCHITECTURE.md`, `docs/CONFIGURATION.md`,
  `docs/MIGRATION-GUIDE.md`, `docs/DESIGN.md`
- `Makefile` with cargo-based `build` / `test` / `clippy` / `fmt-check`
  / `sample` / `ci` targets; canonical version via `Makefile.VERSION` +
  `firefly_kernel::VERSION`

### Quality gate

`make ci` = `cargo fmt --all --check` +
`cargo clippy --workspace --all-targets -- -D warnings` +
`cargo build --workspace` + `cargo test --workspace`.
