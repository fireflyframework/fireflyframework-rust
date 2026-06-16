# Caching

Lumen's `GET /api/v1/wallets/:id` already serves a wallet view from a 30-second
cache — you switched it on back in [CQRS](./09-cqrs.md) with one annotation and
one bean, and have used it ever since without thinking about it. This chapter
opens that machinery up. We will trace a read from the query bus down to the
byte-level cache port underneath it, prove *why* every deposit, withdrawal, and
transfer must *invalidate* that cache so a read after a write never lies, and
then wrap the slow call a cache miss falls through to in the resilience
decorators that keep it from taking the whole service down.

Two crates carry this story, and both reach Lumen through the one `firefly`
facade: `firefly-cache` (as `firefly::cache`) exposes a single cache port plus
a handful of backends and a typed wrapper, and `firefly-resilience` (as
`firefly::resilience`) ships the circuit breaker, rate limiter, bulkhead, and
timeout decorators. The CQRS read cache you already have — `firefly::cqrs::QueryCache`
— sits on top of the cache port.

By the end of this chapter you will:

- Explain how `#[firefly(cache_ttl = "30s")]` on a query turns into a real,
  honored 30-second cache, and which bean honors it.
- Keep a read-after-write *honest* by invalidating a query family at every write
  boundary, and prove the loop closes with Lumen's own HTTP test.
- Read and code against the `Adapter` cache port — the single trait every backend
  (in-memory, Redis, Postgres) implements — and swap the backend at one wiring
  point.
- Memoize an arbitrary value outside the query bus with `Typed<T>::get_or_set`.
- Wrap a slow loader (or any outbound call) in a resilience `Chain` so a timeout,
  an open circuit, or a full bulkhead fails fast instead of hanging.

## Concepts you will meet

Each of these is reintroduced in context where it is first used; this is the
short version so the words are not new when you hit them.

> **Note** **Key term — cache.** A *cache* is a fast, usually in-memory store
> that holds the result of an expensive computation so the next request can skip
> the work. The hard part is never the storing — it is knowing when a stored value
> has gone stale. In Spring this is the `@Cacheable` / `@CacheEvict` family backed
> by a `CacheManager`.

> **Note** **Key term — cache port.** A *port* is an abstract interface that
> consumers depend on instead of a concrete backend, so the backend can be swapped
> without touching the consumers. Firefly's cache port is the `Adapter` trait;
> the Spring analog is the `Cache` / `CacheManager` SPI behind `@Cacheable`.

> **Note** **Key term — TTL.** *Time to live* is how long a cached entry stays
> valid before it expires and is treated as absent. A 30-second TTL means a read
> within 30 seconds of the last fill is served from cache; after that it re-runs
> the work. TTL alone is a stale-data ceiling, not a correctness guarantee — that
> is what invalidation is for.

> **Note** **Key term — read-through / cache-aside.** A *read-through* (or
> *cache-aside*) read checks the cache first; on a miss it runs the real work
> (the *loader*), stores the result, and returns it. The next read inside the TTL
> skips the loader. `Typed<T>::get_or_set` is Firefly's read-through primitive.

> **Note** **Key term — resilience decorator.** A *resilience decorator* wraps an
> async call to bound its failure: a *circuit breaker* stops calling a sick
> dependency, a *rate limiter* caps the outbound rate, a *bulkhead* caps
> concurrency, and a *timeout* bounds duration. This mirrors Resilience4j in the
> Spring world.

## Step 1 — See the cache you already have

You did not have to write any cache code to get a cached read — you *declared*
it. Lumen's `GetWallet` query carries its caching policy as an attribute sitting
right next to the type, in `src/commands.rs`:

```rust
/// `GET /api/v1/wallets/:id` query. `#[firefly(cache_ttl = "30s")]` is reflected
/// on the generated `Message::cache_ttl`, so a `QueryCache` memoises reads for
/// 30 seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetWallet {
    /// The wallet id to fetch.
    pub id: String,
}
```

What just happened: the `#[derive(Query)]` macro reads the `#[firefly(cache_ttl =
"30s")]` attribute and emits a `cache_ttl()` method on the generated `Message`
implementation. The attribute is *declarative* — it states the policy where the
type is defined, and the framework wires the behavior. Nothing in the query
handler mentions caching at all.

> **Note** **Key term — declarative caching.** *Declarative* caching means the
> policy lives as an annotation on the type or method, not as imperative code in
> the body. Spring's `@Cacheable(ttl = ...)` is the analog; here it is
> `#[firefly(cache_ttl = "30s")]`.

Because the TTL is now a fact on the generated code, Lumen pins it with a unit
test so it can never silently disappear:

```rust
#[test]
fn get_wallet_carries_cache_ttl() {
    assert!(GetWallet::default().cache_ttl().is_some());
}
```

What just happened: the test constructs a default `GetWallet`, calls the
generated `cache_ttl()`, and asserts it returns `Some(_)`. If someone deletes the
attribute, this test fails — the caching contract is guarded, not assumed.

> **Tip** **Checkpoint.** Open `samples/lumen/src/commands.rs` and find the
> `#[firefly(cache_ttl = "30s")]` line above `GetWallet`, plus the
> `get_wallet_carries_cache_ttl` test. The attribute and the assertion are the
> two ends of the same declaration.

## Step 2 — Find the bean that honors the TTL

A `cache_ttl()` on a message is inert until something *reads* it on the dispatch
path. That something is the `QueryCache` bean and the bus middleware it installs.

> **Note** **Key term — bus middleware.** *Middleware* wraps every message that
> flows through the CQRS bus, running before and after the handler. The read-cache
> middleware checks the cache before the handler runs and fills it after, so a
> cached query never reaches the handler at all. This is Spring's `@Cacheable`
> interception, realized as a bus interceptor.

In Lumen the `QueryCache` is declared as a single `#[bean]` inside `LumenBeans`
(the `#[derive(Configuration)]` holder in `src/web.rs`):

```rust
use firefly::cqrs::QueryCache;

// samples/lumen/src/web.rs — inside `#[bean] impl LumenBeans { ... }`.

/// The read-side query cache honouring `GetWallet`'s 30s TTL (`@Bean`).
#[bean]
fn query_cache(&self) -> QueryCache {
    QueryCache::new()
}
```

What just happened: `QueryCache::new()` builds an empty, in-memory query cache
keyed by message type plus a hash of the message value. Declaring it as a `#[bean]`
is all the wiring you do — when `FireflyApplication::run()` component-scans the
container and finds a `QueryCache` bean, it calls `query_cache.middleware()` for
you and registers that middleware on the bus. (Validation middleware is installed
by the core; you do not register either by hand.)

So when a `GetWallet` flows through the bus, the read-cache middleware:

- **on a hit** returns the memoized `WalletView` *without ever reaching the
  handler*;
- **on a miss** runs the handler, stores the result under the query's key for the
  declared 30 seconds, and returns it.

> **Design note.** This is the same auto-configuration pattern you saw in
> [Quickstart](./02-quickstart.md): "auto-configures the CQRS bus … the read-cache
> middleware whenever a `QueryCache` bean is present." You add a *bean*, not a
> registration call. Spring's analog is auto-configuring `@EnableCaching` behavior
> once a `CacheManager` bean exists.

The same bean is also `#[autowired]` into the controller, so the write side can
reach the exact cache the middleware reads:

```rust
// samples/lumen/src/web.rs — the WalletApi controller.
#[derive(Clone, Controller)]
pub struct WalletApi {
    #[autowired]
    pub bus: Arc<Bus>,
    #[autowired]
    pub ledger: Arc<Ledger>,
    /// The query cache, invalidated after a mutation so a read-after-write
    /// never serves a stale balance within the 30s `GetWallet` TTL (autowired).
    #[autowired]
    pub query_cache: Arc<QueryCache>,
}
```

What just happened: the framework installs *one* `QueryCache` as bus middleware
and hands the *same* `Arc<QueryCache>` to the controller. `QueryCache` is
`Arc`-backed and cheap to clone, so both handles share the same entries — the
middleware fills the cache, and the controller can drop entries from it.

> **Tip** **Checkpoint.** In `src/web.rs`, the `query_cache` `#[bean]` and the
> `#[autowired] pub query_cache: Arc<QueryCache>` field refer to the same shared
> cache. One reads and fills it (middleware); the other invalidates it
> (controller).

## Step 3 — Keep read-after-write honest

A 30-second TTL is a gift for a read-heavy view and a disaster for correctness if
you never invalidate. Deposit `$2.50`, then read the balance within 30 seconds,
and a cache that only knew about the TTL would happily serve the *old* number.

> **Note** **Key term — invalidation.** *Invalidation* (or *eviction*) is the
> deliberate removal of a cached entry that is now wrong, forcing the next read to
> re-run the work. Read-after-write correctness comes from invalidating at the
> write boundary — the moment a balance changes — not from waiting for a TTL.

Lumen avoids the staleness by invalidating the whole `GetWallet` family after
every mutation. Here is the deposit handler in `src/web.rs`:

```rust
#[post("/wallets/:id/deposit", summary = "Deposit funds", status = 200)]
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

What just happened, line by line:

- `api.bus.send(cmd)` dispatches the `Deposit` command through the bus and awaits
  the resulting `WalletView`. `map_err(cqrs_to_web)?` turns a CQRS error into an
  RFC 9457 `application/problem+json` web error.
- `api.query_cache.invalidate_type::<GetWallet>()` drops *every* cached
  `GetWallet` entry. Internally `invalidate_type::<Q>()` deletes every cache key
  prefixed with `Q`'s type name plus the `:` separator, so the whole `GetWallet`
  family is cleared — the next read re-runs the handler and reflects the write.

Why it matters: the TTL bounds how stale a value *can* get; the explicit
invalidation guarantees a read *after a write you made* is never stale at all.

The withdraw handler does exactly the same, and so does the transfer endpoint
from [Sagas](./12-sagas.md) — a transfer changes *two* balances, so it must
invalidate the family too:

```rust
// In the transfer handler — a transfer touches both wallets' views.
api.query_cache.invalidate_type::<GetWallet>();
```

What just happened: because the cache key includes the message *value*, a transfer
between wallet A and wallet B would otherwise have to evict two specific keys.
Invalidating the whole `GetWallet` type is simpler and always correct — it can
never miss a key — at the cost of dropping cache entries for unrelated wallets,
which simply re-fill on their next read.

> **Design note.** Lumen pairs *read-through caching on the message*
> (`#[firefly(cache_ttl)]`) with *explicit eviction at the write boundary*
> (`invalidate_type`). The reader memoizes; the writer drops the family the moment
> it changes a balance. The backing store is the same swappable `Adapter` port
> every other cache consumer uses (Step 4), so this policy is independent of where
> the bytes actually live.

The end-to-end HTTP test proves the loop closes. Open a wallet with a balance of
`100`, deposit `+250`, withdraw `-50`, then read it back through the cached `GET`:

```rust
// after a deposit(+250) and a withdraw(-50) on an opening balance of 100:
let view: WalletView = get_wallet(&app, &opened.id).await;
assert_eq!(view.balance, 300);   // read-after-write is honest
assert_eq!(view.version, 3);
```

What just happened: each mutating call invalidated the `GetWallet` family, so the
final `GET` re-ran the query against the read model rather than replaying a stale
cached view. The balance reflects both writes (`100 + 250 - 50 = 300`) and the
version is `3` (one event per mutation on top of the open).

> **Tip** **Checkpoint.** Run the wallet HTTP tests:
> `cargo test -p lumen deposit_and_withdraw_update_the_balance`. A green test means
> the read-after-write loop closes — the cache is honored *and* invalidated.

## Step 4 — Trace the read down to the cache port

Everything in Steps 1–3 runs on an in-process cache by default, but the
`QueryCache` — like every other cache consumer — ultimately depends on the
abstract `Adapter` port, never on a concrete client. That single seam is what
lets you move Lumen's cache to Redis without touching a single handler.

Here is the port (from `firefly-cache`, reachable as `firefly::cache::Adapter`):

```rust,ignore
use std::time::Duration;
use async_trait::async_trait;

#[async_trait]
pub trait Adapter: Send + Sync {
    /// Returns the cached bytes for `key`, or `CacheError::NotFound` when absent.
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError>;

    /// Stores `value` under `key` for `ttl` (None or zero = no expiry).
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError>;

    /// Removes the entry. A missing key is a no-op.
    async fn delete(&self, key: &str) -> Result<(), CacheError>;

    /// Removes every entry.
    async fn clear(&self) -> Result<(), CacheError>;

    /// Human-readable adapter identifier (`memory`|`redis`|`noop`|...).
    fn name(&self) -> String;

    /// Returns Ok when the backend is reachable.
    async fn health_check(&self) -> Result<(), CacheError>;

    // The methods below ship default impls so older adapters keep compiling;
    // backends with a cheaper native path (Redis SET NX, SCAN MATCH) override them.

    /// Writes only when `key` is absent; true when the write happened.
    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool, CacheError>;

    /// Whether a live entry exists for `key`.
    async fn exists(&self, key: &str) -> Result<bool, CacheError>;

    /// Removes every entry whose key starts with `prefix`; returns the count.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError>;

    /// A point-in-time counter snapshot, or None when the adapter has none.
    async fn stats(&self) -> Option<CacheStats>;
}
```

What just happened: values cross the port as raw `Vec<u8>` — the port itself
knows nothing about your types. A cache miss is signalled by the
`CacheError::NotFound` variant (not an `Option`), and a `ttl` of `None` (or zero)
means "no expiry." The four trait methods with default implementations
(`set_if_absent`, `exists`, `delete_prefix`, `stats`) let an adapter ship without
them and let a richer backend override with a native, cheaper path.

> **Note** **Key term — adapter.** An *adapter* is a concrete implementation of a
> port. Firefly ships several, and you choose one at wiring time:

| Implementation        | Backing                | Use                                       |
|-----------------------|------------------------|-------------------------------------------|
| `MemoryAdapter`       | `HashMap` + `RwLock`   | in-process, TTL-aware — **the default**   |
| `NoOpAdapter`         | none                   | tests / a deliberately disabled cache     |
| `FallbackAdapter`     | composite (two ports)  | primary-then-secondary, writes to both    |
| `RedisAdapter`        | Redis (RESP)           | distributed cache (`firefly-cache-redis`) |

A `NoOpAdapter` reports every `get` as `NotFound` and silently succeeds every
write — it is the cache that does nothing, perfect for a test that wants the
handler to run every time. `MemoryAdapter` is the live, TTL-aware in-process map
Lumen uses out of the box.

> **Tip** **Checkpoint.** You can name all four adapters and say which is the
> default (`MemoryAdapter`) and which disables caching (`NoOpAdapter`). All of
> them are `firefly::cache::*` and implement the one `Adapter` trait.

## Step 5 — Memoize a value outside the query bus

`QueryCache` keys and serializes query results for you. But when you want to cache
something that does *not* flow through the query bus — say, a wallet's risk score
fetched from an external service — the `Typed<T>` wrapper is the primitive.

> **Note** **Key term — `Typed<T>`.** `Typed<T>` wraps an `Adapter` with
> JSON-encoded read/write helpers for a concrete type `T`. It serializes values as
> `serde_json` bytes (wire-compatible with the other ports) and gives you
> `get_or_set`: consult the cache, call the loader on a miss, persist the result,
> and return it. A caching error never masks a successful loader result.

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly::cache::{MemoryAdapter, Typed};

#[derive(serde::Serialize, serde::Deserialize)]
struct WalletView {
    id: String,
}

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

What just happened, block by block:

- `Arc::new(MemoryAdapter::new())` builds the byte-level cache and wraps it in an
  `Arc` so `Typed` can share it. `Typed::new(cache)` layers JSON encoding on top
  for the `WalletView` type.
- `get_or_set(key, ttl, loader)` is the read-through call. On the first run the key
  is absent, so the loader closure runs, its `WalletView` is JSON-encoded and
  stored under the key for 60 seconds, and the value is returned. A second call
  within 60 seconds skips the loader and decodes the stored bytes.
- The loader returns `Result<WalletView, CacheError>` — any error inside it
  surfaces; but a failure to *write* the loaded value back to the cache does
  **not** mask a successful load (the value is still returned).

`Typed<T>` also offers `put` (always write and return — the always-store path),
`delete` (remove one key), and `delete_prefix` (evict a key family), but
`get_or_set` is the workhorse.

> **Tip** **Checkpoint.** You can describe the three outcomes of `get_or_set`: a
> hit (decode and return, no loader), a miss (run loader, store, return), and a
> store-failure-after-load (return the value anyway, drop the write error).

## Step 6 — Swap and compose backends at one wiring point

The default cache is `MemoryAdapter`. Where does that default live? `Core::new`
(and therefore `WebStack`, which Lumen builds on) reads `CoreConfig.cache: Option<Arc<dyn cache::Adapter>>`
and substitutes a `MemoryAdapter` when it is `None`. To use a different backend,
you pass a different `Arc<dyn Adapter>` there — one constructor, nothing else.

> **Note** **Key term — `FallbackAdapter`.** A `FallbackAdapter` is itself an
> `Adapter` that wraps a *primary* and a *secondary*: it tries the primary first
> and, on a transport failure (anything other than a plain miss), demotes the
> request to the secondary and writes to both. Consumers never see the failover —
> they just see an `Adapter`.

For high availability, compose Redis with an in-process fallback so a Redis blip
degrades to local caching instead of failing the request:

```rust,ignore
use std::sync::Arc;
use firefly::cache::{FallbackAdapter, MemoryAdapter, RedisAdapter};

// Connect the distributed primary (RESP over the network)...
let redis = Arc::new(RedisAdapter::connect("redis://127.0.0.1:6379/0").await?);

// ...and fall through to a local in-process cache on a transport error or miss,
// writing to both so the local layer warms up.
let cache: Arc<dyn firefly::cache::Adapter> =
    Arc::new(FallbackAdapter::new(redis, Arc::new(MemoryAdapter::new())));
```

What just happened: `RedisAdapter::connect(url)` dials Redis and returns a ready
adapter; `FallbackAdapter::new(primary, secondary)` composes it with a
`MemoryAdapter`. The composite is *also* an `Adapter`, so you hand it to
`CoreConfig.cache` exactly as you would any single backend.

Because everything downstream depends on the port, swapping the backend changes
*one* constructor — Lumen's handlers, the `QueryCache`, and the session store are
untouched. A single-process Lumen keeps the default in-memory cache; a multi-node
deployment swaps in `RedisAdapter` so a `GetWallet` cached on one node is visible
on the next, and so an `invalidate_type` on any node clears the shared entry.

> **Design note.** This mirrors the event-store and broker swap you have already
> seen: develop and test against the in-memory adapter, wire the distributed
> backend in production via `CoreConfig`. The teaching baseline stays a no-infra
> `cargo run`; the production path is one line of wiring. [Production &
> Deployment](./20-production.md) does exactly this swap for real.

> **Tip** **Checkpoint.** You can name the single seam — `CoreConfig.cache` — that
> changes the cache backend for the whole service, and explain why no handler,
> `QueryCache`, or controller has to change when you swap it.

## Step 7 — Protect the loader with resilience decorators

A cache miss falls through to a slow call: the read model, the event store, or an
external service. If that call hangs or starts failing, an unguarded loader can
drag the whole service down with it. `firefly-resilience` guards exactly that — and
any outbound call, like the Payments settlement from [HTTP
Clients](./13-http-clients.md).

> **Note** **Key term — circuit breaker.** A *circuit breaker* watches a guarded
> call. While it is *closed*, calls flow through and failures are counted; after
> enough failures it *opens* and short-circuits subsequent calls with an immediate
> error, sparing the sick dependency; after a cooldown it lets one trial call
> through (*half-open*) to decide whether to close again. This is Resilience4j's
> `CircuitBreaker`.

There are four decorators, each shielding one failure mode:

| Decorator        | Guards against                               | Error on trip                   |
|------------------|----------------------------------------------|---------------------------------|
| `CircuitBreaker` | cascading failure of a slow / failing dep    | `ResilienceError::CircuitOpen`  |
| `RateLimiter`    | outbound rate overrun (token bucket)         | `ResilienceError::RateLimited`  |
| `Bulkhead`       | resource exhaustion from runaway concurrency | `ResilienceError::BulkheadFull` |
| `Timeout`        | stuck calls                                  | `ResilienceError::Timeout`      |

> **Note** **Key term — `Chain`.** A `Chain` composes decorators into a single
> guarded call. Decorators run left-to-right with the leftmost outermost, so
> `Chain::new().with(timeout).with(breaker).with(bulkhead)` evaluates as
> `timeout(breaker(bulkhead(call)))` — a deadline bounds the whole call while the
> breaker and bulkhead protect the inner operation.

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly::resilience::{Bulkhead, Chain, CircuitBreaker, CircuitConfig, Timeout};

# async fn ex() -> Result<(), firefly::resilience::ResilienceError> {
let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));

let guarded = Chain::new()
    .with(Timeout::new(Duration::from_secs(2)))   // per-call deadline (outermost)
    .with_shared(breaker.clone())                 // open the circuit on repeated failures
    .with(Bulkhead::new(20));                      // cap concurrent in-flight calls

guarded.execute(|| async {
    // the protected operation — a cache loader, an upstream call, ...
    Ok(())
}).await?;
# Ok(())
# }
```

What just happened, line by line:

- `CircuitBreaker::new(CircuitConfig::default())` builds a breaker with the default
  policy (trip after 5 failures, stay open 30 seconds). It is wrapped in `Arc` so
  you can both hand it to the chain *and* keep a handle to inspect its state.
- `Chain::new()` starts an empty chain. `.with(decorator)` appends a decorator the
  chain *owns*; `.with_shared(arc_decorator)` appends one you keep a handle to —
  that is why the breaker uses `.with_shared(breaker.clone())` while the freshly
  built `Timeout` and `Bulkhead` use `.with(...)`.
- `guarded.execute(|| async { ... })` runs your closure through all three
  decorators, leftmost outermost. The closure returns `Result<(), ResilienceError>`;
  if any decorator trips, `execute` returns that decorator's error and your
  operation may never run.

> **Warning** `Chain::with(...)` takes ownership and requires its argument to
> implement the decorator trait directly — a bare `Arc<CircuitBreaker>` does *not*.
> When you want to keep a handle to a breaker (to read its state, or to share it
> across chains), use `.with_shared(breaker.clone())`, which takes the `Arc`. Using
> `.with(breaker)` on an `Arc` will not compile.

Each decorator also stands alone. Unlike `Chain::execute` (whose value you
discard), `CircuitBreaker::execute` *returns the operation's value*, so a guarded
read still hands you the `WalletView`:

```rust,ignore
use std::time::Duration;
use firefly::resilience::{Bulkhead, CircuitBreaker, CircuitConfig, RateLimiter, Timeout};

let cb = CircuitBreaker::new(CircuitConfig::default());
let _ = cb.execute(|| async { settle().await }).await;       // returns settle()'s value

let rl = RateLimiter::new(100.0, 200);                       // 100 rps, burst 200
let _ = rl.execute(|| async { call().await }).await;

let bh = Bulkhead::new(20);
let _ = bh.try_execute(|| async { call().await }).await;     // non-blocking; BulkheadFull if full

let to = Timeout::new(Duration::from_secs(2));
let _ = to.execute(|| async { slow_call().await }).await;
```

What just happened: each primitive has its own `execute` that wraps a closure
returning `Result<T, ResilienceError>` and propagates the operation's value on
success. `Bulkhead` additionally offers `try_execute`, the non-blocking variant
that returns `BulkheadFull` immediately rather than waiting for a free slot.

> **Tip** **Checkpoint.** You can explain the difference between `Chain::execute`
> (value discarded, returns `Result<(), _>`) and `CircuitBreaker::execute` (returns
> the operation's `T`), and you know to reach for `.with_shared(arc.clone())` when
> the chain needs a breaker you still hold.

## Step 8 — Assemble a resilient cache-aside read

The two halves of this chapter compose into a single shape: a cache-aside read
whose loader is protected by a circuit breaker. This is exactly what a multi-node
Lumen would use to serve a wallet view from Redis, repairing from the read model
(or the event stream) on a miss while the breaker protects that repair:

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
        breaker
            .execute(|| async { load_wallet_view("wlt_alice").await })
            .await
            .map_err(|e| firefly::cache::CacheError::Backend(e.to_string()))
    })
    .await?;
```

What just happened: `get_or_set` is the outer cache-aside read. On a hit it returns
the decoded `WalletView` and the loader never runs. On a miss the loader runs the
real repair — but wrapped in `breaker.execute(...)`, so a streak of failures opens
the circuit and the *next* miss fails fast with `CircuitOpen` instead of hammering
a sick read model. The `map_err` adapts the `ResilienceError` into a
`CacheError::Backend` so it fits `get_or_set`'s error type.

You now have a fast, resilient read path — built by hand from the two primitives,
and the same shape Lumen's declarative `#[firefly(cache_ttl = "30s")]` gives you
for free on the query bus.

> **Tip** **Checkpoint.** You can trace the layering: `get_or_set` (cache-aside)
> wraps `breaker.execute` (failure protection) wraps the real loader (read model /
> event store). The cache absorbs the happy path; the breaker absorbs the failure
> path.

## Recap — what you now understand about Lumen's cache

- The read-side cache Lumen has used since [CQRS](./09-cqrs.md) is *declarative*:
  `#[firefly(cache_ttl = "30s")]` on `GetWallet` is honored by the read-cache bus
  middleware that `FireflyApplication` auto-installs whenever a `QueryCache`
  `#[bean]` is present.
- The `QueryCache` is a single bean — installed as bus middleware by the framework
  and `#[autowired]` into the controller — so every mutating handler (deposit,
  withdraw, **and** transfer) calls `invalidate_type::<GetWallet>()` to keep
  read-after-write honest within the 30-second TTL.
- Underneath the `QueryCache` is the swappable `Adapter` port: `MemoryAdapter` by
  default, `NoOpAdapter` to disable caching, `FallbackAdapter` for
  Redis-with-local-fallback, and `RedisAdapter` for a multi-node deployment —
  selected at one wiring point, `CoreConfig.cache`.
- `Typed<T>::get_or_set` is the read-through memoization primitive for values
  outside the query bus; a write failure after a successful load never masks the
  value.
- `firefly-resilience` ships `CircuitBreaker`, `RateLimiter`, `Bulkhead`, and
  `Timeout`, composable through a `Chain`, to guard both a cache loader and any
  outbound call (like the Payments settlement from [HTTP
  Clients](./13-http-clients.md)).

## Exercises

1. **Prove the TTL is real.** Write a test that opens a wallet, reads it (priming
   the cache), then deposits *directly through the ledger* (bypassing the
   controller, so no `invalidate_type` runs) and reads again within 30 seconds.
   Assert you still see the *old* balance — demonstrating the TTL is genuinely
   serving a memoized value — then call
   `query_cache.invalidate_type::<GetWallet>()` and assert the next read reflects
   the deposit.

2. **Disable the cache with `NoOpAdapter`.** The `QueryCache` itself is in-memory,
   but the byte-level cache the rest of the service uses is `CoreConfig.cache`.
   Build a `CoreConfig` with `cache: Some(Arc::new(NoOpAdapter::default()))`, boot
   the service, and confirm the byte-level cache always reports a miss while the
   wallet flow still passes — useful when you want to measure cold-path latency.

3. **Swap in a fallback adapter.** Build a `FallbackAdapter` whose primary always
   errors on `get`/`set` (a hand-rolled `Adapter` that returns
   `CacheError::Backend(...)`) and whose secondary is a `MemoryAdapter`. Wire it
   into `CoreConfig.cache`, run the deposit/withdraw/read flow, and assert
   correctness is unaffected — the cache degrades to the in-process layer instead
   of failing.

4. **Guard a loader with a `Chain`.** Wrap a deliberately slow loader in a
   `Chain::new().with(Timeout::new(Duration::from_millis(50)))` and assert a loader
   exceeding the deadline surfaces `ResilienceError::Timeout` (check
   `err.is_timeout()`) rather than hanging. Then add `.with_shared(breaker.clone())`
   for a `CircuitBreaker`, trip it with repeated failures, and assert the next call
   fails fast with `ResilienceError::CircuitOpen` (check `err.is_circuit_open()`).

5. **Memoize outside the bus.** Use `Typed<T>::get_or_set` to cache a computed
   value (e.g. a wallet's risk score) under a 10-second TTL. Call it twice with a
   loader that increments a counter, and assert the counter advanced only once —
   proving the second call hit the cache rather than re-running the loader.

## Where to go next

- See how *every* declaration in this chapter — `#[firefly(cache_ttl)]`,
  `#[bean]`, `#[autowired]`, `#[rest_controller]` — is produced by Firefly's macro
  layer in **[Declarative Services with Macros](./21-declarative-macros.md)**.
- Drive the cached read-after-write loop end-to-end, in-process and with no socket
  bound, in **[Testing](./18-testing.md)**.
- Perform the in-memory → Redis cache swap for a real deployment in **[Production
  & Deployment](./20-production.md)**.
