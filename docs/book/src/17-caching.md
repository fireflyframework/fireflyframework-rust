# Caching

`firefly-cache` exposes a single port — `Adapter` — and ships in-process,
no-op, and fallback implementations plus a typed memoisation wrapper. Code
against the port and select the backend (in-memory, Redis, Postgres) at wiring
time. This chapter also covers `firefly-resilience`, the decorators that protect
the calls a cache miss falls back to.

> **Spring parity** — `Adapter` is the `Cache` abstraction;
> `Typed::get_or_set` is `@Cacheable`; the resilience decorators are
> Resilience4j (circuit breaker, rate limiter, bulkhead, timeout) composed for
> reactive Rust.

## The cache port

Every consumer — CQRS query cache, session store, OAuth2 token store — depends
on `Arc<dyn cache::Adapter>`, never on a concrete client. The port:

```rust,ignore
#[async_trait]
pub trait Adapter: Send + Sync {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError>;
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError>;
    async fn delete(&self, key: &str) -> Result<(), CacheError>;
    async fn clear(&self) -> Result<(), CacheError>;
    async fn set_if_absent(&self, key, value, ttl) -> Result<bool, CacheError>;
    async fn exists(&self, key: &str) -> Result<bool, CacheError>;
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError>;
    async fn health_check(&self) -> Result<(), CacheError>;
}
```

A miss is `CacheError::NotFound`; `ttl: None` (or zero) means no expiry.

| Implementation                          | Backing                    | Use                                          |
|-----------------------------------------|----------------------------|----------------------------------------------|
| `MemoryAdapter`                         | `HashMap` + `RwLock`       | in-process, TTL-aware, the default           |
| `NoOpAdapter`                           | none                       | tests / disabled-cache configs               |
| `FallbackAdapter { primary, secondary }` | composite                 | primary-then-secondary, writes to both       |
| `RedisAdapter` (`firefly-cache-redis`)  | Redis (RESP)               | distributed cache                            |

## Typed memoisation

The `Typed<T>` wrapper serializes values as `serde_json` bytes (wire-compatible
with the other ports) and gives you `get_or_set` — the memoisation primitive
that consults the cache, calls the loader on a miss, persists, and returns the
value. A caching error never masks a successful loader result:

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly_cache::{MemoryAdapter, Typed};

#[derive(serde::Serialize, serde::Deserialize)]
struct Order { id: String }

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly_cache::CacheError> {
    let cache = Arc::new(MemoryAdapter::new());
    let typed: Typed<Order> = Typed::new(cache);

    let order = typed
        .get_or_set("order:42", Some(Duration::from_secs(60)), || async {
            // loaded from the repository on a miss
            Ok(Order { id: "42".into() })
        })
        .await?;
    assert_eq!(order.id, "42");
    Ok(())
}
```

## Choosing and composing backends

`Core::new` uses `MemoryAdapter` by default; pass an
`Arc<dyn cache::Adapter>` in `CoreConfig.cache` to swap it. For high
availability, compose Redis with an in-process fallback so a Redis blip degrades
to local caching instead of failing:

```rust,ignore
use std::sync::Arc;
use firefly_cache::{FallbackAdapter, MemoryAdapter};

// Tries Redis first; on a transport error or miss, falls through to memory
// and writes to both.
let cache = FallbackAdapter::new(redis_adapter, Arc::new(MemoryAdapter::new()));
```

Because everything downstream depends on the port, swapping the backend changes
one constructor — your handlers, the CQRS query cache, and the session store are
untouched.

## Resilience decorators

`firefly-resilience` guards the calls a cache miss falls through to (and any
outbound call). Four primitives, composable into a `Chain`:

| Decorator        | Guards against                              | Error on trip                  |
|------------------|---------------------------------------------|--------------------------------|
| `CircuitBreaker` | cascading failure of a slow / failing dep   | `ResilienceError::CircuitOpen` |
| `RateLimiter`    | outbound rate overrun (token bucket)        | `ResilienceError::RateLimited` |
| `Bulkhead`       | resource exhaustion from runaway concurrency | `ResilienceError::BulkheadFull` |
| `Timeout`        | stuck calls                                 | `ResilienceError::Timeout`     |

A `Chain` composes them into a single guarded call:

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly_resilience::{Bulkhead, Chain, CircuitBreaker, CircuitConfig, Timeout};

# async fn ex() -> Result<(), firefly_resilience::ResilienceError> {
let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));

let guarded = Chain::new()
    .with(Timeout::new(Duration::from_secs(2)))   // per-call deadline (outermost)
    .with(breaker)                                 // open the circuit on repeated failures
    .with(Bulkhead::new(20));                      // cap concurrent in-flight calls

guarded.execute(|| async {
    // the protected operation — a cache loader, an upstream call, ...
    Ok(())
}).await?;
# Ok(())
# }
```

Each decorator also stands alone:

```rust,ignore
use std::time::Duration;
use firefly_resilience::{Bulkhead, CircuitBreaker, CircuitConfig, RateLimiter, Timeout};

let cb = CircuitBreaker::new(CircuitConfig::default());
let _ = cb.execute(|| async { charge().await }).await;

let rl = RateLimiter::new(100.0, 200);        // 100 rps, burst 200
let _ = rl.execute(|| async { call().await }).await;

let bh = Bulkhead::new(20);
let _ = bh.try_execute(|| async { call().await }).await; // non-blocking; BulkheadFull if full

let to = Timeout::new(Duration::from_secs(2));
let _ = to.execute(|| async { slow_call().await }).await;
```

## A complete read path

The pieces fit together as a cache-aside read protected by a circuit breaker:

```rust,ignore
use std::sync::Arc;
use std::time::Duration;
use firefly_cache::{MemoryAdapter, Typed};
use firefly_resilience::{CircuitBreaker, CircuitConfig};

let typed: Typed<Order> = Typed::new(Arc::new(MemoryAdapter::new()));
let breaker = CircuitBreaker::new(CircuitConfig::default());

let order = typed
    .get_or_set("order:42", Some(Duration::from_secs(60)), || async {
        // the loader is what the circuit protects: an upstream / DB call.
        breaker.execute(|| async { fetch_order_from_db("42").await }).await
            .map_err(|e| firefly_cache::CacheError::Backend(e.to_string()))
    })
    .await?;
```

You now have a fast, resilient read path. The remaining chapters cover shipping:
testing against real infrastructure, the CLI, and production deployment. Continue
to [Testing](./18-testing.md).
