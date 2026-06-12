use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::adapter::{Adapter, CacheError, CacheStats};

/// In-process [`Adapter`] backed by a map with per-entry TTLs and optional
/// LRU bounding. Suitable for development, tests, and single-instance
/// deployments. The Rust port of pyfly's `InMemoryCache(max_size)`.
///
/// Entries are evicted lazily: an expired entry is removed the first time a
/// `get` observes it past its deadline. Reads hand out an owned copy of the
/// stored bytes, so callers can never mutate cached state (copy-on-read).
///
/// # LRU bounding
///
/// Constructed with [`MemoryAdapter::with_max_entries`], the cache holds at
/// most `max_entries` entries and evicts the least-recently-*used* entry on
/// overflow — every successful `get` and every `set` marks its key as
/// most-recently-used, exactly as pyfly's `OrderedDict.move_to_end`. The
/// default ([`MemoryAdapter::new`]) is unbounded: rely on TTLs to bound
/// memory.
///
/// # Statistics
///
/// The adapter tracks cumulative hits, misses, and evictions in atomic
/// counters surfaced via [`Adapter::stats`] — pyfly's `get_stats()`. A miss
/// is a read that found nothing or an expired entry; an eviction is any
/// removal (explicit `delete`, `delete_prefix`, or LRU/TTL overflow).
#[derive(Debug, Default)]
pub struct MemoryAdapter {
    entries: RwLock<HashMap<String, MemEntry>>,
    /// `Some(n)` bounds the cache to `n` entries with LRU eviction; `None`
    /// is unbounded.
    max_entries: Option<usize>,
    /// Monotonic logical clock stamped onto an entry on every access, so
    /// the lowest-stamped live entry is the LRU eviction victim.
    access_seq: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

#[derive(Debug)]
struct MemEntry {
    value: Vec<u8>,
    exp: Option<Instant>,
    /// Access stamp from [`MemoryAdapter::access_seq`]; the smallest among
    /// live entries identifies the LRU victim.
    last_access: u64,
}

impl MemEntry {
    /// Reports whether the entry's TTL deadline has passed.
    fn expired(&self) -> bool {
        matches!(self.exp, Some(exp) if Instant::now() > exp)
    }
}

impl MemoryAdapter {
    /// Returns an empty, unbounded memory adapter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an empty memory adapter bounded to at most `max_entries`
    /// live entries, evicting the least-recently-used entry on overflow —
    /// pyfly's `InMemoryCache(max_size=…)`.
    ///
    /// A `max_entries` of `0` is treated as "unbounded" rather than "drop
    /// every write", matching the practical reading of pyfly's
    /// `while len > max_size` loop never firing for a non-positive bound.
    #[must_use]
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            max_entries: (max_entries > 0).then_some(max_entries),
            ..Self::default()
        }
    }

    /// Returns the LRU bound, or `None` when unbounded — pyfly's
    /// `max_size`.
    #[must_use]
    pub fn max_entries(&self) -> Option<usize> {
        self.max_entries
    }

    /// Returns the number of entries (including expired-but-uncollected).
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Reports whether the adapter holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }

    /// Returns the keys of every live (non-expired) entry — pyfly's
    /// `get_keys()`. Order is unspecified.
    pub async fn keys(&self) -> Vec<String> {
        let entries = self.entries.read().await;
        entries
            .iter()
            .filter(|(_, e)| !e.expired())
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Next access stamp.
    fn next_seq(&self) -> u64 {
        self.access_seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Evicts least-recently-used entries while the live count exceeds the
    /// bound. Caller must hold the write guard. Counts each eviction.
    fn enforce_bound(&self, entries: &mut HashMap<String, MemEntry>) {
        let Some(max) = self.max_entries else { return };
        while entries.len() > max {
            // Find the live entry with the smallest access stamp.
            let victim = entries
                .iter()
                .min_by_key(|(_, e)| e.last_access)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    entries.remove(&k);
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None => break,
            }
        }
    }
}

#[async_trait]
impl Adapter for MemoryAdapter {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        // Fast path under the write lock so a hit can update its LRU stamp.
        // A read-only fast path would not be able to record recency, which
        // pyfly does on every successful get (move_to_end).
        let seq = self.next_seq();
        let mut entries = self.entries.write().await;
        match entries.get_mut(key) {
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Err(CacheError::NotFound)
            }
            Some(e) if !e.expired() => {
                e.last_access = seq;
                self.hits.fetch_add(1, Ordering::Relaxed);
                // Copy-on-read so callers can't mutate stored bytes.
                Ok(e.value.clone())
            }
            Some(_) => {
                // Expired: lazy eviction. A removal here counts as an
                // eviction and the read as a miss, matching pyfly.
                entries.remove(key);
                self.evictions.fetch_add(1, Ordering::Relaxed);
                self.misses.fetch_add(1, Ordering::Relaxed);
                Err(CacheError::NotFound)
            }
        }
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError> {
        let exp = ttl.filter(|d| !d.is_zero()).map(|d| Instant::now() + d);
        let seq = self.next_seq();
        let mut entries = self.entries.write().await;
        entries.insert(
            key.to_owned(),
            MemEntry {
                value: value.to_vec(),
                exp,
                last_access: seq,
            },
        );
        self.enforce_bound(&mut entries);
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        if self.entries.write().await.remove(key).is_some() {
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
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

    /// Native `SET NX`: stores `value` only when `key` is absent or
    /// expired, under a single write-lock acquisition so the check and the
    /// write are atomic with respect to other adapter callers — pyfly's
    /// "atomic under asyncio" `put_if_absent`.
    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, CacheError> {
        let seq = self.next_seq();
        let mut entries = self.entries.write().await;
        if let Some(e) = entries.get(key) {
            if !e.expired() {
                return Ok(false);
            }
            // An expired entry is treated as absent and overwritten.
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        let exp = ttl.filter(|d| !d.is_zero()).map(|d| Instant::now() + d);
        entries.insert(
            key.to_owned(),
            MemEntry {
                value: value.to_vec(),
                exp,
                last_access: seq,
            },
        );
        self.enforce_bound(&mut entries);
        Ok(true)
    }

    /// Native existence check: reports whether a live entry exists, lazily
    /// evicting an observed-expired entry (pyfly's `exists`).
    async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        let mut entries = self.entries.write().await;
        match entries.get(key) {
            None => Ok(false),
            Some(e) if !e.expired() => Ok(true),
            Some(_) => {
                entries.remove(key);
                self.evictions.fetch_add(1, Ordering::Relaxed);
                Ok(false)
            }
        }
    }

    /// Native prefix eviction: removes every key starting with `prefix` and
    /// returns the count — pyfly's `evict_by_prefix`.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError> {
        let mut entries = self.entries.write().await;
        let matches: Vec<String> = entries
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let removed = matches.len() as u64;
        for k in matches {
            entries.remove(&k);
        }
        self.evictions.fetch_add(removed, Ordering::Relaxed);
        Ok(removed)
    }

    /// Returns a counter snapshot. `size` counts only live (non-expired)
    /// entries, matching pyfly's `get_stats()["size"]`.
    async fn stats(&self) -> Option<CacheStats> {
        let entries = self.entries.read().await;
        let size = entries.values().filter(|e| !e.expired()).count() as u64;
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let evictions = self.evictions.load(Ordering::Relaxed);
        Some(CacheStats::from_counters(size, hits, misses, evictions))
    }
}
