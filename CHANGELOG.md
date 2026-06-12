# Changelog

All notable changes to the Rust port of Firefly Framework.

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
