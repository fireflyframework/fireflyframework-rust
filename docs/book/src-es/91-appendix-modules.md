# Apéndice: Índice de módulos

El workspace de Firefly Rust incluye **86 miembros**: 74 crates del framework bajo
`crates/`, además de la suite de integración entre crates y 11 entradas de ejemplo: el
ejemplo recurrente del libro, [`lumen`](https://github.com/fireflyframework/fireflyframework-rust/tree/main/samples/lumen),
junto a `orders`, `reactive-banking`, `macro-quickstart`, `linkspike-core`,
`linkspike-web` y el ejemplo multimódulo `lumen-ledger`. Cada crate
incluye su propio `README.md` con su superficie pública completa, su justificación de diseño y un
inicio rápido ejecutable.

El catálogo canónico y siempre actualizado es
[`MODULES.md`](https://github.com/fireflyframework/fireflyframework-rust/blob/main/MODULES.md)
en la raíz del repositorio. Este apéndice lo reproduce nivel a nivel como un
índice navegable.

## Puerta de entrada

| Crate | Qué proporciona |
|-------|------------------|
| `firefly` | La fachada de dependencia única: `use firefly::prelude::*;`, alias por crate, el contrato de la macro `__rt`, adaptadores activados por features |
| `firefly-macros` | Macros declarativas: `#[derive(Command/Query/Component/DomainEvent/AggregateRoot)]`, `#[command_handler]`/`#[query_handler]`, `#[scheduled]`, `#[rest_controller]`+verbos, `#[event_listener]` |

## Cimientos

| Crate | Qué proporciona |
|-------|------------------|
| `firefly-kernel` | `ProblemDetail` según RFC 7807, `FireflyResult<T>`, `Clock`, la jerarquía `FireflyError`, ámbitos task-local de correlación/petición/tenant, el kit `ddd` (`Entity`/`Specification`/eventos de dominio) |
| `firefly-reactive` | El núcleo reactivo `Mono<T>` / `Flux<T>`: la piedra angular de cada superficie reactiva de Firefly |
| `firefly-utils` | Helpers de try, reintentos con backoff exponencial, slug, AES-256-GCM, plantillas |
| `firefly-validators` | IBAN, BIC, Luhn, divisa, teléfono, contraseña, IVA, identificadores nacionales/fiscales |
| `firefly-web` | Renderizador de problemas, correlación, idempotencia, enmascaramiento de PII, CORS, CSRF, cabeceras de seguridad, métricas de servidor, los responders reactivos `MonoJson`/`NdJson`/`Sse`, arranque de TLS |
| `firefly-config` | Binding tipado de YAML+env+flags, perfiles, marcadores `${...}`, recarga en tiempo de ejecución, fuentes de propiedades enmascaradas, `ApplicationEventBus`, cliente de config-server |
| `firefly-i18n` | `Bundle` de mensajes con reconocimiento de locale + selector de Accept-Language |
| `firefly-session` | Sesiones HTTP en el lado servidor + `SessionStore` + `SessionLayer` |

## Plataforma

| Crate | Qué proporciona |
|-------|------------------|
| `firefly-cache` | Puerto `Adapter` + Memory/NoOp/Fallback + memoización tipada `Typed<T>` |
| `firefly-observability` | `tracing` + enriquecimiento por correlación, composite de health, banner de arranque, trace-context W3C, métricas |
| `firefly-data` | Puertos agnósticos al almacenamiento: DSL de filtros + `Specification`, `SqlDialect` (pg/mysql/sqlite) + `Specification::to_mongo()`, `Page<T>`, `Repository<T, K>`, `ReactiveCrudRepository`, `PostgresReactiveRepository`, auditoría + borrado lógico, consultas derivadas, paginación |
| `firefly-cqrs` | `Bus` de comandos/consultas con middleware de validación/caché/autorización + `send_mono`/`query_mono` reactivos |
| `firefly-eda` | Envoltura `Event`, puertos `Publisher`/`Subscriber`/`Broker`, `InMemoryBroker`, topics con globs, grupos de consumidores, reintentos/DLQ, suscripciones reactivas `Flux` |
| `firefly-eventsourcing` | Raíces de agregado, event store, snapshots, proyecciones, stream global, outbox transaccional, multitenencia |
| `firefly-orchestration` | `Saga`, `Workflow` (DAG), `Tcc`: compensación, reintento por paso, `StepContext` |
| `firefly-rule-engine` | DSL en YAML → AST → evaluador con `between`/null/`regex`, `EvaluationMode`, validador + `ActionHandler` |
| `firefly-plugins` | SPI de ciclo de vida + registro compuesto |
| `firefly-lifecycle` | Orquestador `Application::run()` con captura de señales + drenaje |
| `firefly-actuator` | `/actuator/{health,info,metrics,env,tasks,version}` + probes, loggers, httpexchanges, threaddump, refresh, y los informes de introspección de DI `beans`/`mappings`/`conditions` (renderizados a partir del inventario en tiempo de compilación de `firefly-container`) |
| `firefly-scheduling` | `Scheduler` Cron + FixedRate + FixedDelay con zonas |
| `firefly-resilience` | `CircuitBreaker`, `RateLimiter`, `Bulkhead`, `Timeout`, `Chain` componible |
| `firefly-security` | `BearerLayer`, `FilterChain` RBAC, `JwksVerifier`, `oauth2`, `RoleHierarchy`, `CsrfLayer`, `BcryptPasswordEncoder` |
| `firefly-migrations` | Migraciones SQL versionadas solo hacia adelante sobre un puerto `Database` |
| `firefly-openapi` | Generador OpenAPI 3.1 + shim de Swagger-UI |
| `firefly-sse` | Escritor de Server-Sent Events con heartbeat + Last-Event-Id |
| `firefly-transactional` | `with_tx(ctx, db, f)` sobre puertos `Database`/`Transaction` enchufables |
| `firefly-testkit` | Firmantes HMAC, `SpyBroker`, helpers de test JSON |
| `firefly-aop` | Consejo orientado a aspectos: `Pointcut`, `JoinPoint`, `Aspect`, `intercept` |
| `firefly-shell` | Framework de CLI interactiva + hooks de arranque `CommandLineRunner`/`ApplicationRunner` |
| `firefly-websocket` | Servidor WebSocket sobre axum + `BroadcastHub` por topic |

## Adaptadores

| Crate | Puerto → backend |
|-------|----------------|
| `firefly-client` | `RestClient` REST + `WebClient` reactivo + SOAP/gRPC/GraphQL/WS |
| `firefly-config-server` | Endpoint REST de configuración centralizada para servicios distribuidos |
| `firefly-idp` + `idp-internal-db` / `idp-keycloak` / `idp-azure-ad` / `idp-aws-cognito` | Proveedores de identidad |
| `firefly-ecm` + `ecm-storage-aws` / `ecm-storage-azure` / `ecm-esignature-*` | Gestión de contenidos + firma electrónica |
| `firefly-notifications` + `notifications-smtp` / `-twilio` / `-firebase` / `-sendgrid` / `-resend` | Canales de notificación |
| `firefly-callbacks` | Subsistema de webhooks salientes (dispatcher HMAC + auditoría + administración) |
| `firefly-webhooks` | Ingestión entrante (Stripe / GitHub / Twilio / HMAC genérico + DLQ) |
| `firefly-data-sqlx` | Puertos `firefly_data` → relacional (Postgres / MySQL / SQLite sobre `sqlx`) |
| `firefly-data-mongodb` | Puertos `firefly_data` → almacén documental (MongoDB) |
| `firefly-cache-redis` / `firefly-cache-postgres` | `cache::Adapter` → Redis (RESP) / tabla clave-valor de Postgres |
| `firefly-eda-kafka` / `-rabbitmq` / `-postgres` / `-redis` | `eda::Broker` → Kafka / RabbitMQ / outbox de Postgres / Redis Streams |
| `firefly-session-redis` / `firefly-session-postgres` / `firefly-session-mongodb` | `SessionRegistry` distribuido → sorted set de Redis / tabla de Postgres / colección de MongoDB |

## Starters

| Crate | Qué empaqueta |
|-------|------------------|
| `firefly-starter-core` | web + cache + observability + eda + cqrs + actuator + lifecycle + scheduling |
| `firefly-starter-application` | starter-core + registro de plugins |
| `firefly-starter-domain` | starter-core + stores de event-sourcing en memoria |
| `firefly-starter-data` | starter-core (tú aportas la BD) |
| `firefly-starter-web` | `WebStack`: `Core` + CORS + cabeceras de seguridad + métricas de petición + access log |
| `firefly-starter-experience` | `ExperienceStack` (alias `Bff`): `WebStack` + `DomainClients` (el `ClientFactory`) + gates de `SignalService` + `WorkflowState` con soporte de Redis + `WorkflowQueryService` + `ChildWorkflowService`: el nivel de experiencia (BFF) |
| `firefly-backoffice` | starter-application + middleware de contexto de back-office |

## DI / Operaciones / Tooling

| Crate | Qué proporciona |
|-------|------------------|
| `firefly-container` | Contenedor de DI opcional con claves por `TypeId` (service locator) |
| `firefly-admin` | Dashboard de gestión embebido + API JSON + streams SSE |
| `firefly-cli` | El binario de desarrollo `firefly` (`new` / `generate` / `db` / `openapi` / `actuator` / `doctor` / `completion` / `sbom` / `license`) |

Para el detalle por crate, abre el `README.md` del crate en el
[repositorio](https://github.com/fireflyframework/fireflyframework-rust/tree/main/crates),
o lee el catálogo [`MODULES.md`](https://github.com/fireflyframework/fireflyframework-rust/blob/main/MODULES.md).
