# Glossary

Definitions of the terms and types that recur throughout this book, in the
precise sense Firefly uses them.

### Actuator
The management surface (`firefly-actuator`) exposing
`/actuator/{health,info,metrics,env,tasks,version,loggers,httpexchanges,refresh}`.
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

### Backpressure
A slow consumer throttling a fast producer. Firefly's reactive `Flux` streams
(NDJSON/SSE responders, `WebClient::body_to_flux`, Postgres row streams) honour
backpressure end-to-end so a large stream never lands fully in memory.

### Bus
The CQRS command/query dispatcher (`firefly_cqrs::Bus`). Handlers register by
input type; dispatch is keyed on `std::any::TypeId`. `send`/`query` are async;
`send_mono`/`query_mono` are the reactive twins.

### CompensationPolicy
How a saga/workflow rolls back on failure: `BestEffort` (continue compensating
even if one compensation fails) or `StopOnError` (abort at the first
compensation failure).

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

### Problem (RFC 7807)
The `application/problem+json` error envelope (`type`, `title`, `status`,
`detail`) that every Firefly service renders for errors and panics, identical
across the ports.

### Projection
A read-side handler that builds a read model from events
(`firefly_eventsourcing::Projection`), driven per-aggregate (`replay`) or over
the global stream (`drive_once` / `replay_all`).

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

### SSE (Server-Sent Events)
A one-way streaming protocol (`text/event-stream`). The `Sse(Flux<T>)` responder
and `firefly-sse`'s `SseWriter` emit it; `WebClient::body_to_flux` decodes it.

### Specification
A composable business-rule predicate (`firefly_kernel::ddd::Specification<T>`)
combined with `.and()`, `.or()`, `.not()`. Any `Fn(&T) -> bool` is one.

### Starter
A crate that bundles a sensible default stack so a service depends on one crate.
`firefly-starter-core` is the common starting point.

### TCC (Try-Confirm-Cancel)
A two-phase distributed-transaction engine (`firefly_orchestration::Tcc`): Try
all participants, Confirm all on success, Cancel the tried participants on any
Try failure.

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
compensation under a configurable `CompensationPolicy`.
