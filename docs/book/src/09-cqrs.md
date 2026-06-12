# CQRS

`firefly-cqrs` provides the framework's **type-dispatched command/query bus**:
typed handlers registered at startup, dispatched through `Bus::send` /
`Bus::query`, matched by `std::any::TypeId`. On top of that sits pluggable
middleware for validation, query caching, and authorization, plus a reactive
`Mono`-returning surface. This chapter wires a real bus end-to-end.

> **Spring parity** — The bus is the framework's command/query dispatcher.
> `register` ~ a `@CommandHandler`/`@QueryHandler`, `send`/`query` ~ dispatching
> through the gateway, and the middleware chain ~ Spring's handler interceptors.

## Commands, queries, and the `Message` trait

Every command and query implements `Message`. For a plain message that is one
line; the trait's optional methods (`validate`, `cache_ttl`, `authorize`) are
overridable defaults that the matching middleware picks up automatically:

```rust,ignore
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    fn validate(&self) -> Result<(), CqrsError> { Ok(()) }   // ValidationMiddleware
    fn cache_ttl(&self) -> Option<Duration>     { None }     // QueryCache
}
```

`Clone` stands in for pass-by-value handler invocation; `Serialize` seeds the
cache key. A message with no special behaviour is `impl Message for MyCommand {}`.

## A bus, end-to-end

Register typed async handlers, then dispatch. The handler's input type is the
dispatch key; its output is the typed result:

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

`Bus::query` is a readability synonym for `Bus::send`. An unrouted message is a
`CqrsError::NoHandler`.

## The middleware chain

Middleware runs first-registered = outermost. Three ship in the box:

| Middleware                  | Behaviour                                                      |
|-----------------------------|---------------------------------------------------------------|
| `ValidationMiddleware`      | calls `Message::validate` before dispatch, short-circuits on error |
| `QueryCache::middleware()`  | memoises results for messages whose `cache_ttl` is `Some`     |
| `AuthorizationMiddleware`   | calls `Message::authorize(ctx)`, denies before the handler runs |

```text
                              ┌──────────────┐
                              │ msg ↦ TypeId  │
                              └──────────────┘
                                    │
                      registered handlers HashMap<TypeId, _>
                                    │
   middleware chain  ────────────────┘
   ┌───┐ ┌───┐ ┌───┐
   │ V │ │ Q │ │ T │  V = ValidationMiddleware
   └───┘ └───┘ └───┘  Q = QueryCache::middleware  T = your own
```

## Query caching and invalidation

`QueryCache` memoises a query result under a `<type name>:<sha-256 of JSON>`
key. After a mutation, evict precisely:

- `cache.invalidate_type::<GetUser>()` — every cached result for exactly that
  query type;
- `cache.invalidate(prefix)` — every key starting with `prefix`.

For event-driven invalidation, `EdaCacheInvalidationBridge::new(cache)` evicts
entries when domain events arrive on a `firefly-eda` broker:
`register(event_type, "order:{order_id}")` maps an event type to cache-key
patterns whose `{field}` placeholders are resolved from the event payload, and
`subscribe(&broker, topic)` wires it in.

## Authorization

A message can gate itself. Implement `authorize` (default: always authorized),
wire the `AuthorizationMiddleware`, and a denial short-circuits dispatch before
the handler runs:

```rust,ignore
use firefly_cqrs::{AuthorizationMiddleware, Bus};

let bus = Bus::new();
bus.use_middleware(AuthorizationMiddleware::new()); // or ::disabled() to allow all
```

The hook receives the dispatch's `ExecutionContext` when one is attached. A
denial becomes `CqrsError::Authorization(result)`, which maps to a 403.

## ExecutionContext

`ExecutionContext` carries the request's identity — user, tenant, organization,
session, request id, source, client IP, user agent, timestamp, arbitrary
properties, and feature flags. Build one with the fluent builder and attach it
to a dispatch; it reaches `authorize`, any middleware reading
`Envelope::context`, and context-aware handlers:

```rust,ignore
use firefly_cqrs::{Bus, ExecutionContext};

let ctx = ExecutionContext::builder()
    .user("u1")
    .tenant("acme")
    .build();

let result: UserCreated = bus
    .send_with_context(CreateUser { name: "alice".into() }, ctx)
    .await?;
```

## Fluent builders

`CommandBuilder::create(cmd)` / `QueryBuilder::create(q)` accumulate the
identity fields the message carries — a fresh UUID `message_id`, `correlated_by`,
`initiated_by`, a timestamp, free-form metadata, an optional context — and
dispatch via `execute_with(&bus)`. `QueryBuilder` adds cache control:
`cached_for(ttl)` / `uncached()` override `cache_ttl` for the dispatch, and
`with_cache_key(key)` replaces the derived key.

## The reactive bus

The bus exposes a Reactor / WebFlux-style surface that wraps the eventual result
in a lazy `Mono<R>` — the same handler lookup, the same middleware chain, run
only when the `Mono` is subscribed, blocked, or awaited.

| Method                          | Returns       | Reactor analog        |
|---------------------------------|---------------|-----------------------|
| `Bus::send_mono(cmd)`           | `Mono<R>`     | `Mono<R> bus.send(cmd)` |
| `Bus::query_mono(q)`            | `Mono<R>`     | `Mono<R> bus.query(q)`  |
| `Bus::send_mono_with_context`   | `Mono<R>`     | context-carrying send  |
| `Bus::query_mono_with_context`  | `Mono<R>`     | context-carrying query |

The reactive methods take `&Arc<Bus>` (so the lazy `Mono` can own the bus);
register handlers on the `Arc<Bus>` exactly as on a `Bus`:

```rust,ignore
use std::sync::Arc;
use firefly_cqrs::{Bus, CqrsError, Message};

let bus = Arc::new(Bus::new());
bus.register(|c: CreateUser| async move {
    Ok::<_, CqrsError>(UserCreated { id: "u1".into(), name: c.name })
});

// Compose with Reactor operators, then block / subscribe / await.
let id = bus
    .send_mono::<_, UserCreated>(CreateUser { name: "alice".into() })
    .map(|u| u.id)
    .block()
    .await?;            // Ok(Some("u1"))
```

Because `firefly-reactive` fixes its error channel to `FireflyError`, a failed
dispatch is mapped from `CqrsError` into a status-faithful `FireflyError` (a
validation failure → 422, an authorization denial → 403, a missing handler →
500), with the original `CqrsError` preserved as the error's `source()`. So a
reactive command flows straight into the RFC 7807 problem stack while staying
inspectable.

## Listing handlers

`Bus::handler_names()` returns the sorted, fully-qualified type names of every
registered handler — consumed by the admin actuator to show the CQRS map.

The bus dispatches commands and queries within a service. To communicate
*between* services, fan out domain events. Continue to
[Event-Driven Architecture & Messaging](./10-eda-messaging.md).
