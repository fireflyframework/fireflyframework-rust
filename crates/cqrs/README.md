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
bounds.
