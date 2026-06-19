# Module index

> Top-level index of every crate shipped in the Firefly Rust workspace.
> Each crate ships its own `README.md` describing its public surface,
> design rationale, and a runnable quick-start snippet — click the
> crate name to open it.
>
> **72 framework crates** under `crates/` plus `tests/integration` and three
> samples (`samples/orders`, `samples/reactive-banking`,
> `samples/macro-quickstart`) = **76 workspace members**.

## 00 — Front door (the one-dependency facade + declarative macros)

The Spring-Boot-starter developer experience: one dependency, one prelude,
declarative macros instead of hand-rolled builder wiring.

| Crate | What it provides |
|-------|------------------|
| [`firefly`](crates/firefly/README.md) | The **one-dependency facade**: `use firefly::prelude::*;` brings the whole framework into scope — `Bus`, `Container`, `Scheduler`, `Saga`/`Step`, `Application`/`ShutdownHandle`, `Core`/`CoreConfig`, `WebResult`/`WebError`/`problem_response`, `FireflyError`/`FireflyResult`, `Mono`/`Flux` — plus every macro. Ergonomic per-crate aliases (`firefly::cqrs`, `firefly::web`, …), the hidden `__rt` macro-contract path, and feature-gated heavy adapters (`data-sqlx`, `data-mongodb`, `eda-*`, `cache-*`, `admin`, `full`) |
| [`firefly-macros`](crates/macros/README.md) | The **declarative service layer** (Spring annotations / pyfly decorators): `#[derive(Command)]` / `#[derive(Query)]` (→ `impl Message`), `#[command_handler]` / `#[query_handler]` (→ `register_<fn>(bus)`), `#[derive(Component)]` / `#[derive(Service)]` / `#[derive(Repository)]` + `register_all!` (→ DI registration), `#[scheduled]` (→ `schedule_<fn>(scheduler)`), `#[rest_controller]` + `#[get/post/put/delete/patch]` (→ `routes(state) -> axum::Router`), `#[derive(DomainEvent)]` / `#[derive(AggregateRoot)]`, `#[event_listener]` (→ `subscribe_<fn>(broker)`), method security `#[pre_authorize]` / `#[post_authorize]` (keyword rules **or** SpEL-style expressions over arguments + principal) and `#[pre_filter]` / `#[post_filter]` collection filtering. Generated code targets the `firefly` facade's `__rt` path |

## 01 — Foundational

| Crate | What it provides |
|-------|------------------|
| [`firefly-reactive`](crates/reactive/README.md) | The **`Mono<T>` / `Flux<T>` reactive core** — the Project Reactor / WebFlux analog and keystone of every reactive surface: lazy `FireflyError`-typed publishers, `Scheduler` (`Immediate`/`Parallel`/`BoundedElastic`), `FluxSink`, `Backoff` retry, full operator set (map/flat_map/filter/reduce/merge/zip/retry/timeout/backpressure/window) |
| [`firefly-kernel`](crates/kernel/README.md) | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, `FireflyError` hierarchy, task-local correlation/request/tenant scopes, `ddd` module (`Entity`/`Specification`/domain events), typed `ErrorResponse` |
| [`firefly-utils`](crates/utils/README.md) | Try helpers, retry with exponential backoff, slug, AES-256-GCM, templates |
| [`firefly-validators`](crates/validators/README.md) | IBAN, BIC, Luhn, currency, phone, password, sort code, VAT, Spanish IDs, `national_id`/`tax_id` |
| [`firefly-web`](crates/web/README.md) | Problem renderer, correlation, idempotency, PII masking, CORS, CSRF (double-submit cookie), security headers, server metrics, content negotiation, `ServerProperties`/TLS bootstrap, the reactive `MonoJson`/`NdJson`/`Sse`/`SseEvents` responders (NDJSON/SSE streaming with backpressure) — composable `tower` layers |
| [`firefly-config`](crates/config/README.md) | Typed YAML+env+flag binding with profile selection, `${key:default}` placeholders, runtime reload/refresh, masked property sources, `accepts_profiles`, `ApplicationEventBus`, config-server client |
| [`firefly-i18n`](crates/i18n/README.md) | Locale-aware message `Bundle` + Accept-Language picker |

## 02 — Platform

| Crate | What it provides |
|-------|------------------|
| [`firefly-cache`](crates/cache/README.md) | `Adapter` trait port + Memory (LRU + stats) / NoOp / Fallback + typed `Typed<T>` (Redis/Postgres adapters in `cache-*`) |
| [`firefly-observability`](crates/observability/README.md) | `tracing` + correlation enrichment, health composite, startup banner, W3C trace-context propagation, log redaction, rolling file appender, console renderer, labeled metrics |
| [`firefly-data`](crates/data/README.md) | Filter DSL, composable `Specification`, `Page<T>`, `Repository<T, K>`, `Mapper`/`Projection` object mapper, `QueryMethodParser` derived queries, `Pageable`/`Sort`/`Order`, auditing (`Auditor`/`AuditStamps`/`UserProvider`) + soft-delete (`SoftDeletePolicy`), the `SqlDialect` abstraction (`PostgresDialect`/`MySqlDialect`/`SqliteDialect` — `Filter::to_sql_with`/`Specification::to_sql_with`) and `Specification::to_mongo()` document lowering, and the **reactive** `ReactiveCrudRepository<T, ID>` / `ReactiveSpecificationRepository` (in-memory `ReactiveMemoryRepository` + real `PostgresReactiveRepository` streaming rows as `Flux<T>`). The storage-agnostic **ports** the `data-sqlx` / `data-mongodb` adapters implement |
| [`firefly-cqrs`](crates/cqrs/README.md) | Command + query `Bus` with validation + caching middleware, plus **reactive** `send_mono`/`query_mono` (+ `_with_context`) returning `Mono<R>` |
| [`firefly-eda`](crates/eda/README.md) | `Event` envelope, `Publisher`/`Subscriber`/`Broker` ports, `InMemoryBroker`, glob topics, consumer groups, `EventFilter` chain, queryable `EdaDeadLetterStore`, `EventPublisherHealthIndicator`, `wrap_listener` retry/DLQ, plus **reactive** `subscribe_reactive` → `Flux<Event>` and `publish_mono` (transports in `eda-*`) |
| [`firefly-eventsourcing`](crates/eventsourcing/README.md) | Aggregate roots + event store + snapshots + projections, global `stream_all` + cross-aggregate projections, multi-tenancy, `EventSourcedRepository` |
| [`firefly-orchestration`](crates/orchestration/README.md) | `Saga` (compensation), `Workflow` (DAG) with step compensation / `wait_all`·`wait_any` / `ChildWorkflowService` / `ContinueAsNew` / conditional + async steps / per-step retry·backoff·timeout · `StepContext` data passing, `Tcc` |
| [`firefly-rule-engine`](crates/rule-engine/README.md) | YAML DSL → AST → evaluator with `between`/null/`regex` operators, `Rule.otherwise`, `EvaluationMode`, ruleset validator + `ActionHandler` (sub-modules: interfaces, models, core, web, sdk) |
| [`firefly-plugins`](crates/plugins/README.md) | Lifecycle SPI + composite registry |
| [`firefly-lifecycle`](crates/lifecycle/README.md) | `Application::run()` orchestrator with signal trap + drain |
| [`firefly-actuator`](crates/actuator/README.md) | `/actuator/{health,info,metrics,env,tasks,version}` + liveness/readiness probes, runtime loggers, `httpexchanges`, `threaddump`, labeled Micrometer metrics, `refresh`, `management.endpoints.web` exposure |
| [`firefly-scheduling`](crates/scheduling/README.md) | Cron + FixedRate + FixedDelay `Scheduler` |
| [`firefly-resilience`](crates/resilience/README.md) | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, composable `Chain` |
| [`firefly-security`](crates/security/README.md) | `Authentication` extension, `BearerLayer`, RBAC `FilterChain` (`ROLE_`-aware, path-segment-safe), the authentication spine (`AuthenticationManager`/`ProviderManager`, `UserDetails`+`DaoAuthenticationProvider`, `SecurityContextRepository`, `DelegatingPasswordEncoder`), web mechanisms (`httpBasic`, `formLogin`, `TokenBasedRememberMeServices`, `RequestCache`, `SessionCreationPolicy`, `SecurityFilterChains`), method-security depth (`PermissionEvaluator` + `has_permission`, consumed by the expression `#[pre_authorize]`/`#[post_authorize]`/`#[pre_filter]`/`#[post_filter]` macros), JWKS `JwksVerifier` (RSA/EC/EdDSA, `nbf` + clock-skew), `oauth2` (PKCE/OIDC login + RP-initiated logout, outbound `AuthorizedClientManager`, RFC 7662 opaque-token introspection, authorization server + RFC 8414 `AuthorizationServerRouter`), **one-time-token + WebAuthn/passkey** passwordless login, `RoleHierarchy`, `CsrfLayer`, `PasswordEncoder` (bcrypt + Argon2id) — Spring Security 6-faithful (see the book's *Spring Security Parity* appendix) |
| [`firefly-migrations`](crates/migrations/README.md) | Versioned SQL migrations (`V001__init.sql`) over a `Database` port |
| [`firefly-openapi`](crates/openapi/README.md) | OpenAPI 3.1 generator + Swagger-UI shim |
| [`firefly-sse`](crates/sse/README.md) | Server-Sent Events writer w/ heartbeat + Last-Event-Id |
| [`firefly-transactional`](crates/transactional/README.md) | `with_tx(ctx, db, f)` over pluggable `Database` / `Transaction` ports |
| [`firefly-testkit`](crates/testkit/README.md) | HMAC signers (Stripe / GitHub / HMAC / Twilio), `SpyBroker` + `assert_event_published`/`assert_event_published_with`, JSON test helpers, a `TestClient`/`TestResponse` in-process axum router driver (fluent `assert_status`/`assert_json_eq`/…), and DI test `Slice`/`BuiltSlice` (the pyfly `slice_context`/`mock_bean` analog) |

## 03 — Adapters

### Service client + config server

| Crate | What it provides |
|-------|------------------|
| [`firefly-client`](crates/client/README.md) | REST builder with retry, problem decode, correlation propagation; the **reactive `WebClient`** (`WebClientBuilder` → `get`/`post`/… → `retrieve().body_to_mono::<T>()` / `body_to_flux::<T>()` / `exchange()`); SOAP/gRPC/WS scaffolds |
| [`firefly-config-server`](crates/config-server/README.md) | Spring-Cloud-Config-compatible REST endpoint |

### Identity providers

| Crate | Backing |
|-------|---------|
| [`firefly-idp`](crates/idp/README.md) | Common `Adapter` trait port |
| [`firefly-idp-internal-db`](crates/idp-internal-db/README.md) | Self-hosted (bcrypt + HS256 JWT) — **Full** |
| [`firefly-idp-keycloak`](crates/idp-keycloak/README.md) | Keycloak OIDC + admin REST over `reqwest` — **Full** |
| [`firefly-idp-azure-ad`](crates/idp-azure-ad/README.md) | Azure AD / Entra ID (Microsoft Graph + ROPC) — **Full** |
| [`firefly-idp-aws-cognito`](crates/idp-aws-cognito/README.md) | AWS Cognito (JSON API + self-contained SigV4) — **Full** |

### Enterprise content management

| Crate | Backing |
|-------|---------|
| [`firefly-ecm`](crates/ecm/README.md) | Adapter framework + LocalStore — **Full** |
| [`firefly-ecm-storage-aws`](crates/ecm-storage-aws/README.md) | AWS S3 (`S3Store`, real REST + SigV4) — **Full** |
| [`firefly-ecm-storage-azure`](crates/ecm-storage-azure/README.md) | Azure Blob Storage (`BlobStore`, real REST) — **Full** |
| [`firefly-ecm-esignature-docusign`](crates/ecm-esignature-docusign/README.md) | DocuSign eSignature REST v2.1 (`RestProvider`) — **Full** |
| [`firefly-ecm-esignature-adobe-sign`](crates/ecm-esignature-adobe-sign/README.md) | Adobe Sign REST v6 — **Full** |
| [`firefly-ecm-esignature-logalty`](crates/ecm-esignature-logalty/README.md) | Logalty eIDAS REST — **Full** |

### Notifications

| Crate | Channel |
|-------|---------|
| [`firefly-notifications`](crates/notifications/README.md) | Dispatcher + MemoryChannel — **Full** |
| [`firefly-notifications-smtp`](crates/notifications-smtp/README.md) | SMTP email via `lettre` (`SmtpEmailProvider`, real MIME, STARTTLS) — **Full** |
| [`firefly-notifications-sendgrid`](crates/notifications-sendgrid/README.md) | SendGrid email via v3 `/mail/send` over `reqwest` (`SendGridEmailProvider` + envelope `Channel`) — **Full** |
| [`firefly-notifications-resend`](crates/notifications-resend/README.md) | Resend email via `POST /emails` over `reqwest` (`ResendEmailProvider` + envelope `Channel`) — **Full** |
| [`firefly-notifications-twilio`](crates/notifications-twilio/README.md) | Twilio SMS via the Messages REST API — **Full** |
| [`firefly-notifications-firebase`](crates/notifications-firebase/README.md) | Firebase Cloud Messaging (push) via the FCM HTTP v1 API — **Full** |

### Webhooks (outbound + inbound)

| Crate | What it provides |
|-------|------------------|
| [`firefly-callbacks`](crates/callbacks/README.md) | Outbound webhook subsystem (HMAC dispatcher + audit + REST admin + SDK) |
| [`firefly-webhooks`](crates/webhooks/README.md) | Inbound ingestion (Stripe / GitHub / Twilio / generic HMAC validators + DLQ + SDK) |

### Infrastructure adapters

Optional leaf crates implementing a platform port over a real backing
library, selected at wiring time. The heavy SDK dependency
(`rdkafka` / `lapin` / `tokio-postgres` / `redis` / `lettre`) is pulled
in only by services that select that backend.

| Crate | Port → backend |
|-------|----------------|
| [`firefly-data-sqlx`](crates/data-sqlx/README.md) | `firefly_data` ports → **relational** (Postgres / MySQL / SQLite over `sqlx`): `SqlxRepository` / `SqlxReactiveRepository`, dialect-aware `UPSERT`, streams reads as `Flux<T>`, auto auditing + soft-delete (`Db`/`Backend`, `SqlxRowMapper`/`AnyRow`, `ColumnValue`/`RowWriter`) — **Full** |
| [`firefly-data-mongodb`](crates/data-mongodb/README.md) | `firefly_data` ports → **document** store (MongoDB via `mongodb`): `MongoRepository<T, ID>` over the *same* `ReactiveCrudRepository` / `ReactiveSpecificationRepository` traits, lowering `Specification::to_mongo()`, `BaseDocument` audit/soft-delete mixin, `Audited` hook — **Full** |
| [`firefly-cache-redis`](crates/cache-redis/README.md) | `cache::Adapter` → Redis (`RedisAdapter`, RESP via `redis`) — **Full** |
| [`firefly-cache-postgres`](crates/cache-postgres/README.md) | `cache::Adapter` → Postgres key/value table with TTL (`PostgresCacheAdapter`, real SQL over `tokio-postgres`) — **Full** |
| [`firefly-eda-kafka`](crates/eda-kafka/README.md) | `eda::Broker` → Apache Kafka (`KafkaBroker`, `new_kafka_broker`, `rdkafka`) — **Full** |
| [`firefly-eda-rabbitmq`](crates/eda-rabbitmq/README.md) | `eda::Broker` → RabbitMQ (`RabbitMqBroker`, durable direct exchange, publisher confirms, `lapin`) — **Full** |
| [`firefly-eda-postgres`](crates/eda-postgres/README.md) | `eda::Broker` → Postgres transactional outbox + `LISTEN`/`NOTIFY` (`PostgresBroker`, advisory-lock drain) — **Full** |
| [`firefly-eda-redis`](crates/eda-redis/README.md) | `eda::Broker` → Redis Streams consumer groups (`RedisStreamsBroker`, `new_redis_broker`) — **Full** |

(The notifications-`smtp` adapter is grouped with Notifications above.)

## 04 — Starters

| Crate | What it bundles |
|-------|------------------|
| [`firefly-starter-core`](crates/starter-core/README.md) | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| [`firefly-starter-application`](crates/starter-application/README.md) | starter-core + plugins registry |
| [`firefly-starter-domain`](crates/starter-domain/README.md) | starter-core + in-memory event-sourcing stores |
| [`firefly-starter-data`](crates/starter-data/README.md) | starter-core (consumer supplies their own DB) |
| [`firefly-starter-web`](crates/starter-web/README.md) | `WebStack` — `Core` + CORS + security headers + request metrics + access log (web batteries on by default, optional `FilterChain` security) — **Full** |
| [`firefly-backoffice`](crates/backoffice/README.md) | starter-application + back-office context middleware |

## 05 — DI / AOP / Shell / Sessions / WebSockets

The PyFly-parity cross-cutting crates. These are *opt-in* — none of the
Go-parity core or the starters require them.

| Crate | What it provides |
|-------|------------------|
| [`firefly-container`](crates/container/README.md) | Opt-in, `TypeId`-keyed DI `Container` (service locator): `register_factory`, `resolve`/`resolve_all`, `bind::<dyn Trait>`, `Scope` (Singleton/Prototype/Request/Session/custom), `Provider<T>`, `RefreshScope` — explicit factory closures (no reflective autowiring) |
| [`firefly-aop`](crates/aop/README.md) | Spring-style aspect advice: `Pointcut` glob matcher, `JoinPoint`, `Aspect` (5 hooks), `AspectRegistry`/`AdviceBinding`, `intercept` chain executor with `around`/`Proceed` — explicit weaving at the call site |
| [`firefly-session`](crates/session/README.md) | Server-side HTTP `Session` (typed serde attributes) + async `SessionStore` (`MemorySessionStore` / `CacheSessionStore`) + `SessionLayer` (cookie load/save, id rotation, invalidation, HMAC signing, `SessionRegistry` concurrency control via `MemorySessionRegistry`) |
| [`firefly-session-redis`](crates/session-redis/README.md) | A **distributed** `SessionRegistry` over a Redis sorted set (`RedisSessionRegistry`, `ZADD`/`ZRANGE`/`ZREM`/`ZCARD`, sliding `EXPIRE`): the per-principal concurrency cap holds cluster-wide, not just in one process — **Full** |
| [`firefly-session-postgres`](crates/session-postgres/README.md) | A **durable, distributed** `SessionRegistry` over a Postgres table (`PostgresSessionRegistry`, idempotent `ON CONFLICT` upsert, `ORDER BY created_at` oldest-first) for relational-only deployments — **Full** |
| [`firefly-shell`](crates/shell/README.md) | Spring-Shell-style CLI framework: `CommandSpec` builder, typed `CommandArgs`, `StdShell` parser + REPL, `ApplicationArguments`, `CommandLineRunner`/`ApplicationRunner` + `RunnerRegistry` post-startup hooks |
| [`firefly-websocket`](crates/websocket/README.md) | WebSocket server over axum: `WsSession` (typed send/recv), `WebSocketHandler` lifecycle trait, `ws_route`/`serve_ws` registration, topic `BroadcastHub` fan-out |

## 06 — Operations

| Crate | What it provides |
|-------|------------------|
| [`firefly-admin`](crates/admin/README.md) | Spring-Boot-Admin-style embedded dashboard: single-page UI (overview / health / metrics / loggers / mappings / caches / scheduled tasks / traces / CQRS / transactions / beans / config / instances), JSON API over `firefly-actuator`, and SSE live streams |

## 07 — Tooling

| Crate | What it provides |
|-------|------------------|
| [`firefly-cli`](crates/cli/README.md) | The `firefly` developer binary: `new` (project scaffold), `generate`/`g` (handler / entity / command / saga / migration / …), `info`, `doctor` (toolchain checks), `db` (migrate / upgrade), `openapi` (spec gen), `completion` (shell-completion scripts), `sbom` (dependency SBOM), `license` (dependency-license report), and remote actuator introspection (`actuator` / `routes` / `env` / `health` / `metrics`) |

## 08 — Tests

| Member | Purpose |
|--------|---------|
| [`tests/integration`](tests/integration) | Cross-crate integration suite (CQRS + callbacks + webhooks + saga roundtrips + starter-core boot) |

## 09 — Samples

| Path | Purpose |
|------|---------|
| [`samples/orders/`](samples/orders) | Reference service demonstrating idempotent POST + cached GET + actuator + lifecycle |
| [`samples/reactive-banking/`](samples/reactive-banking) | End-to-end **reactive** service (`firefly-sample-reactive-banking`): reactive CQRS (`Bus::send_mono`/`query_mono`), event sourcing, a saga-backed money transfer, a `Flux<AccountEvent>` NDJSON/SSE stream, JWT-secured `starter-web`, and a `WebClient` SDK — running on in-memory defaults or real Postgres/Kafka |
| [`samples/macro-quickstart/`](samples/macro-quickstart) | The **declarative** one-dependency DX (`firefly-sample-macro-quickstart`): the orders behaviour re-expressed through `firefly-macros` over the single `firefly` facade — `#[derive(Command)]`/`#[query_handler]`/`#[rest_controller]`/`#[derive(Component)]`/`#[scheduled]` instead of hand-rolled wiring (376 source lines vs 1022, two modules vs seven) |
