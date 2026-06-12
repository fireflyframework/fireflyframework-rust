# Module index

> Top-level index of every crate shipped in the Firefly Rust workspace.
> Each crate ships its own `README.md` describing its public surface,
> design rationale, and a runnable quick-start snippet — click the
> crate name to open it.

## 01 — Foundational

| Crate | What it provides |
|-------|------------------|
| [`firefly-kernel`](crates/kernel/README.md) | RFC 7807 `ProblemDetail`, `FireflyResult<T>`, `Clock`, `FireflyError` hierarchy, task-local correlation |
| [`firefly-utils`](crates/utils/README.md) | Try helpers, retry with exponential backoff, slug, AES-256-GCM, templates |
| [`firefly-validators`](crates/validators/README.md) | IBAN, BIC, Luhn, currency, phone, password, sort code, VAT, Spanish IDs |
| [`firefly-web`](crates/web/README.md) | Problem renderer, correlation, idempotency, PII masking — composable `tower` layers |
| [`firefly-config`](crates/config/README.md) | Typed YAML+env+flag binding with profile selection |
| [`firefly-i18n`](crates/i18n/README.md) | Locale-aware message `Bundle` + Accept-Language picker |

## 02 — Platform

| Crate | What it provides |
|-------|------------------|
| [`firefly-cache`](crates/cache/README.md) | `Adapter` trait port + Memory / NoOp / Fallback + typed `Typed<T>` |
| [`firefly-observability`](crates/observability/README.md) | `tracing` + correlation enrichment, health composite, startup banner |
| [`firefly-data`](crates/data/README.md) | Filter DSL, `Page<T>`, `Repository<T, K>` |
| [`firefly-cqrs`](crates/cqrs/README.md) | Command + query `Bus` with validation + caching middleware |
| [`firefly-eda`](crates/eda/README.md) | `Event` envelope, `Publisher`/`Subscriber`, in-memory broker, Kafka/RabbitMQ scaffolds |
| [`firefly-eventsourcing`](crates/eventsourcing/README.md) | Aggregate roots + event store + snapshots + projections |
| [`firefly-orchestration`](crates/orchestration/README.md) | `Saga`, `Workflow` (DAG), `Tcc` engines |
| [`firefly-rule-engine`](crates/rule-engine/README.md) | YAML DSL → AST → evaluator (sub-modules: interfaces, models, core, web, sdk) |
| [`firefly-plugins`](crates/plugins/README.md) | Lifecycle SPI + composite registry |
| [`firefly-lifecycle`](crates/lifecycle/README.md) | `Application::run()` orchestrator with signal trap + drain |
| [`firefly-actuator`](crates/actuator/README.md) | `/actuator/{health,info,metrics,env,tasks,version}` |
| [`firefly-scheduling`](crates/scheduling/README.md) | Cron + FixedRate + FixedDelay `Scheduler` |
| [`firefly-resilience`](crates/resilience/README.md) | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, composable `Chain` |
| [`firefly-security`](crates/security/README.md) | `Authentication` extension, `BearerLayer`, RBAC `FilterChain` |
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
| [`firefly-idp-keycloak`](crates/idp-keycloak/README.md) | Keycloak OIDC + admin REST — Stub |
| [`firefly-idp-azure-ad`](crates/idp-azure-ad/README.md) | Azure AD / Entra ID (MSAL + Microsoft Graph) — Stub |
| [`firefly-idp-aws-cognito`](crates/idp-aws-cognito/README.md) | AWS Cognito — Stub |

### Enterprise content management

| Crate | Backing |
|-------|---------|
| [`firefly-ecm`](crates/ecm/README.md) | Adapter framework + LocalStore — **Full** |
| [`firefly-ecm-storage-aws`](crates/ecm-storage-aws/README.md) | AWS S3 — Stub |
| [`firefly-ecm-storage-azure`](crates/ecm-storage-azure/README.md) | Azure Blob Storage — Stub |
| [`firefly-ecm-esignature-docusign`](crates/ecm-esignature-docusign/README.md) | DocuSign — Stub |
| [`firefly-ecm-esignature-adobe-sign`](crates/ecm-esignature-adobe-sign/README.md) | Adobe Sign — Stub |
| [`firefly-ecm-esignature-logalty`](crates/ecm-esignature-logalty/README.md) | Logalty — Stub |

### Notifications

| Crate | Channel |
|-------|---------|
| [`firefly-notifications`](crates/notifications/README.md) | Dispatcher + MemoryChannel — **Full** |
| [`firefly-notifications-sendgrid`](crates/notifications-sendgrid/README.md) | SendGrid (email) — Stub |
| [`firefly-notifications-resend`](crates/notifications-resend/README.md) | Resend (email) — Stub |
| [`firefly-notifications-twilio`](crates/notifications-twilio/README.md) | Twilio (SMS) — Stub |
| [`firefly-notifications-firebase`](crates/notifications-firebase/README.md) | Firebase (push) — Stub |

### Webhooks (outbound + inbound)

| Crate | What it provides |
|-------|------------------|
| [`firefly-callbacks`](crates/callbacks/README.md) | Outbound webhook subsystem (HMAC dispatcher + audit + REST admin + SDK) |
| [`firefly-webhooks`](crates/webhooks/README.md) | Inbound ingestion (Stripe / GitHub / Twilio / generic HMAC validators + DLQ + SDK) |

## 04 — Starters

| Crate | What it bundles |
|-------|------------------|
| [`firefly-starter-core`](crates/starter-core/README.md) | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| [`firefly-starter-application`](crates/starter-application/README.md) | starter-core + plugins registry |
| [`firefly-starter-domain`](crates/starter-domain/README.md) | starter-core + in-memory event-sourcing stores |
| [`firefly-starter-data`](crates/starter-data/README.md) | starter-core (consumer supplies their own DB) |
| [`firefly-backoffice`](crates/backoffice/README.md) | starter-application + back-office context middleware |

## 05 — Tests

| Member | Purpose |
|--------|---------|
| [`tests/integration`](tests/integration) | Cross-crate integration suite (CQRS + callbacks + webhooks + saga roundtrips + starter-core boot) |

## 06 — Samples

| Path | Purpose |
|------|---------|
| [`samples/orders/`](samples/orders) | Reference service demonstrating idempotent POST + cached GET + actuator + lifecycle |
