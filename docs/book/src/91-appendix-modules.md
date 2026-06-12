# Appendix: Module Index

The Firefly Rust workspace ships **67 members** — 65 framework crates under
`crates/`, plus the cross-crate integration suite and the Orders sample. Each
crate carries its own `README.md` with its full public surface, design
rationale, and a runnable quick-start.

The canonical, always-current catalogue is
[`MODULES.md`](https://github.com/fireflyframework/fireflyframework-rust/blob/main/MODULES.md)
in the repository root. This appendix reproduces it tier-by-tier as a navigable
index.

## Foundational

| Crate | What it provides |
|-------|------------------|
| `firefly-kernel` | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, the `FireflyError` hierarchy, task-local correlation/request/tenant scopes, the `ddd` kit (`Entity`/`Specification`/domain events) |
| `firefly-reactive` | The `Mono<T>` / `Flux<T>` reactive core — the Project Reactor analog and keystone of every reactive surface |
| `firefly-utils` | Try helpers, retry with exponential backoff, slug, AES-256-GCM, templates |
| `firefly-validators` | IBAN, BIC, Luhn, currency, phone, password, VAT, national/tax IDs |
| `firefly-web` | Problem renderer, correlation, idempotency, PII masking, CORS, CSRF, security headers, server metrics, the reactive `MonoJson`/`NdJson`/`Sse` responders, TLS bootstrap |
| `firefly-config` | Typed YAML+env+flag binding, profiles, `${...}` placeholders, runtime reload, masked property sources, `ApplicationEventBus`, config-server client |
| `firefly-i18n` | Locale-aware message `Bundle` + Accept-Language picker |
| `firefly-session` | Server-side HTTP sessions + `SessionStore` + `SessionLayer` |

## Platform

| Crate | What it provides |
|-------|------------------|
| `firefly-cache` | `Adapter` port + Memory/NoOp/Fallback + typed `Typed<T>` memoisation |
| `firefly-observability` | `tracing` + correlation enrichment, health composite, startup banner, W3C trace-context, metrics |
| `firefly-data` | Filter DSL, `Page<T>`, `Repository<T, K>`, `ReactiveCrudRepository`, `PostgresReactiveRepository`, derived queries, paging |
| `firefly-cqrs` | Command/query `Bus` with validation/cache/authorization middleware + reactive `send_mono`/`query_mono` |
| `firefly-eda` | `Event` envelope, `Publisher`/`Subscriber`/`Broker` ports, `InMemoryBroker`, glob topics, consumer groups, retry/DLQ, reactive `Flux` subscriptions |
| `firefly-eventsourcing` | Aggregate roots, event store, snapshots, projections, global stream, transactional outbox, multi-tenancy |
| `firefly-orchestration` | `Saga`, `Workflow` (DAG), `Tcc` — compensation, per-step retry, `StepContext` |
| `firefly-rule-engine` | YAML DSL → AST → evaluator with `between`/null/`regex`, `EvaluationMode`, validator + `ActionHandler` |
| `firefly-plugins` | Lifecycle SPI + composite registry |
| `firefly-lifecycle` | `Application::run()` orchestrator with signal trap + drain |
| `firefly-actuator` | `/actuator/{health,info,metrics,env,tasks,version}` + probes, loggers, httpexchanges, threaddump, refresh |
| `firefly-scheduling` | Cron + FixedRate + FixedDelay `Scheduler` with zones |
| `firefly-resilience` | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, composable `Chain` |
| `firefly-security` | `BearerLayer`, RBAC `FilterChain`, `JwksVerifier`, `oauth2`, `RoleHierarchy`, `CsrfLayer`, `BcryptPasswordEncoder` |
| `firefly-migrations` | Versioned forward-only SQL migrations over a `Database` port |
| `firefly-openapi` | OpenAPI 3.1 generator + Swagger-UI shim |
| `firefly-sse` | Server-Sent Events writer with heartbeat + Last-Event-Id |
| `firefly-transactional` | `with_tx(ctx, db, f)` over pluggable `Database`/`Transaction` ports |
| `firefly-testkit` | HMAC signers, `SpyBroker`, JSON test helpers |
| `firefly-aop` | Spring-style aspect advice — `Pointcut`, `JoinPoint`, `Aspect`, `intercept` |
| `firefly-shell` | Spring-Shell-style CLI framework + `CommandLineRunner`/`ApplicationRunner` |
| `firefly-websocket` | WebSocket server over axum + topic `BroadcastHub` |

## Adapters

| Crate | Port → backend |
|-------|----------------|
| `firefly-client` | REST `RestClient` + reactive `WebClient` + SOAP/gRPC/GraphQL/WS |
| `firefly-config-server` | Spring-Cloud-Config-compatible REST endpoint |
| `firefly-idp` + `idp-internal-db` / `idp-keycloak` / `idp-azure-ad` / `idp-aws-cognito` | Identity providers |
| `firefly-ecm` + `ecm-storage-aws` / `ecm-storage-azure` / `ecm-esignature-*` | Content management + e-signature |
| `firefly-notifications` + `notifications-smtp` / `-twilio` / `-firebase` / `-sendgrid` / `-resend` | Notification channels |
| `firefly-callbacks` | Outbound webhook subsystem (HMAC dispatcher + audit + admin) |
| `firefly-webhooks` | Inbound ingestion (Stripe / GitHub / Twilio / generic HMAC + DLQ) |
| `firefly-cache-redis` | `cache::Adapter` → Redis (RESP) |
| `firefly-eda-kafka` / `-rabbitmq` / `-postgres` / `-redis` | `eda::Broker` → Kafka / RabbitMQ / Postgres outbox / Redis Streams |

## Starters

| Crate | What it bundles |
|-------|------------------|
| `firefly-starter-core` | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| `firefly-starter-application` | starter-core + plugins registry |
| `firefly-starter-domain` | starter-core + in-memory event-sourcing stores |
| `firefly-starter-data` | starter-core (you supply the DB) |
| `firefly-backoffice` | starter-application + back-office context middleware |

## DI / Operations / Tooling

| Crate | What it provides |
|-------|------------------|
| `firefly-container` | Opt-in `TypeId`-keyed DI container (service locator) |
| `firefly-admin` | Spring-Boot-Admin-style embedded dashboard + JSON API + SSE |
| `firefly-cli` | The `firefly` developer binary (`new` / `generate` / `db` / `openapi` / `actuator` / `doctor`) |

For per-crate detail, open the crate's `README.md` in the
[repository](https://github.com/fireflyframework/fireflyframework-rust/tree/main/crates),
or read the [`MODULES.md`](https://github.com/fireflyframework/fireflyframework-rust/blob/main/MODULES.md)
catalogue.
