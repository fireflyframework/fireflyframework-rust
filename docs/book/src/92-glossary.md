# Glossary

Definitions of the terms and types that recur throughout this book, in the
precise sense Firefly uses them.

### Actuator
The management surface (`firefly-actuator`) exposing the endpoints
`/actuator/health`, `/actuator/info`, `/actuator/metrics`, `/actuator/env`,
`/actuator/tasks`, `/actuator/version`, `/actuator/loggers`,
`/actuator/httpexchanges`, and `/actuator/refresh`.
Mounted by `core.actuator_router(..)`, typically on a separate, firewalled port.

### Adapter
A concrete implementation of a **port**. Selected at wiring time as an
`Arc<dyn Port>` so heavy SDK dependencies stay out of services that do not use
them. Examples: `RedisAdapter` (a `cache::Adapter`), `KafkaBroker` (an
`eda::Broker`).

### Aggregate root
The consistency boundary in DDD. The non-event-sourced variant holds a
`PendingEvents<E>` buffer (`firefly_kernel::ddd`); the event-sourced variant is
an `AggregateRoot` whose state is rebuilt by replaying its `DomainEvent`s
(`firefly-eventsourcing`).

### Autowired
A component field the DI container resolves and injects by type
(`#[autowired]`, `firefly-container`). The field type sets the shape: `Arc<T>`
(required), `Option<Arc<T>>` (optional), `Vec<Arc<T>>` (all implementations),
`Provider<T>` (deferred). The Rust analog of Spring's `@Autowired`.

### Backpressure
A slow consumer throttling a fast producer. Firefly's reactive `Flux` streams
(NDJSON/SSE responders, `WebClient::body_to_flux`, Postgres row streams) honour
backpressure end-to-end so a large stream never lands fully in memory.

### Bean
Any value the DI `Container` builds, wires, and owns. Declared with a
**stereotype** derive (`#[derive(Component/Service/Repository/Configuration/
Controller)]`) or produced by a `#[bean]` factory method on a
`#[derive(Configuration)]` holder. Keyed by `TypeId`; resolvable with
`resolve::<T>()`.

### BFF (Backend-for-Frontend)
See **Experience tier**.

### Bus
The CQRS command/query dispatcher (`firefly_cqrs::Bus`). Handlers register by
input type; dispatch is keyed on `std::any::TypeId`. `send`/`query` are async;
`send_mono`/`query_mono` are the reactive twins.

### Compensation
The undo step a **saga** runs in reverse order when a later step fails
(`Step::with_compensation`). In Lumen's transfer saga, the debit's compensation
is a refund deposit on the source wallet.

### CompensationPolicy
How a saga/workflow rolls back on failure: `BestEffort` (continue compensating
even if one compensation fails) or `StopOnError` (abort at the first
compensation failure).

### Component scanning
Link-time bean discovery (`Container::scan()` / `firefly::scan`): every
non-generic stereotype derive submits an `inventory` thunk, and `scan` collects
them across the crate graph, applies **conditions** and **profiles**, and
registers the survivors. The Rust analog of Spring's `@ComponentScan` (link-time,
not reflective). Generic beans use the `register_all!` fallback.

### Conditional bean
A bean registered only when the environment matches — `#[firefly(profile = "…",
condition_on_property = "k=v", condition_on_bean = "T",
condition_on_missing_bean = "T", condition_on_class = "label",
condition_on_single_candidate = "T")]`. Evaluated by `scan` in two passes
(config/profile facts first, registry-dependent checks second). Spring's
`@Profile` / `@ConditionalOn*`.

### Container
The opt-in, `TypeId`-keyed DI service locator (`firefly-container`): registers
beans, resolves by type or name, supports scopes, trait-object bindings,
`primary`/`order` disambiguation, deferred `Provider<T>`, lifecycle hooks, and
component scanning. Distinct from **Core** (the wired infrastructure bundle).

### Core
The wired infrastructure bundle returned by `Core::new(CoreConfig)`
(`firefly-starter-core`): cache, CQRS bus, event broker, health composite,
metrics, scheduler, logging, and the middleware chain.

### Correlation id
A per-request identifier carried in the `X-Correlation-Id` header and a kernel
task-local scope. It auto-enriches every log line, every published event, and
every outbound client call so a request stitches together across services.

### DomainEvent
The event-sourced, versioned, wire-formatted event in `firefly-eventsourcing`
(distinct from the transient `TransientDomainEvent` in `firefly_kernel::ddd`).
Its JSON is byte-compatible across the ports.

### Event (EDA)
The envelope every `firefly-eda` event flows through — `id`, `type`, `source`,
`topic`, `correlationId`, `time`, `headers`, `payload`, `key`. Constructed with
`Event::new`, wire-compatible across the ports.

### Experience tier (BFF)
The top service tier (`firefly-starter-experience`, `ExperienceStack` / `Bff`): a
Backend-for-Frontend that composes several **domain** SDKs into journey-specific,
atomic REST endpoints. It owns no database and calls only domain services
(`channel → experience → domain → core`). Built from `DomainClients` (the
`ClientFactory`), `SignalService` gates, Redis-capable `WorkflowState`, and
`WorkflowQueryService`.

### FireflyError
The framework's error type (`firefly_kernel::FireflyError`). It renders as an
RFC 7807 `application/problem+json` response, and it is the fixed error channel
of the reactive `Mono`/`Flux` (their terminal `Err` signal).

### FilterChain
The path-based authorization matcher in `firefly-security` (`permit` / `require`
/ glob `permit_pattern` / `require_pattern`). Fail-closed once any rule is
declared (Spring Security 6 deny-by-default).

### Flux
A reactive publisher of *0..N* values plus a terminal completion-or-error
(`firefly_reactive::Flux`). The Rust analog of Reactor's `Flux<T>`.

### Idempotency
The replay behaviour applied to `POST`/`PUT`/`PATCH` requests carrying an
`Idempotency-Key` header. A repeat replays the stored response
(`Idempotent-Replay: true`); reuse with a different body is a 409.

### Mono
A reactive publisher of *at most one* value plus a terminal error
(`firefly_reactive::Mono`). The Rust analog of Reactor's `Mono<T>`. An empty
`Mono` (`Ok(None)`) is the equivalent of `Mono.empty()`.

### NDJSON
Newline-delimited JSON (`application/x-ndjson`) — one compact JSON document per
line. The `NdJson(Flux<T>)` responder streams it with backpressure.

### Outbox (transactional)
A pattern (`TransactionalOutbox`, `firefly-eda-postgres`) that writes events in
the same transaction as the state change and delivers them to consumers
afterward, giving at-least-once delivery without a separate broker.

### Port
An object-safe `async_trait` trait defining an integration point —
`cache::Adapter`, `eda::Broker`, `notifications::Channel`, `idp::Adapter`. Code
depends on the port; an **adapter** implements it.

### Primary
The disambiguator (`#[firefly(primary)]`) that picks one bean when several
implementations are bound to the same port. Resolving with no primary among
multiple candidates is a `NoUniqueBean` error naming every candidate. Spring's
`@Primary`.

### Problem (RFC 7807 / 9457)
The `application/problem+json` error envelope (`type`, `title`, `status`,
`detail`) that every Firefly service renders for errors and panics, identical
across the ports. RFC 9457 obsoletes and is wire-compatible with RFC 7807; the
book uses both numbers interchangeably.

### Profile
A named environment (`prod`, `dev`, `test`) that gates conditional beans
(`#[firefly(profile = "expr")]`). The expression grammar supports `&`, `|`, `!`,
comma-as-OR, and parentheses (Spring Boot 2.4+). Active profiles live on the
`ApplicationContext` / `ConditionContext`.

### Projection
A read-side handler that builds a read model from events
(`firefly_eventsourcing::Projection`), driven per-aggregate (`replay`) or over
the global stream (`drive_once` / `replay_all`).

### Qualifier
A name used to select a specific bean when several share a type
(`#[firefly(qualifier = "replica")]` → `resolve_named`). Spring's
`@Qualifier`.

### Reactive
The `Mono`/`Flux` programming model (`firefly-reactive`) and everything built on
it — reactive endpoints, repositories, the `WebClient`, reactive EDA/CQRS. The
Rust analog of Project Reactor / Spring WebFlux.

### Saga
A sequential distributed-transaction engine (`firefly_orchestration::Saga`) with
reverse-order compensation on failure. See also `Workflow` (DAG) and `Tcc`.

### Scheduler
The task runner (`firefly_scheduling::Scheduler`) owning Cron, FixedRate, and
FixedDelay triggers, each on its own tokio task with panic recovery.

### Scope
A bean's lifecycle (`#[firefly(scope = "…")]`): `singleton` (one cached
instance, the default), `transient` (fresh per resolve), `request`, or `session`
(both driven by a `ScopeHandler`). Spring's `singleton` / `prototype` (Firefly:
`transient`) / `request` / `session`.

### Signal
An external event that satisfies a parked workflow gate in the **experience
tier** (`SignalService::deliver` / `Node::wait_for_signal`). Delivery is
buffered, so a signal that arrives before the gate parks is not lost. Spring's
`@WaitForSignal`.

### SSE (Server-Sent Events)
A one-way streaming protocol (`text/event-stream`). The `Sse(Flux<T>)` responder
and `firefly-sse`'s `SseWriter` emit it; `WebClient::body_to_flux` decodes it.

### Specification
A composable business-rule predicate (`firefly_kernel::ddd::Specification<T>`)
combined with `.and()`, `.or()`, `.not()`. Any `Fn(&T) -> bool` is one.

### Starter
A crate that bundles a sensible default stack so a service depends on one crate.
`firefly-starter-core` is the common starting point; `firefly-starter-domain`
and `firefly-starter-experience` add the domain and BFF tiers.

### Stereotype
The architectural-role label a DI bean carries (`component`, `service`,
`repository`, `configuration`, `controller`, `bean`), set by which derive
declared it. Functionally equivalent; the differences are the documented intent
and the grouping shown in the admin `/beans` view. Spring's `@Component` family.

### TCC (Try-Confirm-Cancel)
A two-phase distributed-transaction engine (`firefly_orchestration::Tcc`): Try
all participants, Confirm all on success, Cancel the tried participants on any
Try failure.

### Value object
A domain type defined entirely by its attributes (no identity) and **immutable**:
every operation returns a new value. Lumen's `Money` is the canonical example —
exact integer-cent arithmetic, closed under `add`/`subtract`. The DDD counterpart
of an **aggregate root** (which has identity).

### Verifier
The async port (`firefly_security::Verifier`) that validates a bearer token and
returns an `Authentication`. `JwksVerifier`, IDP adapters, and `VerifierFn`
closures all satisfy it.

### WebClient
The reactive HTTP client (`firefly_client::WebClient`) whose terminal operators
return `Mono`/`Flux` (`body_to_mono`, `body_to_flux`, `exchange`). The Rust
analog of WebFlux's `WebClient`.

### Workflow
A DAG distributed-transaction engine (`firefly_orchestration::Workflow`):
independent nodes run concurrently within a wave, with reverse-order
compensation under a configurable `CompensationPolicy`. In the **experience
tier**, a node may park on a **signal** gate.

### WorkflowState
Redis-capable persisted journey state in the **experience tier**
(`firefly_starter_experience::WorkflowState`): round-trips a workflow run's
`StepContext` snapshot through the cache `Adapter`, keyed by correlation id, so a
parked journey survives a client disconnect (`save` / `load` / `delete`).
