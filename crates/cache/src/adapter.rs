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

/// The canonical Firefly cache port. Implementations must be safe for
/// concurrent use (`Send + Sync`), and the trait is object-safe so adapters
/// compose behind `Arc<dyn Adapter>`.
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
}
