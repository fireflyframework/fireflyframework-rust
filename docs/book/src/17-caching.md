# Caching

By the end of this chapter, you will understand exactly how Lumen's
`GET /api/v1/wallets/:id` serves a wallet view from a 30-second cache — and why
every deposit, withdrawal, and transfer *invalidates* that cache so a read after
a write never lies. The cache itself was switched on back in
[CQRS](./09-cqrs.md) with one annotation and one middleware; this chapter opens
it up: the unified cache port behind it, the backends you can swap in, and the
`firefly-resilience` decorators that protect the slow call a cache miss falls
through to.

`firefly-cache` exposes a single port — `Adapter` — and ships in-process,
no-op, and fallback implementations plus a typed memoization wrapper. Code
against the port and select the backend (in-memory, Redis, Postgres) at wiring
time. All of it reaches Lumen through the one `firefly` facade, as
`firefly::cache` and `firefly::resilience`.

> **Spring parity.** `Adapter` is the `Cache` abstraction; `Typed::get_or_set`
> is `@Cacheable`; the query-side `#[firefly(cache_ttl = "30s")]` is the
> declarative read-through Lumen uses. The resilience decorators are
> Resilience4j (circuit breaker, rate limiter, bulkhead, timeout) composed for
> reactive Rust.

## Lumen's read-side cache

Lumen's `GetWallet` query carries its caching policy as a declaration sitting
right next to the type — no cache code in the handler:

```rust
/// `GET /api/v1/wallets/:id` query. `#[firefly(cache_ttl = "30s")]` is reflected
/// on the generated `Message::cache_ttl`, so a `QueryCache` memoises reads for
/// 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    pub id: String,
}
```

The `#[derive(Query)]` macro reads that attribute and emits a `cache_ttl()` on
the generated `Message` impl — a fact the test pins so it can never silently
disappear:

```rust
#[test]
fn get_wallet_carries_cache_ttl() {
    assert!(GetWallet::default().cache_ttl().is_some());
}
```

The TTL is inert until something *honors* it. That something is the `QueryCache`
middleware, installed on the bus in Lumen's composition root (`build_app` in
`src/web.rs`):

```rust
use firefly::cqrs::QueryCache;

// Read-side caching on the bus (honours GetWallet's 30s cache_ttl). The
// handle is kept so a mutation can invalidate it for read-after-write.
let query_cache = QueryCache::new();
bus.use_middleware(query_cache.middleware());
// Validation middleware enforces the `#[firefly(validate)]` checks.
bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());
```

`QueryCache::new()` builds the cache; `query_cache.middleware()` returns the bus
middleware that consults it. When a `GetWallet` flows through the bus, the
middleware checks the cache: a hit returns the memoized `WalletView` without ever
reaching the handler; a miss runs the handler, stores the result under the
query's key for 30 seconds, and returns it. The `query_cache` handle is kept on
`LumenApp` precisely so the write side can reach back into it.

> **Why the handle, not just the middleware?** The middleware reads and fills the
> cache; only a *holder of the handle* can invalidate it. Lumen keeps
> `query_cache` on `LumenApp` and passes a clone into the controller state, so
> the mutating handlers can drop stale entries the moment they change a balance.

### Read-after-write invalidation

A 30-second TTL is great for a read-heavy view and a disaster for correctness if
you never invalidate. Deposit `$2.50`, then read the balance within 30 seconds,
and a naive cache would happily serve the *old* number. Lumen avoids that by
invalidating the whole `GetWallet` family after every mutation. From the
controller in `src/web.rs`:

```rust
#[post("/wallets/:id/deposit")]
async fn deposit(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    Json(body): Json<AmountBody>,
) -> WebResult<Json<WalletView>> {
    let cmd = Deposit { wallet_id: id, amount: body.amount };
    let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(view))
}
```

`invalidate_type::<GetWallet>()` drops every cached `GetWallet` entry, so the
next read re-runs the handler and reflects the write. The withdraw handler does
the same, and so does the transfer endpoint from [Sagas](./12-sagas.md) — a
transfer changes *two* balances, so it must invalidate the family too:

```rust
// In the transfer handler — a transfer touches both wallets' views.
api.query_cache.invalidate_type::<GetWallet>();
```

The end-to-end HTTP test proves the loop closes: deposit then withdraw, then read
back, and the cached view reflects both writes rather than a stale balance.

```rust
// after a deposit(+250) and a withdraw(-50) on an opening balance of 100:
let view: WalletView = body_json(res).await;
assert_eq!(view.balance, 300);   // read-after-write is honest
assert_eq!(view.version, 3);
```

> **Spring parity.** This is `@Cacheable` on the query plus `@CacheEvict` on the
> command, the read-through/evict pattern you know from Spring's cache
> abstraction. The declaration lives on the message (`#[firefly(cache_ttl)]`),
> the eviction is an explicit `invalidate_type` at the write boundary, and the
> backing store is the same swappable `Adapter` port every other consumer uses.

## The cache port

Everything above runs on `MemoryAdapter` by default, but the `QueryCache` — like
every other consumer (session store, OAuth2 token store) — depends on the
abstract `Adapter` port, never on a concrete client. That is what lets you move
Lumen's cache to Redis without touching a handler. The port:

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

## Typed memoization

Lumen's `QueryCache` keys and serializes for you, but when you cache something
*outside* the query bus — say, a wallet's risk score fetched from an external
service — the `Typed<T>` wrapper is the primitive. It serializes values as
`serde_json` bytes (wire-compatible with the other ports) and gives you
`get_or_set`: consult the cache, call the loader on a miss, persist, and return
the value. A caching error never masks a successful loader result:

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly::cache::{MemoryAdapter, Typed};

#[derive(serde::Serialize, serde::Deserialize)]
struct WalletView { id: String }

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), firefly::cache::CacheError> {
    let cache = Arc::new(MemoryAdapter::new());
    let typed: Typed<WalletView> = Typed::new(cache);

    let view = typed
        .get_or_set("wallet:wlt_alice", Some(Duration::from_secs(60)), || async {
            // loaded from the ledger / read model on a miss
            Ok(WalletView { id: "wlt_alice".into() })
        })
        .await?;
    assert_eq!(view.id, "wlt_alice");
    Ok(())
}
```

## Choosing and composing backends

`Core::new` (and therefore `WebStack`, which Lumen builds on) uses
`MemoryAdapter` by default; pass an `Arc<dyn cache::Adapter>` in
`CoreConfig.cache` to swap it. For high availability, compose Redis with an
in-process fallback so a Redis blip degrades to local caching instead of
failing:

```rust,ignore
use std::sync::Arc;
use firefly::cache::{FallbackAdapter, MemoryAdapter};

// Tries Redis first; on a transport error or miss, falls through to memory
// and writes to both.
let cache = FallbackAdapter::new(redis_adapter, Arc::new(MemoryAdapter::new()));
```

Because everything downstream depends on the port, swapping the backend changes
one constructor — Lumen's handlers, the `QueryCache`, and the session store are
untouched. A single-process Lumen keeps the default in-memory cache; a multi-node
deployment swaps in `RedisAdapter` so a `GetWallet` cached on one node is seen on
the next, and so an `invalidate_type` on any node clears the shared entry.

> **The in-memory → real-infra swap.** This mirrors the event store and broker
> swap you have already seen: develop and test against the in-memory adapter,
> wire the distributed backend in production via `CoreConfig`. The teaching
> baseline stays a no-infra `cargo run`; the production path is one line of
> wiring.

## Resilience decorators

`firefly-resilience` guards the calls a cache miss falls through to — and any
outbound call, like the Payments settlement from
[HTTP Clients](./13-http-clients.md). Four primitives, composable into a
`Chain`:

| Decorator        | Guards against                               | Error on trip                  |
|------------------|----------------------------------------------|--------------------------------|
| `CircuitBreaker` | cascading failure of a slow / failing dep    | `ResilienceError::CircuitOpen` |
| `RateLimiter`    | outbound rate overrun (token bucket)         | `ResilienceError::RateLimited` |
| `Bulkhead`       | resource exhaustion from runaway concurrency | `ResilienceError::BulkheadFull` |
| `Timeout`        | stuck calls                                  | `ResilienceError::Timeout`     |

A `Chain` composes them into a single guarded call:

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly::resilience::{Bulkhead, Chain, CircuitBreaker, CircuitConfig, Timeout};

# async fn ex() -> Result<(), firefly::resilience::ResilienceError> {
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
use firefly::resilience::{Bulkhead, CircuitBreaker, CircuitConfig, RateLimiter, Timeout};

let cb = CircuitBreaker::new(CircuitConfig::default());
let _ = cb.execute(|| async { settle().await }).await;

let rl = RateLimiter::new(100.0, 200);        // 100 rps, burst 200
let _ = rl.execute(|| async { call().await }).await;

let bh = Bulkhead::new(20);
let _ = bh.try_execute(|| async { call().await }).await; // non-blocking; BulkheadFull if full

let to = Timeout::new(Duration::from_secs(2));
let _ = to.execute(|| async { slow_call().await }).await;
```

> **Spring parity.** `Chain` is the composed Resilience4j decorator stack; the
> ordering (timeout outermost, breaker, bulkhead innermost) is the same call-wrap
> order you tune in `resilience4j.yaml`. `CircuitBreaker::execute` returns the
> operation's value (so a guarded read still hands you the `WalletView`), while
> `Chain::execute` is for guarded operations whose value you discard.

## A complete read path

The pieces fit together as a cache-aside read protected by a circuit breaker —
exactly the shape a multi-node Lumen would use to serve a wallet view from Redis,
repairing from the read model (or the event stream) on a miss while a breaker
protects that repair:

```rust,ignore
use std::sync::Arc;
use std::time::Duration;
use firefly::cache::{MemoryAdapter, Typed};
use firefly::resilience::{CircuitBreaker, CircuitConfig};

let typed: Typed<WalletView> = Typed::new(Arc::new(MemoryAdapter::new()));
let breaker = CircuitBreaker::new(CircuitConfig::default());

let view = typed
    .get_or_set("wallet:wlt_alice", Some(Duration::from_secs(30)), || async {
        // the loader is what the circuit protects: the read model / event store.
        breaker.execute(|| async { load_wallet_view("wlt_alice").await }).await
            .map_err(|e| firefly::cache::CacheError::Backend(e.to_string()))
    })
    .await?;
```

You now have a fast, resilient read path — the same one Lumen's declarative
`#[firefly(cache_ttl = "30s")]` gives you for free on the query bus.

## What changed in Lumen

- We opened up the read-side cache Lumen has used since CQRS:
  `#[firefly(cache_ttl = "30s")]` on `GetWallet` is honored by the `QueryCache`
  middleware that `build_app` installs on the bus with
  `bus.use_middleware(query_cache.middleware())`.
- We saw why the `query_cache` handle lives on `LumenApp`: so every mutating
  handler — deposit, withdraw, **and** transfer — can call
  `invalidate_type::<GetWallet>()` and keep read-after-write honest within the
  30-second TTL.
- We traced the cache down to its `Adapter` port, the swappable backends
  (`MemoryAdapter` default → `RedisAdapter` for a multi-node deployment via
  `CoreConfig.cache`), the `Typed<T>::get_or_set` memoization primitive, and the
  `FallbackAdapter` for Redis-with-local-fallback.
- We covered the `firefly-resilience` decorators (`CircuitBreaker`, `RateLimiter`,
  `Bulkhead`, `Timeout`) and the `Chain` that composes them — the guard that
  protects both a cache loader and the outbound Payments call from
  [HTTP Clients](./13-http-clients.md).

## Exercises

1. **Prove the TTL.** Write a test that opens a wallet, reads it (priming the
   cache), then deposits *directly through the ledger* (bypassing the controller,
   so no invalidation runs) and reads again within 30 seconds. Assert you still
   see the *old* balance — demonstrating the TTL is real — then call
   `query_cache.invalidate_type::<GetWallet>()` and assert the read now reflects
   the deposit.

2. **Swap in a fallback adapter.** Build a `FallbackAdapter` whose primary always
   errors on `get`/`set` and whose secondary is a `MemoryAdapter`. Wire it into
   `CoreConfig.cache`, run the deposit/withdraw/read flow, and assert correctness
   is unaffected — the cache degrades to the in-process layer instead of failing.

3. **Guard a loader with a `Chain`.** Wrap a deliberately slow loader in a
   `Chain::new().with(Timeout::new(...))` and assert that a loader exceeding the
   deadline surfaces `ResilienceError::Timeout` rather than hanging. Then add a
   `CircuitBreaker` to the chain, trip it with repeated failures, and assert the
   next call fails fast with `ResilienceError::CircuitOpen`.

The remaining chapters fold Lumen back together through the declarative-macro
lens, then cover shipping it. Continue to
[Declarative Services with Macros](./21-declarative-macros.md).
