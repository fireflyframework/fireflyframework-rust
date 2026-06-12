# Java → Rust Migration Guide

This guide is the cookbook for porting an existing Firefly Java service
(or a sibling .NET / Go / Python service) to the Rust port. Each
section maps a Java / Spring concept to its idiomatic Rust translation.

## Module references

```
Java                                 → Rust crate
org.fireflyframework:firefly-common  → firefly-kernel
firefly-common-utils                 → firefly-utils
firefly-common-validators            → firefly-validators
firefly-web                          → firefly-web
firefly-common-cache                 → firefly-cache
firefly-otel-spring-boot-starter     → firefly-observability
firefly-common-data                  → firefly-data
firefly-common-cqrs                  → firefly-cqrs
firefly-common-eda                   → firefly-eda
firefly-event-sourcing-…             → firefly-eventsourcing
firefly-common-domain (orchestration)→ firefly-orchestration
firefly-common-rule-engine           → firefly-rule-engine
firefly-platform-plugins             → firefly-plugins
firefly-service-client               → firefly-client
firefly-config-server                → firefly-config-server
firefly-idp + firefly-idp-*          → firefly-idp / firefly-idp-{internal-db,keycloak,azure-ad,aws-cognito}
firefly-ecm + firefly-ecm-*          → firefly-ecm / firefly-ecm-{storage,esignature}-*
firefly-notifications + …            → firefly-notifications / firefly-notifications-{sendgrid,resend,twilio,firebase}
firefly-callbacks                    → firefly-callbacks
firefly-webhooks                     → firefly-webhooks
spring-boot @ConfigurationProperties → firefly-config
spring-boot SpringApplication        → firefly-lifecycle
spring-boot-starter-actuator         → firefly-actuator
spring @Scheduled                    → firefly-scheduling
resilience4j-*                       → firefly-resilience
spring-security                      → firefly-security
flyway                               → firefly-migrations
springdoc-openapi                    → firefly-openapi
spring MessageSource                 → firefly-i18n
spring ServerSentEvent               → firefly-sse
spring @Transactional                → firefly-transactional
spring-boot-starter-test             → firefly-testkit

# PyFly-parity layer (additive)
spring @Component/@Autowired DI      → firefly-container
spring AOP / @Aspect                 → firefly-aop
spring HttpSession / Spring Session  → firefly-session
spring-shell @ShellMethod            → firefly-shell
jakarta @ServerEndpoint / WebSocket  → firefly-websocket
spring-boot CommandLineRunner        → firefly-shell (RunnerRegistry)
spring-boot-admin                    → firefly-admin
spring-security OAuth2 resource/auth → firefly-security (oauth2)
spring Kafka / @KafkaListener        → firefly-eda-kafka
spring AMQP / RabbitMQ               → firefly-eda-rabbitmq
(transactional outbox over Postgres) → firefly-eda-postgres
(Redis Streams listener)             → firefly-eda-redis
spring-data-redis cache              → firefly-cache-redis
spring JavaMailSender (SMTP)         → firefly-notifications-smtp
```

## Spring concept mapping

Each entry is wire-shape compatible with its Java counterpart but
expressed in idiomatic Rust (explicit construction, `tower` middleware
composition, `async`/`await`).

| Spring concept                           | Rust crate + entry point                                                            |
|------------------------------------------|--------------------------------------------------------------------------------------|
| Spring WebFlux (Netty)                   | `axum` router on `tokio`, middleware as `tower::Layer`s                              |
| Reactor `Mono<T>` / `Flux<T>`            | `async fn -> FireflyResult<T>` / `impl Stream<Item = T>`                             |
| `@Component` / `@Autowired` DI container | Explicit construction by default; ports injected as `Arc<dyn Trait>`; `Core::new(CoreConfig)` wires the infrastructure tier. For a service-locator surface, opt into `firefly_container::Container` (`register_factory` / `resolve` / `bind::<dyn Trait>`) |
| `@Aspect` + `@Before/@Around/@AfterReturning/@AfterThrowing/@After` | `firefly_aop::{AspectRegistry, Aspect, intercept}` — pointcut globs over qualified names, weaving at the call site (no monkey-patching) |
| Spring Shell `@ShellMethod` / `@ShellOption` | `firefly_shell::{CommandSpec, StdShell}` builder + typed `CommandArgs`; `CommandLineRunner` / `ApplicationRunner` + `RunnerRegistry` for post-startup hooks |
| `HttpSession` / Spring Session         | `firefly_session::{Session, SessionLayer, SessionStore}` — typed serde attributes, cookie load/save, id rotation, concurrency control |
| `@ServerEndpoint` / Spring WebSocket   | `firefly_websocket::{WebSocketHandler, ws_route, WsSession}` on axum; `BroadcastHub` for topic fan-out |
| Spring Boot Admin                      | `firefly-admin` — embedded dashboard SPA + `/admin/api/*` JSON over `firefly-actuator` + SSE; server/client modes via `firefly.admin.*` |
| Spring Security OAuth2 (resource + authorization server) | `firefly_security::oauth2` — `JwksVerifier`, `ClientRegistration`, `OAuth2LoginHandler` (PKCE/OIDC), `AuthorizationServer` (client-credentials + refresh) |
| `@KafkaListener` / Spring AMQP / message brokers | `firefly-eda-{kafka,rabbitmq,postgres,redis}` — same `Broker` port, swap the constructor (see below) |
| `spring-data-redis` cache / `JavaMailSender` | `firefly_cache_redis::RedisAdapter` / `firefly_notifications_smtp::SmtpEmailProvider` |
| `application.yaml` + `@ConfigurationProperties` | `firefly_config::load::<T>(&sources)` / `load_from_profile(dir, app, fallback)`|
| `SpringApplication.run()`                | `core.new_application().on_server(..).run().await` (`firefly-lifecycle`)             |
| `spring-boot-starter-actuator`           | `core.actuator_router(info_contributors)` → `/actuator/{health,info,metrics,env,tasks,version}` |
| `@Scheduled(cron="0 9 * * MON-FRI")`     | `scheduler.cron("name", "0 9 * * 1-5", run)` / `fixed_rate(..)` / `fixed_delay(..)`  |
| `@CircuitBreaker @RateLimiter @Bulkhead` | `firefly_resilience::Chain` composing `Timeout`, `CircuitBreaker`, `Bulkhead`, `RateLimiter` |
| `@PreAuthorize("hasRole('ADMIN')")`      | `FilterChain::new().require("/admin/", "ADMIN")` + `BearerLayer`                     |
| R2DBC repositories                       | `firefly_data::Repository<T, K>` trait + `Filter` DSL + `Page<T>`                    |
| `@Transactional`                         | `with_tx(&TxContext::root(), &db, \|ctx\| { .. })` (`firefly-transactional`)         |
| Flyway `V001__init.sql`                  | `firefly_migrations::run` over a `Database` port with a `DirSource` / `EmbeddedSource` |
| `@RestController` + springdoc            | `firefly_openapi::Builder` + `RouteDef` descriptors → `/openapi.json` + Swagger-UI shim |
| `MessageSource.getMessage(...)`          | `bundle.t(locale, key, args)` after `LocaleLayer` resolves Accept-Language           |
| Spring `ServerSentEvent`                 | `firefly_sse::SseWriter` with heartbeat + Last-Event-Id resumption                   |
| `@ControllerAdvice` ProblemDetail handler| `ProblemLayer` + `WebResult<T>` (`?` renders RFC 7807)                               |
| `@SpringBootTest`                        | `tower::ServiceExt::oneshot` against the router + `firefly_testkit::{sign_hmac, SpyBroker, must_encode}` |

## Reactive types

The Java framework uses Project Reactor; async Rust is its most
natural analog — every Reactor operator chain becomes an `async fn`.

| Java                                | Rust                                                     |
|-------------------------------------|----------------------------------------------------------|
| `Mono<T>`                           | `async fn(..) -> FireflyResult<T>`                       |
| `Flux<T>`                           | `impl Stream<Item = T>` (`futures` / `tokio-stream`)     |
| `Mono.error(new ResourceNotFound())`| `Err(FireflyError::not_found("…"))`                      |
| `Mono.deferContextual(…)`           | Task-local read: `firefly_kernel::correlation_id()`      |
| Schedulers / `subscribeOn`          | `tokio::spawn(fut)`                                      |
| `Mono.timeout(...)`                 | `tokio::time::timeout(d, fut)`                           |
| `Flux.onBackpressureBuffer`         | Bounded `tokio::sync::mpsc` channels                     |
| Subscription disposal               | Future drop; `CancellationToken` for cooperative engines |

There is no leading `ctx` parameter as in Go: ambient request metadata
travels in task-local scopes (`with_correlation_id`), and cancellation
is structural — dropping a future cancels it.

## Error handling

| Java                                       | Rust                                                      |
|--------------------------------------------|-----------------------------------------------------------|
| `throw new ResourceNotFoundException(...)` | `return Err(FireflyError::not_found("…"))`                |
| `@ControllerAdvice` ProblemDetail handler  | `firefly_web::ProblemLayer` + `WebResult<T>`              |
| `ErrorEnvelope`                            | `firefly_kernel::ProblemDetail`                           |
| `OperationResult<T>`                       | `FireflyResult<T>` (= `Result<T, FireflyError>`)          |
| `exception.getCause()` chain               | `std::error::Error::source()` chain; `as_problem(&err)` walks it |

## CQRS

```java
// Java
@CommandHandler
public Mono<UserCreated> handle(CreateUser cmd) { … }

bus.send(new CreateUser("alice"))
   .doOnNext(this::publish)
   .subscribe();
```

```rust
// Rust
bus.register(|c: CreateUser| async move {
    Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
});
let out: UserCreated = bus.send(CreateUser { name: "alice".into() }).await?;
```

Validation and caching are lifted to the message — overridable default
methods on the `Message` trait, picked up by `ValidationMiddleware` and
`QueryCache::middleware()`:

```rust
impl Message for CreateUser {
    fn validate(&self) -> Result<(), CqrsError> { /* … */ Ok(()) }       // Java: @Valid
}
impl Message for GetUser {
    fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(30)) } // Java: @Cacheable
}
```

## HTTP middleware

```java
// Java (Spring WebFlux)
@Bean
WebFilter idempotencyFilter() { return new IdempotencyFilter(...); }
```

```rust
// Rust
let core = Core::new(CoreConfig { app_name: "orders".into(), ..CoreConfig::default() });
let app = core.apply_middleware(router);
axum::serve(listener, app).await?;
```

`core.apply_middleware(router)` applies the canonical outermost chain —
panic-recovering ProblemDetail rendering, correlation-id propagation,
and idempotency — as `tower` layers, the Rust analog of Go's single
`core.Middleware()` wrapper.

## Repositories

```java
// Java (R2DBC)
public interface UserRepository extends R2dbcRepository<User, String> { }
```

```rust
// Rust
use firefly_data::{MemoryRepository, Repository};

let repo: MemoryRepository<User, String> =
    MemoryRepository::new(|u: &User| u.id.clone());
```

Services that talk to PostgreSQL define their own typed repository
conforming to `Repository<T, K>`; the `Filter` DSL renders to SQL via
`to_sql`, and transactional participation goes through
`firefly-transactional`'s `TxContext`.

## Sagas

```java
// Java
@Saga
class CheckoutSaga {
    @Step
    void reserveStock(...) { ... }
    @Compensation
    void releaseStock(...) { ... }
}
```

```rust
// Rust
use firefly_orchestration::{CompensationPolicy, Saga, Step};

let saga = Saga::new("checkout")
    .policy(CompensationPolicy::BestEffort)
    .step(Step::new("reserve", || async { Ok(()) }).with_compensation(|| async { Ok(()) }))
    .step(Step::new("charge", || async { Ok(()) }).with_compensation(|| async { Ok(()) }));

let outcome = saga.run().await?;
```

Compensation policy maps directly: `CompensationPolicy::BestEffort`
(default) / `CompensationPolicy::StopOnError`. DAG workflows
(`Workflow` + `Node::depends_on`) and Try-Confirm-Cancel (`Tcc`) follow
the same builder shape — see
[`crates/orchestration/README.md`](../crates/orchestration/README.md).

## Idempotency

```java
// Java
@Idempotent("Idempotency-Key")
@PostMapping("/orders")
Mono<Order> place(@RequestBody PlaceOrder cmd) { … }
```

In Rust, idempotency is a layer applied at the framework boundary —
`core.apply_middleware` installs `IdempotencyLayer` for every
POST/PUT/PATCH that carries an `Idempotency-Key` header. Replays answer
with `Idempotent-Replay: true`; reusing a key with a different body
returns 409.

## Configuration

| Java (Spring Boot YAML key)  | Rust binding                                |
|------------------------------|---------------------------------------------|
| `firefly.app.name`           | `CoreConfig.app_name`                       |
| `firefly.cache.adapter`      | `Arc<dyn cache::Adapter>` injection         |
| `firefly.eda.broker`         | `Arc<dyn eda::Broker>` injection            |
| `firefly.idempotency.ttl`    | `IdempotencyConfig.ttl`                     |
| Profile-specific YAML        | `load_from_profile(dir, app, fallback)` + `FIREFLY_PROFILE` |

`firefly-config` is a full typed loader (YAML + env + flags + profile
selection) — see [CONFIGURATION.md](CONFIGURATION.md) for the binding
rules and the complete Java-key mapping tables.

## PyFly → Rust idioms

The Rust port carries the full PyFly module surface, but where PyFly
relies on the Python runtime (decorators, reflection, `contextvars`,
monkey-patching), the Rust port substitutes an explicit, type-safe
equivalent. Porting a PyFly service, the recurring substitutions are:

| PyFly idiom                                   | Rust idiom                                                       |
|-----------------------------------------------|------------------------------------------------------------------|
| `@decorator` metadata (`@shell_method`, `@before`, `@bean`, `@cacheable`) | A fluent **builder** call — `CommandSpec::new(..).arg(..)`, `register(aspect, pointcut, order)`, `register_factory(scope, f)`, `Message::cache_ttl` override |
| `contextvars` / context-local request state   | **task-local scopes** (`with_correlation_id`, the kernel task-locals) or request `Extension`s; never an ambient global |
| DI autowiring from `__init__` type hints       | `firefly_container` **explicit factory closures** (`resolve`/`resolve_all`/`resolve_named` inside the closure), or plain `Arc<dyn Trait>` construction |
| `@aspect` monkey-patching via `setattr`        | **call-site weaving** — wrap the call in an `Invocation`, route through `intercept` |
| `Protocol` ports + adapter classes             | `#[async_trait]` object-safe traits + adapter crates behind `Arc<dyn Port>` |
| `@auto_configuration` / `@conditional_on_*`    | **explicit wiring** at bootstrap (`Core::new`, `SessionLayer::from_config`, `mount(..)`) — the condition is just an `if` over your config |
| dynamic typing (`Any`, `except Exception`)     | type-erasure where unavoidable (`Arc<dyn Any + Send + Sync>` in AOP, `Box<dyn Error>`); typed everywhere else |
| `asyncio.run` sync/async bridge                | uniformly `async` (`tokio`); handlers return futures |
| `fnmatch` pattern matching                     | `globset` glob matching (same `*` / `?` / `[..]` semantics) |

The wire formats, exit codes, and behavioral contracts stay identical to
PyFly — only the mechanism changes. Each ported crate's README has a
"pyfly parity" section listing its exact substitutions and any
deliberate divergences.

## Message brokers

PyFly (and Spring) bind a listener with a decorator/annotation; the Rust
`firefly-eda` port is constructor-swappable — code against
`Arc<dyn Broker>` and pick the backend at wiring time:

```rust
// In-memory (default, no infrastructure)
let broker = firefly_eda::InMemoryBroker::new();

// Kafka — same Publisher/Subscriber surface
let broker = firefly_eda_kafka::new_kafka_broker(firefly_eda_kafka::KafkaConfig {
    brokers: vec!["localhost:9092".into()],
    consumer_group: "orders-svc".into(),
    ..Default::default()
})?;

// RabbitMQ / Postgres outbox / Redis Streams: same shape, different crate.
broker.subscribe("orders.created", handler(|ev| async move { Ok(()) })).await?;
```

No handler changes are needed to move between transports — the `Event`
JSON envelope is byte-identical across all of them and across the
sibling ports. See [CONFIGURATION.md](CONFIGURATION.md) for each
transport's connection-config surface.

## Porting notes per tier

Mirroring the tier layout in [`MODULES.md`](../MODULES.md):

- **01 Foundational.** Port first; everything depends on it. Swap
  exception hierarchies for `FireflyError` constructors and let `?` +
  `WebResult` replace `@ControllerAdvice`. Validators are pure
  functions — direct translations.
- **02 Platform.** CQRS handlers, sagas, rules, and schedules are
  shape-preserving rewrites (annotation → builder/registration call).
  The one structural change: anything that read Reactor's context now
  reads a task-local or an explicit handle (`TxContext`,
  `CancellationToken`).
- **03 Adapters.** Code against the parent-port trait
  (`firefly_idp::Adapter`, `firefly_ecm::ContentStore`,
  `firefly_notifications::Channel`) and inject the concrete crate at
  wiring time. Vendor adapters that are Stub on the Go side are Stub
  here too — they fail loud with typed not-implemented errors, so a
  port now keeps the call sites stable for the wired release.
- **04 Starters.** Replace `@SpringBootApplication` + starter POMs with
  a `Core::new(CoreConfig)` (or the application / domain / data /
  backoffice variant) in `main`; `core.new_application().run().await`
  replaces `SpringApplication.run` including signal handling and
  graceful drain.
- **05/06 Tests + samples.** `@SpringBootTest` slices become in-process
  `tower::ServiceExt::oneshot` calls against the composed router — no
  sockets; `firefly-testkit` supplies the HMAC signers and the
  `SpyBroker` used to assert published events.
