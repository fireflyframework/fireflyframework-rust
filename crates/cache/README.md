# `firefly-cache`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-common-cache` · **Go module:** `cache`

## Overview

`firefly-cache` is the framework's **distributed-cache abstraction**. It
exposes a single port — `Adapter` — and ships three implementations
(`MemoryAdapter`, `NoOpAdapter`, `FallbackAdapter`) plus a typed wrapper
(`Typed<T>`) with `get_or_set` memoisation. Every consumer (CQRS query
cache, idempotency middleware, custom service code) talks to the same
`Adapter` regardless of whether it's running an in-process map during
local dev or — once the Redis adapter ships in the next minor — a Redis
cluster in production.

## Why a separate crate?

Rust has no standard cache abstraction. Many projects pick a library,
scatter the API surface across the codebase, and then can't swap
backends without a rewrite. `firefly_cache::Adapter` is **the single
contract** every Firefly crate agrees on — enforced at compile time
through the type system, and object-safe so adapters compose behind
`Arc<dyn Adapter>`.

## Mental model

```
       ┌──────────────────────────────────────┐
       │              Adapter (port)          │
       └──────────────────────────────────────┘
            ▲                ▲             ▲
            │                │             │
   ┌────────┴────┐  ┌────────┴────┐  ┌─────┴────┐
   │ MemoryAdapter│  │ NoOpAdapter │  │ FallbackAdapter │ ← composes 2 adapters
   │ map + RwLock │  │ always miss │  │  primary │
   └─────────────┘  └─────────────┘  │  + secondary │
                                     └──────────┘
```

`FallbackAdapter` is itself an `Adapter`, so consumers stay insulated
from the failover behaviour.

## Public surface

### `Adapter`

```rust,ignore
#[async_trait]
pub trait Adapter: Send + Sync {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError>;
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError>;
    async fn delete(&self, key: &str) -> Result<(), CacheError>;
    async fn clear(&self) -> Result<(), CacheError>;
    fn name(&self) -> String;
    async fn health_check(&self) -> Result<(), CacheError>;
}
```

A miss is signalled by `CacheError::NotFound` — the Rust analogue of the
Go port's `ErrNotFound` sentinel, rendering the same
`firefly/cache: not found` message. `ttl: None` (or a zero duration)
means no expiry, matching Go's `ttl <= 0`.

### Implementations

| Type                                  | Backing                       | Notes                                                          |
|---------------------------------------|-------------------------------|----------------------------------------------------------------|
| `MemoryAdapter`                       | `HashMap` + tokio `RwLock`    | TTL-aware (lazy eviction); copy-on-read so callers can't mutate stored bytes |
| `NoOpAdapter`                         | none                          | Drop-in for tests / disabled-cache configurations              |
| `FallbackAdapter { primary, secondary }` | composite                     | Tries primary first; on transport error or miss, falls through to secondary; writes to both |

### Typed wrapper

```rust,ignore
pub struct Typed<T> { pub adapter: Arc<dyn Adapter>, /* … */ }

impl<T: Serialize + DeserializeOwned> Typed<T> {
    pub async fn get(&self, key: &str) -> Result<T, CacheError>;
    pub async fn set(&self, key: &str, value: &T, ttl: Option<Duration>) -> Result<(), CacheError>;
    pub async fn get_or_set(&self, key, ttl, loader) -> Result<T, CacheError>;
}
```

`get_or_set` is the memoisation primitive — it consults the cache, calls
the loader on miss, persists, and returns the loaded value. Caching
errors do **not** mask successful loader results. Values are stored as
`serde_json` bytes, wire-compatible with the Go port's `encoding/json`
output for equivalently-tagged types.

## Quick start

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
            // load from the repository on miss
            Ok(Order { id: "42".into() })
        })
        .await?;
    assert_eq!(order.id, "42");
    Ok(())
}
```

For high availability, compose Redis (planned for the next minor) +
Memory:

```rust,ignore
let cache = FallbackAdapter::new(redis_adapter, Arc::new(MemoryAdapter::new()));
```

## Testing

```bash
cargo test -p firefly-cache
```

Covers TTL eviction, fallback union semantics, copy-on-read isolation,
the `get_or_set` loader-runs-once invariant, Go-compatible JSON bytes,
and `Send + Sync` object safety.
