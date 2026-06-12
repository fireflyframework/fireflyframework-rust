# Module index

> Top-level index of every crate shipped in the Firefly Rust workspace.
> Each crate ships its own `README.md` describing its public surface,
> design rationale, and a runnable quick-start snippet — click the
> crate name to open it.

## 01 — Foundational

| Crate | What it provides |
|-------|------------------|
| [`firefly-kernel`](crates/kernel/README.md) | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, `FireflyError` hierarchy, task-local correlation/request/tenant scopes, `ddd` module (`Entity`/`Specification`/domain events), typed `ErrorResponse` |
| [`firefly-utils`](crates/utils/README.md) | Try helpers, retry with exponential backoff, slug, AES-256-GCM, templates |
| [`firefly-validators`](crates/validators/README.md) | IBAN, BIC, Luhn, currency, phone, password, sort code, VAT, Spanish IDs, `national_id`/`tax_id` |
| [`firefly-web`](crates/web/README.md) | Problem renderer, correlation, idempotency, PII masking, CORS, CSRF (double-submit cookie), security headers, server metrics, content negotiation, `ServerProperties`/TLS bootstrap — composable `tower` layers |
| [`firefly-config`](crates/config/README.md) | Typed YAML+env+flag binding with profile selection, `${key:default}` placeholders, runtime reload/refresh, masked property sources, `accepts_profiles`, `ApplicationEventBus`, config-server client |
| [`firefly-i18n`](crates/i18n/README.md) | Locale-aware message `Bundle` + Accept-Language picker |

## 02 — Platform

| Crate | What it provides |
|-------|------------------|
| [`firefly-cache`](crates/cache/README.md) | `Adapter` trait port + Memory (LRU + stats) / NoOp / Fallback + typed `Typed<T>` (Redis/Postgres adapters in `cache-*`) |
| [`firefly-observability`](crates/observability/README.md) | `tracing` + correlation enrichment, health composite, startup banner, W3C trace-context propagation, log redaction, rolling file appender, console renderer, labeled metrics |
| [`firefly-data`](crates/data/README.md) | Filter DSL, `Page<T>`, `Repository<T, K>`, `Mapper`/`Projection` object mapper, `QueryMethodParser` derived queries, `Pageable`/`Sort`/`Order` |
| [`firefly-cqrs`](crates/cqrs/README.md) | Command + query `Bus` with validation + caching middleware |
| [`firefly-eda`](crates/eda/README.md) | `Event` envelope, `Publisher`/`Subscriber`/`Broker` ports, `InMemoryBroker`, glob topics, consumer groups, `EventFilter` chain, queryable `EdaDeadLetterStore`, `EventPublisherHealthIndicator`, `wrap_listener` retry/DLQ (transports in `eda-*`) |
| [`firefly-eventsourcing`](crates/eventsourcing/README.md) | Aggregate roots + event store + snapshots + projections, global `stream_all` + cross-aggregate projections, multi-tenancy, `EventSourcedRepository` |
| [`firefly-orchestration`](crates/orchestration/README.md) | `Saga` (compensation), `Workflow` (DAG) with step compensation / `wait_all`·`wait_any` / `ChildWorkflowService` / `ContinueAsNew` / conditional + async steps / per-step retry·backoff·timeout · `StepContext` data passing, `Tcc` |
| [`firefly-rule-engine`](crates/rule-engine/README.md) | YAML DSL → AST → evaluator with `between`/null/`regex` operators, `Rule.otherwise`, `EvaluationMode`, ruleset validator + `ActionHandler` (sub-modules: interfaces, models, core, web, sdk) |
| [`firefly-plugins`](crates/plugins/README.md) | Lifecycle SPI + composite registry |
| [`firefly-lifecycle`](crates/lifecycle/README.md) | `Application::run()` orchestrator with signal trap + drain |
| [`firefly-actuator`](crates/actuator/README.md) | `/actuator/{health,info,metrics,env,tasks,version}` + liveness/readiness probes, runtime loggers, `httpexchanges`, `threaddump`, labeled Micrometer metrics, `refresh`, `management.endpoints.web` exposure |
| [`firefly-scheduling`](crates/scheduling/README.md) | Cron + FixedRate + FixedDelay `Scheduler` |
| [`firefly-resilience`](crates/resilience/README.md) | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, composable `Chain` |
| [`firefly-security`](crates/security/README.md) | `Authentication` extension, `BearerLayer`, RBAC `FilterChain`, JWKS `JwksVerifier`, `oauth2` (client registrations + PKCE/OIDC login + authorization server), `RoleHierarchy`, `CsrfLayer`, `PasswordEncoder` (bcrypt) |
| [`firefly-migrations`](crates/migrations/README.md) | Versioned SQL migrations (`V001__init.sql`) over a `Database` port |
| [`firefly-openapi`](crates/openapi/README.md) | OpenAPI 3.1 generator + Swagger-UI shim |
| [`firefly-sse`](crates/sse/README.md) | Server-Sent Events writer w/ heartbeat + Last-Event-Id |
| [`firefly-transactional`](crates/transactional/README.md) | `with_tx(ctx, db, f)` over pluggable `Database` / `Transaction` ports |
| [`firefly-testkit`](crates/testkit/README.md) | HMAC signers, `SpyBroker`, JSON test helpers |

## 03 — Adapters

### Service client + config server

| Crate | What it provides |
|-------|------------------|
| [`firefly-client`](crates/client/README.md) | REST builder with retry, problem decode, correlation propagation; SOAP/gRPC/WS scaffolds |
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
| [`firefly-ecm-storage-aws`](crates/ecm-storage-aws/README.md) | AWS S3 (`S3Store`) — **Full** (+ back-compat stub) |
| [`firefly-ecm-storage-azure`](crates/ecm-storage-azure/README.md) | Azure Blob Storage (`BlobStore`) — **Full** (+ back-compat stub) |
| [`firefly-ecm-esignature-docusign`](crates/ecm-esignature-docusign/README.md) | DocuSign REST v2.1 — **Full** (+ legacy stub) |
| [`firefly-ecm-esignature-adobe-sign`](crates/ecm-esignature-adobe-sign/README.md) | Adobe Sign REST v6 — **Full** (+ legacy stub) |
| [`firefly-ecm-esignature-logalty`](crates/ecm-esignature-logalty/README.md) | Logalty eIDAS REST — **Full** (+ legacy stub) |

### Notifications

| Crate | Channel |
|-------|---------|
| [`firefly-notifications`](crates/notifications/README.md) | Dispatcher + MemoryChannel — **Full** |
| [`firefly-notifications-smtp`](crates/notifications-smtp/README.md) | SMTP email via `lettre` (`SmtpEmailProvider`, real MIME, STARTTLS) — **Full** |
| [`firefly-notifications-sendgrid`](crates/notifications-sendgrid/README.md) | SendGrid (email) — Stub |
| [`firefly-notifications-resend`](crates/notifications-resend/README.md) | Resend (email) — Stub |
| [`firefly-notifications-twilio`](crates/notifications-twilio/README.md) | Twilio (SMS) — **Full** real provider (+ Go-parity stub) |
| [`firefly-notifications-firebase`](crates/notifications-firebase/README.md) | Firebase Cloud Messaging (push) — **Full** real provider (+ Go-parity stub) |

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
| [`firefly-cache-redis`](crates/cache-redis/README.md) | `cache::Adapter` → Redis (`RedisAdapter`, RESP via `redis`) — **Full** |
| [`firefly-cache-postgres`](crates/cache-postgres) | `cache::Adapter` → Postgres key/value table with TTL (`tokio-postgres`) — **Stub** (port pending) |
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
| [`firefly-starter-web`](crates/starter-web) | starter-core + web middleware + security + actuator wiring — **Stub** (port pending) |
| [`firefly-backoffice`](crates/backoffice/README.md) | starter-application + back-office context middleware |

## 05 — DI / AOP / Shell / Sessions / WebSockets

The PyFly-parity cross-cutting crates. These are *opt-in* — none of the
Go-parity core or the starters require them.

| Crate | What it provides |
|-------|------------------|
| [`firefly-container`](crates/container/README.md) | Opt-in, `TypeId`-keyed DI `Container` (service locator): `register_factory`, `resolve`/`resolve_all`, `bind::<dyn Trait>`, `Scope` (Singleton/Prototype/Request/Session/custom), `Provider<T>`, `RefreshScope` — explicit factory closures (no reflective autowiring) |
| [`firefly-aop`](crates/aop/README.md) | Spring-style aspect advice: `Pointcut` glob matcher, `JoinPoint`, `Aspect` (5 hooks), `AspectRegistry`/`AdviceBinding`, `intercept` chain executor with `around`/`Proceed` — explicit weaving at the call site |
| [`firefly-session`](crates/session/README.md) | Server-side HTTP `Session` (typed serde attributes) + async `SessionStore` (`MemorySessionStore` / `CacheSessionStore`) + `SessionLayer` (cookie load/save, id rotation, invalidation, HMAC signing, `SessionRegistry` concurrency control) |
| [`firefly-shell`](crates/shell/README.md) | Spring-Shell-style CLI framework: `CommandSpec` builder, typed `CommandArgs`, `StdShell` parser + REPL, `ApplicationArguments`, `CommandLineRunner`/`ApplicationRunner` + `RunnerRegistry` post-startup hooks |
| [`firefly-websocket`](crates/websocket/README.md) | WebSocket server over axum: `WsSession` (typed send/recv), `WebSocketHandler` lifecycle trait, `ws_route`/`serve_ws` registration, topic `BroadcastHub` fan-out |

## 06 — Operations

| Crate | What it provides |
|-------|------------------|
| [`firefly-admin`](crates/admin) | Spring-Boot-Admin-style embedded dashboard: single-page UI (overview / health / metrics / loggers / mappings / caches / scheduled tasks / traces / CQRS / transactions / beans / config / instances), JSON API over `firefly-actuator`, and SSE live streams |

## 07 — Tooling

| Crate | What it provides |
|-------|------------------|
| [`firefly-cli`](crates/cli/README.md) | The `firefly` developer binary: `new` (project scaffold), `generate`/`g` (handler / entity / command / saga / migration / …), `info`, `doctor` (toolchain checks), `actuator` (remote `/actuator/*` introspection) |

## 08 — Tests

| Member | Purpose |
|--------|---------|
| [`tests/integration`](tests/integration) | Cross-crate integration suite (CQRS + callbacks + webhooks + saga roundtrips + starter-core boot) |

## 09 — Samples

| Path | Purpose |
|------|---------|
| [`samples/orders/`](samples/orders) | Reference service demonstrating idempotent POST + cached GET + actuator + lifecycle |
