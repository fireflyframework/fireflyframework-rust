# `firefly-cqrs`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-common-cqrs` · **Go module:** `cqrs` · **.NET project:** `FireflyFramework.Cqrs`

## Overview

`firefly-cqrs` provides the framework's **type-dispatched command/query
bus** with generics, plus pluggable middleware for validation, query
caching, and any custom cross-cutting concern. Service authors register
typed handlers at startup and dispatch through `Bus::send` /
`Bus::query`; the bus matches by `std::any::TypeId`.

```rust,ignore
bus.register(|c: CreateUser| async move {
    Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
});

let out: UserCreated = bus.send(CreateUser { name: "alice".into() }).await?;
```

## Why generics + `TypeId`?

The Java firefly-common-cqrs module dispatches by class; the .NET port
dispatches by type; the Go port keys a registry by `reflect.Type` behind
a generic facade. Rust gets the same single dispatch path with zero
casts in user code: `register` and `send` are fully typed, and only the
internal registry — `HashMap<TypeId, DynHandler>` — is type-erased.

## Public surface

| Symbol                              | Purpose                                                          | Go equivalent              |
|-------------------------------------|------------------------------------------------------------------|----------------------------|
| `Bus::new()`                        | Empty bus                                                        | `New() *Bus`               |
| `Bus::register(handler)`            | Install async handler for messages of type `C` returning `R`     | `Register[C, R](bus, h)`   |
| `Bus::send(cmd)`                    | Dispatch, returns typed result                                   | `Send[C, R](ctx, bus, cmd)`|
| `Bus::query(q)`                     | Synonym for `send` (readability)                                 | `Query[Q, R](ctx, bus, q)` |
| `Bus::use_middleware(mw)`           | Append middleware (run-order: first-registered = outermost)      | `Bus.Use(mw...)`           |
| `CqrsError::NoHandler`              | Error variant for unrouted messages                              | `ErrNoHandler`             |
| `Message`                           | Trait every command/query implements (one line for plain types)  | implicit `any`             |
| `Envelope`, `AnyResult`, `DynHandler`, `HandlerFuture` | Type-erased dispatch shapes for custom middleware | `anyHandler`            |

### Middleware

| Symbol                              | Purpose                                                                              |
|-------------------------------------|---------------------------------------------------------------------------------------|
| `ValidationMiddleware`              | Calls `Message::validate` before dispatch and short-circuits on error                 |
| `QueryCache::middleware()`          | Memoises results for messages whose `Message::cache_ttl` returns `Some`               |
| `QueryCache::invalidate(prefix)`    | Removes every entry whose key starts with `prefix` (`<type name>:<sha-256 of JSON>`)  |
| `QueryCache::invalidate_type::<Q>()`| Typed convenience: invalidates every cached result for exactly query type `Q` (matches the `<type name>:` prefix, so name-prefix siblings stay cached) |

### Optional capabilities

Go discovers `Validatable` / `Cacheable` through runtime interface
queries. Rust has no equivalent, so they become overridable default
methods on the `Message` trait — the corresponding middleware picks them
up automatically:

```rust,ignore
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    fn validate(&self) -> Result<(), CqrsError> { Ok(()) }   // Go: Validatable
    fn cache_ttl(&self) -> Option<Duration>     { None }     // Go: Cacheable
}
```

The `Serialize` supertrait mirrors Go, where `json.Marshal` works on any
struct (it seeds the cache key); `Clone` stands in for Go's
pass-by-value handler invocation. A plain message is one line:
`impl Message for MyCommand {}`.

## Mental model

```
                              ┌──────────────┐
                              │ msg ↦ TypeId  │
                              └──────────────┘
                                    │
                      registered handlers HashMap<TypeId, _>
                                    │
   middleware chain  ────────────────┘
   (use_middleware)
   ┌───┐ ┌───┐ ┌───┐
   │ V │ │ Q │ │ T │  V = ValidationMiddleware
   └───┘ └───┘ └───┘  Q = QueryCache::middleware
                       T = your tracing/auth/etc.
```

## Quick start

```rust
use std::time::Duration;
use firefly_cqrs::{Bus, CqrsError, Message, QueryCache, ValidationMiddleware};
use serde::Serialize;

#[derive(Clone, Serialize)]
struct CreateUser { name: String }

impl Message for CreateUser {
    fn validate(&self) -> Result<(), CqrsError> {
        if self.name.is_empty() {
            return Err(CqrsError::validation("name required"));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize)]
struct GetUser { id: String }

impl Message for GetUser {
    fn cache_ttl(&self) -> Option<Duration> { Some(Duration::from_secs(60)) }
}

#[derive(Clone, Debug)]
struct UserCreated { id: String, name: String }

#[tokio::main]
async fn main() {
    let bus = Bus::new();
    bus.use_middleware(ValidationMiddleware::new());
    let cache = QueryCache::new();
    bus.use_middleware(cache.middleware());

    bus.register(|c: CreateUser| async move {
        Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
    });
    bus.register(|q: GetUser| async move {
        Ok::<_, CqrsError>(UserCreated { id: q.id, name: "alice".into() })
    });

    let created: UserCreated = bus.send(CreateUser { name: "alice".into() }).await.unwrap();
    let view: UserCreated = bus.query(GetUser { id: created.id }).await.unwrap();
    assert_eq!(view.name, "alice");

    cache.invalidate_type::<GetUser>(); // after a mutation
}
```

## pyfly parity

On top of the Go-parity surface above, the crate ports pyfly's CQRS layer
(`pyfly.cqrs.{authorization,context,cache.eda_bridge,fluent}` and the
`HandlerRegistry` listing). Every Python idiom is adapted to a Rust one —
decorators and kwargs-reflection become builders/closures, `contextvars`
become an explicitly-threaded value, and `AuthorizationException` becomes
a `CqrsError` variant — while preserving behaviour and wire strings.

### Authorization

| Symbol                                   | pyfly equivalent                                  |
|------------------------------------------|---------------------------------------------------|
| `Message::authorize(ctx)`                | the message's `authorize()` / `authorize_with_context(ctx)` hook (default = always authorized, same pattern as `validate`) |
| `AuthorizationMiddleware` (`new` / `disabled` / `with_enabled`) | `AuthorizationService(enabled=…)` wired into the bus |
| `AuthorizationResult` (`success` / `failure` / `failure_with` / `combine` / `error_messages`) | frozen `AuthorizationResult` dataclass |
| `AuthorizationError` + `AuthorizationSeverity` (`WARNING`/`ERROR`/`CRITICAL`) | the matching frozen dataclass / `StrEnum` (wire strings preserved) |
| `CqrsError::Authorization(result)` + `is_authorization` / `authorization_result` | `AuthorizationException` raised on denial |

A denial short-circuits dispatch before the handler runs; a disabled
middleware authorizes everything. The hook receives the dispatch's
`ExecutionContext` when one is attached, and `None` otherwise.

### Structured validation

On top of the terse `Message::validate` hook (`Result<(), CqrsError>`),
the crate ports pyfly's `pyfly.cqrs.validation` result types for messages
that need to accumulate **multiple** field errors:

| Symbol                                   | pyfly equivalent                                  |
|------------------------------------------|---------------------------------------------------|
| `ValidationResult` (`success` / `failure` / `failure_with` / `from_errors` / `combine` / `error_messages` / `into_cqrs_error`) | frozen `ValidationResult` dataclass |
| `ValidationError` (`new` + `with_error_code` / `with_severity` / `with_rejected_value`) | frozen `ValidationError` dataclass |
| `ValidationSeverity` (`WARNING` / `ERROR` / `CRITICAL`) + `VALIDATION_ERROR_CODE` | `ValidationSeverity` `StrEnum` + default `"VALIDATION_ERROR"` code |
| `StructuredValidate::validate_structured()` | the `obj.validate()` returning a `ValidationResult` that `AutoValidationProcessor` discovers |

This surface is **additive and entirely opt-in** — it does not change the
`Bus`, the `Message` trait's required shape, or the `ValidationMiddleware`.
A message opts in by implementing `StructuredValidate` and folding the
result back into the existing channel:

```rust,ignore
impl StructuredValidate for CreateUser {
    fn validate_structured(&self) -> ValidationResult {
        let mut r = ValidationResult::success();
        if self.name.is_empty() {
            r = r.combine(ValidationResult::failure("name", "name is required"));
        }
        r
    }
}

impl Message for CreateUser {
    // Bridge the structured result into the unchanged ValidationMiddleware.
    fn validate(&self) -> Result<(), CqrsError> {
        self.validate_structured().into_cqrs_error()
    }
}
```

`ValidationResult::into_cqrs_error()` renders the failure summary exactly
like pyfly's `CqrsValidationException` (explicit summary → joined
`"<field>: <message>"` messages → `"Validation failed"`), so the existing
`CqrsError::Validation` short-circuit and wire string are preserved. The
`ValidationSeverity` and `ValidationError` JSON shapes match pyfly's
`StrEnum` / dataclass field names byte-for-byte.

### ExecutionContext

`ExecutionContext` (user / tenant / organization / session / request /
source / client IP / user agent / `created_at` / arbitrary properties /
feature flags) is the Rust spelling of pyfly's `DefaultExecutionContext`,
built via the fluent `ExecutionContext::builder()`. Attach one with
`Bus::send_with_context` / `Bus::query_with_context` (or a builder's
`with_context`); it reaches `Message::authorize`, any middleware reading
`Envelope::context`, and handlers registered via
`Bus::register_with_context` (pyfly's context-aware `do_handle(cmd, ctx)`).

### Fluent builders

`CommandBuilder::create(cmd)` / `QueryBuilder::create(q)` accumulate the
identity fields pyfly's `Command`/`Query` base classes carry — a fresh
UUID `message_id`, `correlated_by`, `initiated_by`, `at` (timestamp),
free-form `with_metadata`, an optional `with_context` — and dispatch via
`execute_with(&bus)`. `QueryBuilder` adds cache control: `cached_for(ttl)`
/ `uncached()` override `Message::cache_ttl` for the dispatch, and
`with_cache_key(key)` replaces the derived `<type>:<sha-256>` key (pyfly's
`get_cache_key()` override). Field mutation uses a typed `with(|m| …)`
closure in place of Python's reflective `with_field`.

### EDA cache-invalidation bridge

`EdaCacheInvalidationBridge::new(cache)` evicts `QueryCache` entries when
domain events arrive on a `firefly-eda` broker (pyfly's
`EdaCacheInvalidationBridge`). `register(event_type, "order:{order_id}")`
maps an event type to cache-key patterns whose `{field}` placeholders are
resolved from the event's JSON payload; `subscribe(&broker, topic)` wires
it in (call once per topic — the Rust `Subscriber` port is per-topic where
pyfly subscribes a `"*"` wildcard). Explicit `CacheInvalidationEvent`s on
the dedicated `CACHE_INVALIDATION_TOPIC` evict their prefixes directly
with no rule registration.

### Admin listing

`Bus::handler_names()` returns the sorted, fully-qualified type names of
every registered handler — pyfly's `HandlerRegistry.get_registered_*_types()`,
consumed later by the admin actuator.

### Domain-event publishing

A command surfaces the events it produced by overriding
`Message::domain_events()` (pyfly's `command.domain_events`). Install a
`DomainEventMiddleware` built from a `CommandEventPublisher` and, after a
successful dispatch, the middleware publishes each event:

```rust
let publisher = Arc::new(EdaCommandEventPublisher::new(broker)); // over firefly-eda
bus.use_middleware(
    DomainEventMiddleware::new(publisher).with_destination("orders.events"),
);
```

`EdaCommandEventPublisher` adapts each `DomainEvent` (an `event_type` + JSON
payload) to a canonical `firefly_eda::Event` and publishes it to the resolved
topic (default `cqrs.events`); `NoOpEventPublisher` silently drops events when
no EDA integration is wired. `EventFailureStrategy::{Log, Raise}` controls
whether a publish failure is logged (the command still succeeds) or surfaced
as a `CqrsError::EventPublish`. Result-side events (pyfly's
`result.domain_events`) are published via `Bus::send_publishing`, which runs
the full middleware chain and then publishes the events a result type exposes
through the `DomainEvents` trait. This ports pyfly's `@publish_domain_event` +
`DefaultCommandBus._try_publish_events`.

### Metrics

`CqrsMetrics::new(registry)` registers the pyfly metric family
(`firefly_cqrs_command_processed` / `_failed` / `_validation_failed` /
`_processing_time_seconds` and the query equivalents) on a
`firefly_observability::MetricsRegistry`. Install a `MetricsMiddleware` to
time and count every dispatch automatically (`MetricsMiddleware::for_queries`
on a query-only bus); a `CqrsError::Validation` failure also bumps the
validation-failed counter. This ports pyfly's `CqrsMetricsService`.

### Health

`CqrsHealthIndicator::new(bus)` is a `firefly_observability::Indicator`
reporting `UP` (with a `handlers` count detail) when the bus has at least one
registered handler, else `UNKNOWN` — pyfly's `CqrsHealthIndicator`. Register
it with the framework health composite so CQRS contributes a `cqrs` component
to `/actuator/health`.

## Reactive

Alongside the async `Bus::send` / `Bus::query`, the bus exposes a
**Reactor / WebFlux-style** reactive surface built on
[`firefly-reactive`](../reactive/README.md). It is **strictly additive**:
the existing async API, the registry, the middleware chain, and every
wire format are unchanged — the reactive methods just wrap the eventual
result in a lazy [`Mono<R>`](../reactive/README.md), exactly as a Spring
WebFlux reactive command bus hands back a `Mono<R>` instead of a blocking
`R`.

| Symbol                                | Purpose                                                      | Reactor / WebFlux analog        |
|---------------------------------------|--------------------------------------------------------------|---------------------------------|
| `Bus::send_mono(cmd) -> Mono<R>`      | Reactive twin of `Bus::send` (same lookup + middleware)      | `Mono<R> bus.send(cmd)`         |
| `Bus::query_mono(q) -> Mono<R>`       | Reactive twin of `Bus::query`                                | `Mono<R> bus.query(q)`          |
| `Bus::send_mono_with_context(cmd, ctx)` | `send_mono` with an `ExecutionContext` attached            | context-carrying reactive send  |
| `Bus::query_mono_with_context(q, ctx)`  | `query_mono` with an `ExecutionContext` attached           | context-carrying reactive query |
| `cqrs_error_to_firefly(err)`          | Maps a `CqrsError` into the reactive stack's `FireflyError`  | `CqrsError` → `Throwable`       |

The reactive methods take `&Arc<Bus>` (so the lazy `Mono` can own the
bus); register handlers on the `Arc<Bus>` exactly as on a `Bus`. Nothing
runs until the `Mono` is subscribed, blocked, or awaited, at which point
it executes the *same* handler lookup and the *same* validation /
authorization / caching middleware chain as `Bus::send`:

```rust,ignore
use std::sync::Arc;
use firefly_cqrs::{Bus, CqrsError, Message};

let bus = Arc::new(Bus::new());
bus.register(|c: CreateUser| async move {
    Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
});

// Compose with Reactor operators, then block/subscribe/await.
let id = bus
    .send_mono::<_, UserCreated>(CreateUser { name: "alice".into() })
    .map(|u| u.id)
    .block()
    .await?;            // Ok(Some("u1"))
```

Because `firefly-reactive` fixes its error channel to
`firefly_kernel::FireflyError` (WebFlux models everything as a
`Throwable`), a failed dispatch is mapped from `CqrsError` into a
status-faithful `FireflyError` via `cqrs_error_to_firefly` — a validation
failure → 422, an authorization denial → 403, a missing handler / type
mismatch / domain error → 500 — with the original `CqrsError` preserved
as the error's `source()` cause, so it flows straight into the RFC 7807
problem stack while staying inspectable.

## Testing

```bash
cargo test -p firefly-cqrs
```

Covers the full Go suite — happy-path dispatch, `NoHandler` for unrouted
messages, validation short-circuit, query-cache hit-rate (loader runs
once), and prefix-keyed invalidation — plus Rust-specific cases:
middleware registration order, TTL expiry, zero-TTL (cache forever),
per-value cache keys, error responses never cached, handler overwrite,
result-type-mismatch diagnostics, concurrent dispatch, and `Send + Sync`
bounds. The `pyfly_parity_test` suite ports pyfly's
`test_authorization.py`, `test_context.py`,
`test_eda_cache_invalidation.py`, and `test_fluent_builders.py` (plus
`HandlerRegistry` listing and context threading) end-to-end against an
in-memory EDA broker. The `reactive_test` suite covers the
`send_mono` / `query_mono` happy path, operator composition, the caching
middleware running through a `Mono`, the `CqrsError` → `FireflyError`
status mapping (validation → 422, authorization → 403, handler → 500),
the no-handler path, and the `*_with_context` overloads.
