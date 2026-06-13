# Appendix: Migrating from Spring Boot

If you are porting a Firefly Java/Spring Boot service to Rust — or simply come
from a Spring background — this appendix is your translation table. Each Firefly
Rust crate is wire-shape compatible with its Java counterpart but expressed in
idiomatic Rust: explicit construction, `tower` middleware composition, and
`async`/`await`.

Everything below is the same surface Lumen — the digital-wallet service this book
builds — exercises. Where a row maps to a concrete Lumen file, that is the place
to see it running.

## Lumen at a glance

The fastest way to read this appendix is against Lumen's macros. Each Lumen
declaration is a Spring annotation in idiomatic Rust:

| Lumen (`samples/lumen`) | Spring / Firefly-Java |
|-------------------------|------------------------|
| `#[rest_controller(path = "/api/v1")]` + `#[get]`/`#[post]` (`web.rs`) | `@RestController` + `@GetMapping`/`@PostMapping` |
| `#[derive(Command)]` / `#[derive(Query)]` + `#[command_handler]` / `#[query_handler]` (`commands.rs`) | `@CommandHandler` / `@QueryHandler` |
| `#[derive(AggregateRoot)]` / `#[derive(DomainEvent)]` (`domain.rs`) | Axon-style `@Aggregate` / `@EventSourcingHandler` |
| `#[event_listener(topic = "wallets.events")]` (`ledger.rs`) | `@KafkaListener` / `@RabbitListener` |
| `Saga::new(...).step(Step::with_compensation(...))` (`transfer.rs`) | `@Saga` + `@SagaOrchestrationStart` / compensation |
| `JwtService` + `BearerLayer` + `FilterChain` (`security.rs`) | `spring-security` resource server + `@PreAuthorize` |
| `#[scheduled(fixed_rate = "60s")]` (`housekeeping.rs`) | `@Scheduled(fixedRate = 60000)` |
| `WebStack::actuator_router(...)` + `InfoContributor` (`main.rs`) | `spring-boot-starter-actuator` + `InfoContributor` |

## Module mapping

| Java / Spring                          | Rust crate                          |
|----------------------------------------|-------------------------------------|
| `firefly-common`                       | `firefly-kernel`                    |
| `firefly-web` + `firefly-spring-utils` | `firefly-web`                       |
| `firefly-common-cache`                 | `firefly-cache`                     |
| `firefly-otel-spring-boot-starter`     | `firefly-observability`             |
| `firefly-common-data`                  | `firefly-data`                      |
| `firefly-common-cqrs`                  | `firefly-cqrs`                      |
| `firefly-common-eda`                   | `firefly-eda`                       |
| `firefly-event-sourcing-…`             | `firefly-eventsourcing`             |
| `firefly-common-domain` (orchestration) | `firefly-orchestration`            |
| `firefly-service-client`               | `firefly-client`                    |
| Spring WebFlux (Netty)                 | `axum` on `tokio`, `tower::Layer`s  |
| Reactor `Mono` / `Flux`                | `firefly_reactive::{Mono, Flux}`    |
| `@ConfigurationProperties`             | `firefly-config`                    |
| `SpringApplication`                    | `firefly-lifecycle`                 |
| `spring-boot-starter-actuator`         | `firefly-actuator`                  |
| `@Scheduled`                           | `firefly-scheduling`                |
| `resilience4j-*`                       | `firefly-resilience`                |
| `spring-security`                      | `firefly-security`                  |
| Flyway                                 | `firefly-migrations`                |
| `springdoc-openapi`                    | `firefly-openapi`                   |
| `MessageSource`                        | `firefly-i18n`                      |
| `ServerSentEvent`                      | `firefly-sse`                       |
| `@Transactional`                       | `firefly-transactional`             |
| `spring-boot-starter-test`             | `firefly-testkit`                   |
| `@Component` / `@Autowired` DI         | `firefly-container` (opt-in)        |
| `@Aspect` / Spring AOP                 | `firefly-aop`                       |
| `HttpSession` / Spring Session         | `firefly-session`                   |
| Spring Shell `@ShellMethod`            | `firefly-shell`                     |
| Spring WebSocket / `@ServerEndpoint`   | `firefly-websocket`                 |
| Spring Boot Admin                      | `firefly-admin`                     |
| `@KafkaListener`                       | `firefly-eda-kafka`                 |
| Spring AMQP (RabbitMQ)                 | `firefly-eda-rabbitmq`              |
| `spring-data-redis` cache              | `firefly-cache-redis`              |
| `JavaMailSender` (SMTP)                | `firefly-notifications-smtp`        |

## Reactive types

The Java framework uses Project Reactor — and so does the Rust port.
`firefly-reactive` is a faithful `Mono` / `Flux` reimplementation, so a Reactor
operator chain maps almost one-to-one (see
[The Reactive Model](./05-reactive-model.md)):

| Project Reactor                  | firefly-reactive                                |
|----------------------------------|-------------------------------------------------|
| `Mono<T>`                        | `Mono<T>`                                        |
| `Flux<T>`                        | `Flux<T>`                                        |
| `Throwable` (error signal)       | `firefly_kernel::FireflyError` (fixed)           |
| `Mono.empty()`                   | `Ok(None)` from a `Mono`                          |
| `Mono.error(new NotFound())`     | `Mono::error(FireflyError::not_found("…"))`      |
| `Schedulers.parallel()`          | `Scheduler::Parallel`                            |
| `subscribeOn` / `publishOn`      | `subscribe_on` / `publish_on`                    |
| `Mono.timeout(d)`                | `Mono::timeout(d)`                               |
| `Flux.onBackpressureBuffer()`    | `Flux::on_backpressure_buffer(n)`                |
| `Retry.backoff(..)`              | `Backoff` + `Mono::retry_backoff`                |
| `Mono.toFuture()` / `await`      | `Mono::into_future` / `.await`                   |
| `Flux.toStream()`                | `Flux::to_stream`                                |

> **Note** — Ambient request metadata that Reactor carries in the subscriber
> context (`deferContextual`) travels in Rust task-local scopes instead —
> `firefly_kernel::correlation_id()`, `request_id()`, `tenant_id()` — set by the
> `CorrelationLayer` HTTP middleware. Cancellation, which Reactor models as
> subscription disposal, is future-drop in Rust (and a `CancellationToken` for
> the cooperative orchestration engines).

## Annotation → macro cheat sheet

The declarative layer ([chapter 21](./21-declarative-macros.md)) maps the Spring
annotation set onto `firefly-macros`. Every macro arrives through
`use firefly::prelude::*;`:

| Spring / pyfly annotation | Firefly-Rust macro | On |
|---------------------------|--------------------|----|
| `@RestController` + `@GetMapping`/`@PostMapping` | `#[rest_controller(path)]` + `#[get]`/`#[post]`/`#[put]`/`#[delete]`/`#[patch]` | an `impl` block |
| `@CommandHandler` / `@QueryHandler` | `#[command_handler]` / `#[query_handler]` | `async fn(Msg) -> Result<R, CqrsError>` |
| message + `@Valid` / `@Cacheable` | `#[derive(Command)]` / `#[derive(Query)]` (`#[firefly(validate)]`, `#[firefly(cache_ttl = "…")]`) | a message struct |
| `@KafkaListener` / `@RabbitListener` | `#[event_listener(topic = "…")]` | `async fn(Event) -> FireflyResult<()>` |
| `@Scheduled(fixedRate = …)` / `(cron = …)` | `#[scheduled(fixed_rate = "…")]` / `#[scheduled(cron = "…")]` | a zero-arg `async fn` |
| Axon `@Aggregate` / event payloads | `#[derive(AggregateRoot)]` / `#[derive(DomainEvent)]` | a struct |
| `@Component` / `@Service` / `@Repository` / `@Configuration` / `@Controller` | `#[derive(Component/Service/Repository/Configuration/Controller)]` | a struct |
| `@Autowired` field/ctor injection | `#[autowired]` on an `Arc<T>` / `Option<Arc<T>>` / `Vec<Arc<T>>` / `Provider<T>` field | a component field |
| `@Bean` factory method | `#[bean]` (+ `#[bean(name/scope/primary/profile)]`) | a method on a `#[derive(Configuration)]` `impl` |
| `@Primary` / `@Order` / `@Qualifier` | `#[firefly(primary)]` / `#[firefly(order = N)]` / `#[firefly(qualifier = "…")]` | a component / field |
| `@Profile` / `@ConditionalOnProperty` / `@ConditionalOnBean` / `@ConditionalOnMissingBean` | `#[firefly(profile = "…")]` / `condition_on_property` / `condition_on_bean` / `condition_on_missing_bean` | a component |
| `@PostConstruct` / `@PreDestroy` | `#[firefly(post_construct = "…")]` / `#[firefly(pre_destroy = "…")]` | a component |
| `@ConfigurationProperties(prefix)` | `#[derive(ConfigProperties)]` `#[firefly(prefix = "…")]` | a `serde::Deserialize` struct |
| `@ComponentScan` / `@EnableAutoConfiguration` | `Container::scan()` / `firefly::scan(&c)` (link-time `inventory`) | the container |
| explicit bean list | `register_all!(&c, [A, B, …])` | — |

See the [DI deep-dive](./04a-dependency-injection.md) for the worked examples.

## Service-tier mapping

| Spring estate | Firefly-Rust starter |
|---------------|----------------------|
| data/core service (owns the DB) | `firefly-starter-core` / `firefly-starter-data` |
| domain service (sagas, CQRS, ES) | `firefly-starter-domain` |
| edge / BFF service (`@Workflow` + `WebClient` aggregation) | `firefly-starter-experience` (`ExperienceStack`, `DomainClients`, `SignalService`) |
| `@WaitForSignal` workflow gate | `Node::wait_for_signal(...)` + `SignalService::deliver(...)` |

See [The Experience Tier (BFF)](./20a-experience-tier.md).

## Programming-model mapping

| Spring concept                                | Rust entry point                                                    |
|-----------------------------------------------|----------------------------------------------------------------------|
| `@SpringBootApplication` auto-config          | `Core::new(CoreConfig { .. })`                                       |
| `SpringApplication.run()`                     | `core.new_application().on_server(..).run().await`                   |
| `@Component` / `@Autowired`                   | explicit `Arc<dyn Trait>` injection; `#[derive(Component)]` + `#[autowired]` for the container |
| `@RestController`                             | `#[rest_controller]` (or an axum handler on `core.apply_middleware(router)`) |
| `@RestController` returning `Mono<T>`         | a handler returning `MonoJson(Mono<T>)`                              |
| `@RestController` returning `Flux<T>` (NDJSON) | a handler returning `NdJson(Flux<T>)` / `Sse(Flux<T>)`              |
| `@ControllerAdvice` ProblemDetail handler     | `ProblemLayer` + `WebResult<T>` (`?` renders RFC 7807)              |
| `@CommandHandler` / `@QueryHandler`           | `bus.register(\|c: Cmd\| async move { .. })`                         |
| `@Cacheable`                                  | `Typed::get_or_set(key, ttl, loader)`                               |
| `@Scheduled(cron = "0 9 * * MON-FRI")`        | `scheduler.cron("name", "0 9 * * 1-5", run)`                        |
| `@CircuitBreaker @RateLimiter @Bulkhead`      | `Chain::new().with(Timeout::..).with(CircuitBreaker::..)`            |
| `@PreAuthorize("hasRole('ADMIN')")`           | `FilterChain::new().require("/admin/", &["ADMIN"])` + `BearerLayer` |
| R2DBC `ReactiveCrudRepository<T, ID>`         | `firefly_data::ReactiveCrudRepository<T, ID>`                       |
| `Pageable` / `Sort` / `PageRequest`           | `firefly_data::{Pageable, Sort, Order}` + `Page<T>`                 |
| Axon / event-sourced aggregates               | `firefly_eventsourcing` (`AggregateRoot`, `EventStore`)             |
| Temporal / Camunda workflows                  | `firefly_orchestration::Workflow` (DAG + compensation)             |
| `@Transactional`                              | `with_tx(ctx, &db, \|ctx\| { .. })`                                 |
| Flyway `V001__init.sql`                       | `firefly_migrations::run` over a `Database` port                   |
| `WebClient`                                   | `firefly_client::WebClientBuilder` → `body_to_mono` / `body_to_flux` |
| `RestTemplate` / `RestClient`                 | `firefly_client::RestBuilder` → `client.request(..)`               |
| `MessageSource.getMessage(...)`               | `bundle.t(locale, key, args)`                                       |
| `@SpringBootTest`                             | `tower::ServiceExt::oneshot` + `firefly_testkit`                   |

## What changes, and what stays the same

**Stays the same** — the wire contracts. The `application/problem+json` shape,
the `Idempotency-Key` semantics, the event envelope JSON, the saga step
definitions, the HMAC webhook signatures, the `Page<T>` JSON, the
`Authentication` claims mapping, and the configuration keys are byte-identical to
the Java release line. A Java service and a Rust service interoperate on the wire
with no adapter.

**Changes** — the wiring style. Spring's reflective DI and annotation scanning
become explicit construction (`Core::new`, constructor injection, optional
`Container`). Annotation-driven aspects become explicit weaving at the call site.
There is no leading `ctx` parameter and no XML — ambient state lives in
task-locals, and dependencies are `Arc<dyn Trait>` fields the compiler checks.

## A worked port

A Spring WebFlux controller:

```java
@RestController
class OrderController {
    @GetMapping("/orders/{id}")
    Mono<Order> get(@PathVariable String id) {
        return service.find(id)
            .switchIfEmpty(Mono.error(new ResourceNotFound("order " + id)));
    }
}
```

becomes a Firefly Rust handler:

```rust,ignore
use axum::extract::Path;
use firefly_reactive::Mono;
use firefly_web::MonoJson;

async fn get_order(Path(id): Path<String>) -> MonoJson<Order> {
    // Mono::empty() (Ok(None)) renders a 404 problem automatically — no
    // switchIfEmpty needed; an Err(FireflyError) renders that error's problem.
    MonoJson(service.find(&id))
}
```

The `Mono<T>` model carries over directly; the empty-Mono → 404 behaviour is
built into the `MonoJson` responder, so the `switchIfEmpty` boilerplate
disappears.

For the full crate catalogue, see the [Module Index](./91-appendix-modules.md).
