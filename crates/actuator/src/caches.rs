//! `GET /actuator/caches` (+ `/{name}` drill-down and
//! `POST /{name}/evict`) — Spring Boot parity via a local [`CacheOps`]
//! trait so the actuator stays decoupled from `firefly-cache` (the
//! starter bridges the two).

use async_trait::async_trait;

/// The cache-manager name reported in the wire shape — pyfly hardwires
/// Spring's conventional `cacheManager`.
pub(crate) const CACHE_MANAGER: &str = "cacheManager";

/// One cache reported on `/actuator/caches`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDescriptor {
    /// The cache name (pyfly reports the configured provider name).
    pub name: String,
    /// The backing implementation, e.g. `firefly_cache::MemoryAdapter`.
    pub target: String,
}

/// Cache operations consulted by the `/actuator/caches` endpoint.
/// Implemented by the cache integration (e.g. a starter bridging
/// `firefly-cache`) so this crate carries no cache dependency.
#[async_trait]
pub trait CacheOps: Send + Sync {
    /// Snapshot of the configured caches.
    fn caches(&self) -> Vec<CacheDescriptor>;

    /// Clears the named cache. Returns `false` when no such cache
    /// exists (rendered as 404 by `POST /actuator/caches/{name}/evict`).
    async fn evict(&self, name: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeCaches {
        evictions: AtomicUsize,
    }

    #[async_trait]
    impl CacheOps for FakeCaches {
        fn caches(&self) -> Vec<CacheDescriptor> {
            vec![CacheDescriptor {
                name: "default".into(),
                target: "firefly_cache::MemoryAdapter".into(),
            }]
        }

        async fn evict(&self, name: &str) -> bool {
            if name == "default" {
                self.evictions.fetch_add(1, Ordering::SeqCst);
                true
            } else {
                false
            }
        }
    }

    #[tokio::test]
    async fn evict_reports_unknown_cache() {
        let ops = FakeCaches {
            evictions: AtomicUsize::new(0),
        };
        assert!(ops.evict("default").await);
        assert!(!ops.evict("nope").await);
        assert_eq!(ops.evictions.load(Ordering::SeqCst), 1);
    }
}
