# Changelog

All notable changes to the Rust port of Firefly Framework.

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
