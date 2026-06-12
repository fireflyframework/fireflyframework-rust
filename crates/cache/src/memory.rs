use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::adapter::{Adapter, CacheError};

/// In-process [`Adapter`] backed by a map with per-entry TTLs. Suitable for
/// development, tests, and single-instance deployments.
///
/// Entries are evicted lazily: an expired entry is removed the first time a
/// `get` observes it past its deadline. Reads hand out an owned copy of the
/// stored bytes, so callers can never mutate cached state (copy-on-read).
#[derive(Debug, Default)]
pub struct MemoryAdapter {
    entries: RwLock<HashMap<String, MemEntry>>,
}

#[derive(Debug)]
struct MemEntry {
    value: Vec<u8>,
    exp: Option<Instant>,
}

impl MemoryAdapter {
    /// Returns an empty memory adapter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of entries (including expired-but-uncollected).
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Reports whether the adapter holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }
}

#[async_trait]
impl Adapter for MemoryAdapter {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        {
            let entries = self.entries.read().await;
            match entries.get(key) {
                None => return Err(CacheError::NotFound),
                Some(e) => match e.exp {
                    // Expired — fall through to the lazy eviction below.
                    Some(exp) if Instant::now() > exp => {}
                    // Copy-on-read so callers can't mutate stored bytes.
                    _ => return Ok(e.value.clone()),
                },
            }
        }
        self.entries.write().await.remove(key);
        Err(CacheError::NotFound)
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError> {
        let exp = ttl.filter(|d| !d.is_zero()).map(|d| Instant::now() + d);
        self.entries.write().await.insert(
            key.to_owned(),
            MemEntry {
                value: value.to_vec(),
                exp,
            },
        );
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        self.entries.write().await.remove(key);
        Ok(())
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.entries.write().await.clear();
        Ok(())
    }

    fn name(&self) -> String {
        "memory".to_owned()
    }

    /// The memory adapter is always healthy.
    async fn health_check(&self) -> Result<(), CacheError> {
        Ok(())
    }
}
