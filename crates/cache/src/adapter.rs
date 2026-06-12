use std::time::Duration;

use async_trait::async_trait;

/// Error type shared by every cache adapter and the [`Typed`](crate::Typed)
/// facade.
///
/// [`CacheError::NotFound`] is the cache-miss signal — the Rust analogue of
/// the Go port's `ErrNotFound` sentinel — and renders the same message
/// (`firefly/cache: not found`). Every other variant is treated as a
/// transport/backend failure by [`FallbackAdapter`](crate::FallbackAdapter).
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Returned by [`Adapter::get`] when the key is absent.
    #[error("firefly/cache: not found")]
    NotFound,

    /// JSON (de)serialization failure raised by the [`Typed`](crate::Typed)
    /// facade.
    #[error("firefly/cache: codec: {0}")]
    Codec(#[from] serde_json::Error),

    /// Backend / connectivity failure reported by an adapter.
    #[error("firefly/cache: backend: {0}")]
    Backend(String),
}

impl CacheError {
    /// Reports whether this error is the cache-miss signal
    /// ([`CacheError::NotFound`]) — the ergonomic stand-in for Go's
    /// `errors.Is(err, ErrNotFound)`.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, CacheError::NotFound)
    }
}

/// A point-in-time snapshot of an adapter's runtime counters — the Rust
/// rendering of pyfly's `InMemoryCache.get_stats()` / `RedisCacheAdapter
/// .get_stats()` dictionary.
///
/// `hit_rate` is `hits / (hits + misses)`, or `0.0` when no read has been
/// observed (matching pyfly's `requests else 0.0` guard). Adapters that do
/// not track a particular counter report `0` for it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CacheStats {
    /// Number of live (non-expired) entries the adapter currently holds.
    /// For remote backends this is the server-reported key count
    /// (e.g. Redis `DBSIZE`).
    pub size: u64,
    /// Cumulative count of reads that found a live value.
    pub hits: u64,
    /// Cumulative count of reads that missed (absent or expired).
    pub misses: u64,
    /// Cumulative count of entries removed — explicit deletes, prefix
    /// evictions, and LRU/TTL evictions all count.
    pub evictions: u64,
    /// `hits / (hits + misses)`; `0.0` when no read has happened yet.
    pub hit_rate: f64,
}

impl CacheStats {
    /// Builds a [`CacheStats`] from the raw counters, deriving `hit_rate`
    /// exactly as pyfly does (`hits / requests`, or `0.0` when no read has
    /// occurred).
    #[must_use]
    pub fn from_counters(size: u64, hits: u64, misses: u64, evictions: u64) -> Self {
        let requests = hits + misses;
        let hit_rate = if requests == 0 {
            0.0
        } else {
            hits as f64 / requests as f64
        };
        Self {
            size,
            hits,
            misses,
            evictions,
            hit_rate,
        }
    }

    /// Total reads observed (`hits + misses`) — pyfly's `requests` field.
    #[must_use]
    pub fn requests(&self) -> u64 {
        self.hits + self.misses
    }
}

/// The canonical Firefly cache port. Implementations must be safe for
/// concurrent use (`Send + Sync`), and the trait is object-safe so adapters
/// compose behind `Arc<dyn Adapter>`.
///
/// The methods [`set_if_absent`](Adapter::set_if_absent),
/// [`exists`](Adapter::exists), [`delete_prefix`](Adapter::delete_prefix) and
/// [`stats`](Adapter::stats) carry default implementations so that adapters
/// shipped before they existed keep compiling unchanged. Backends with a
/// native, atomic, or cheaper path (Redis `SET NX`, `SCAN MATCH`, `DBSIZE`;
/// the in-process [`MemoryAdapter`](crate::MemoryAdapter)) override them.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Returns the cached bytes for `key`, or [`CacheError::NotFound`] when
    /// absent.
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError>;

    /// Stores `value` under `key` for the given `ttl`. A `ttl` of `None`
    /// (or a zero duration) means no expiry — the Rust spelling of the Go
    /// port's `ttl <= 0`.
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError>;

    /// Removes the entry. A missing key is a no-op (returns `Ok`).
    async fn delete(&self, key: &str) -> Result<(), CacheError>;

    /// Removes every entry from the cache.
    async fn clear(&self) -> Result<(), CacheError>;

    /// Returns a human-readable adapter identifier (`memory`|`redis`|`noop`|...).
    fn name(&self) -> String;

    /// Returns `Ok` when the backend is reachable.
    async fn health_check(&self) -> Result<(), CacheError>;

    /// Stores `value` under `key` only when the key is currently absent,
    /// returning `true` when the write happened and `false` when an entry
    /// was already present — pyfly's `put_if_absent` / Redis `SET NX`. Used
    /// for distributed locking and idempotency guards.
    ///
    /// The default is a non-atomic get-then-set built on
    /// [`exists`](Adapter::exists) and [`set`](Adapter::set); backends that
    /// can do this atomically (Redis `SET NX`) override it.
    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, CacheError> {
        if self.exists(key).await? {
            return Ok(false);
        }
        self.set(key, value, ttl).await?;
        Ok(true)
    }

    /// Reports whether a live (non-expired) entry exists for `key` —
    /// pyfly's `exists`. The default probes via [`get`](Adapter::get),
    /// mapping [`CacheError::NotFound`] to `false`; backends override with a
    /// cheaper existence check (Redis `EXISTS`).
    async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        match self.get(key).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_not_found() => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Removes every entry whose key starts with `prefix`, returning the
    /// number removed — pyfly's `evict_by_prefix` (relied on by
    /// transactional persistence and CQRS invalidation).
    ///
    /// The default returns [`CacheError::Backend`] (`"delete_prefix
    /// unsupported"`): a port cannot enumerate arbitrary keys without
    /// backend support. The in-process [`MemoryAdapter`](crate::MemoryAdapter)
    /// and the Redis adapter (`SCAN MATCH`) override it.
    async fn delete_prefix(&self, _prefix: &str) -> Result<u64, CacheError> {
        Err(CacheError::Backend(
            "delete_prefix unsupported by this adapter".to_owned(),
        ))
    }

    /// Returns a [`CacheStats`] snapshot, or `None` when the adapter does
    /// not expose counters — pyfly's `get_stats`. The default returns
    /// `None`; counter-tracking backends override it.
    async fn stats(&self) -> Option<CacheStats> {
        None
    }
}
