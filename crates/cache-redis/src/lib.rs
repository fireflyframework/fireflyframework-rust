//! firefly-cache-redis — a Redis-backed [`firefly_cache::Adapter`].
//!
//! [`RedisAdapter`] is the Rust port of pyfly's `RedisCacheAdapter`: it
//! implements the full Firefly cache port (`get` / `set` / `delete` /
//! `clear` / `set_if_absent` / `exists` / `delete_prefix` / `stats` /
//! `health_check`) over the [`redis`] crate, using the native Redis verbs
//! pyfly's adapter relies on:
//!
//! | Port method      | Redis command(s)                          |
//! |------------------|-------------------------------------------|
//! | `get`            | `GET`                                     |
//! | `set`            | `SET key value [PX ttl]`                  |
//! | `set_if_absent`  | `SET key value [PX ttl] NX`               |
//! | `delete`         | `DEL`                                     |
//! | `exists`         | `EXISTS`                                  |
//! | `delete_prefix`  | `SCAN MATCH <prefix>* ` loop + `DEL`      |
//! | `clear`          | `FLUSHDB`                                 |
//! | `stats`          | `DBSIZE` + in-process hit/miss counters   |
//! | `health_check`   | `PING`                                    |
//!
//! Unlike pyfly — whose adapter is handed an already-connected
//! `redis.asyncio.Redis` client and has explicit `start()`/`stop()`
//! lifecycle hooks — [`RedisAdapter`] takes a connection **URL** (or a
//! pre-built [`redis::aio::MultiplexedConnection`]) and establishes the
//! multiplexed connection lazily on first use, matching the rest of the
//! Rust port's adapter-crate convention (cf. `firefly-eda-redis`). There is
//! no `start`/`stop`: construction is connection setup and `Drop` is
//! teardown.
//!
//! Values cross the [`firefly_cache::Adapter`] port as raw bytes, so the
//! JSON encoding lives in [`firefly_cache::Typed`] exactly as for the
//! in-process [`firefly_cache::MemoryAdapter`] — the adapter itself is
//! byte-transparent and therefore wire-compatible with every sibling port.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::time::Duration;
//! use firefly_cache::{Adapter, Typed};
//! use firefly_cache_redis::RedisAdapter;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let adapter = Arc::new(RedisAdapter::connect("redis://127.0.0.1:6379/0").await?);
//! adapter.set("k", b"v", Some(Duration::from_secs(60))).await?;
//! assert_eq!(adapter.get("k").await?, b"v");
//! # Ok(())
//! # }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use firefly_cache::{Adapter, CacheError, CacheStats};
use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, Client, ExistenceCheck, SetExpiry, SetOptions};
use tokio::sync::Mutex;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// A [`firefly_cache::Adapter`] backed by a single Redis logical database.
///
/// See the [crate docs](crate) for the command mapping. The adapter holds a
/// cloneable [`MultiplexedConnection`] behind a [`Mutex`] so concurrent
/// callers serialize their pipelined requests over the one connection
/// (multiplexed connections are cheap to share this way). Hit/miss/eviction
/// counters are kept in-process (atomic), exactly like pyfly's adapter —
/// Redis itself does not expose per-adapter hit counters.
#[derive(Debug)]
pub struct RedisAdapter {
    conn: Mutex<MultiplexedConnection>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl RedisAdapter {
    /// Connects to Redis at `url` (e.g. `redis://127.0.0.1:6379/0`) and
    /// returns a ready adapter.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] if the URL is malformed or the
    /// initial multiplexed connection cannot be established.
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let client = Client::open(url).map_err(backend_err)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(backend_err)?;
        Ok(Self::from_connection(conn))
    }

    /// Wraps an already-established [`MultiplexedConnection`] — the
    /// dependency-injection entry point, paralleling pyfly's
    /// `RedisCacheAdapter(client)`.
    #[must_use]
    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self {
            conn: Mutex::new(conn),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Returns up to `limit` keys matching the glob-style `pattern` via a
    /// cursor-driven `SCAN MATCH` loop — pyfly's `get_keys(pattern,
    /// limit)`. The scan stops as soon as `limit` keys are collected; a
    /// `limit` of `0` returns no keys.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::Backend`] on a transport failure.
    pub async fn keys(&self, pattern: &str, limit: usize) -> Result<Vec<String>, CacheError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut conn = self.conn.lock().await;
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(limit)
                .query_async(&mut *conn)
                .await
                .map_err(backend_err)?;
            for k in batch {
                keys.push(k);
                if keys.len() >= limit {
                    return Ok(keys);
                }
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(keys)
    }

    /// Reports whether Redis answers `PING` — pyfly's `is_available()`.
    /// Unlike [`Adapter::health_check`], a failure is reported as `Ok(false)`
    /// rather than an error, so callers can degrade gracefully (pyfly's
    /// fail-soft `is_available`).
    pub async fn is_available(&self) -> bool {
        self.ping().await.is_ok()
    }

    /// Issues `PING`, returning the transport error on failure.
    async fn ping(&self) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        redis::cmd("PING")
            .query_async::<()>(&mut *conn)
            .await
            .map_err(backend_err)
    }
}

#[async_trait]
impl Adapter for RedisAdapter {
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        let mut conn = self.conn.lock().await;
        let raw: Option<Vec<u8>> = conn.get(key).await.map_err(backend_err)?;
        match raw {
            Some(bytes) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Ok(bytes)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                Err(CacheError::NotFound)
            }
        }
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        match px_millis(ttl) {
            // `SET key value PX <ms>`.
            Some(ms) => {
                let opts = SetOptions::default().with_expiration(SetExpiry::PX(ms));
                conn.set_options::<_, _, ()>(key, value, opts)
                    .await
                    .map_err(backend_err)
            }
            // `SET key value` (no expiry).
            None => conn.set::<_, _, ()>(key, value).await.map_err(backend_err),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        let removed: i64 = conn.del(key).await.map_err(backend_err)?;
        if removed > 0 {
            self.evictions.fetch_add(removed as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn clear(&self) -> Result<(), CacheError> {
        let mut conn = self.conn.lock().await;
        redis::cmd("FLUSHDB")
            .query_async::<()>(&mut *conn)
            .await
            .map_err(backend_err)
    }

    fn name(&self) -> String {
        "redis".to_owned()
    }

    async fn health_check(&self) -> Result<(), CacheError> {
        self.ping().await
    }

    /// `SET key value [PX <ms>] NX` — the native atomic conditional write
    /// pyfly's `put_if_absent` uses. Returns `true` when the key was set
    /// (it was absent), `false` when it already existed (Redis answers nil).
    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, CacheError> {
        let mut conn = self.conn.lock().await;
        let mut opts = SetOptions::default().conditional_set(ExistenceCheck::NX);
        if let Some(ms) = px_millis(ttl) {
            opts = opts.with_expiration(SetExpiry::PX(ms));
        }
        // On NX failure Redis replies with a nil bulk, deserialized to
        // `None`; on success it replies `+OK`, deserialized to `Some(_)`.
        let set: Option<String> = conn
            .set_options(key, value, opts)
            .await
            .map_err(backend_err)?;
        Ok(set.is_some())
    }

    /// `EXISTS key` — pyfly's `exists`.
    async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        let mut conn = self.conn.lock().await;
        let count: i64 = conn.exists(key).await.map_err(backend_err)?;
        Ok(count > 0)
    }

    /// Cursor-driven `SCAN MATCH <prefix>*` collecting every matching key,
    /// then a single `DEL` of the batch — pyfly's `evict_by_prefix`. Returns
    /// the number of keys removed.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError> {
        let pattern = format!("{}*", glob_escape(prefix));
        let mut conn = self.conn.lock().await;
        let mut matched: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut *conn)
                .await
                .map_err(backend_err)?;
            matched.extend(batch);
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        if matched.is_empty() {
            return Ok(0);
        }
        let removed: i64 = conn.del(&matched).await.map_err(backend_err)?;
        let removed = removed.max(0) as u64;
        self.evictions.fetch_add(removed, Ordering::Relaxed);
        Ok(removed)
    }

    /// `DBSIZE` for `size`, plus the in-process hit/miss/eviction counters —
    /// pyfly's `get_stats`.
    async fn stats(&self) -> Option<CacheStats> {
        let size: i64 = {
            let mut conn = self.conn.lock().await;
            redis::cmd("DBSIZE").query_async(&mut *conn).await.ok()?
        };
        Some(CacheStats::from_counters(
            size.max(0) as u64,
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
            self.evictions.load(Ordering::Relaxed),
        ))
    }
}

/// Converts an optional TTL into whole milliseconds for `SET … PX`. A
/// `None` or zero duration means no expiry (`None`), matching the
/// `firefly_cache` contract (`ttl <= 0` => persistent). Sub-millisecond
/// TTLs round up to `1` so a positive TTL never silently becomes
/// persistent.
fn px_millis(ttl: Option<Duration>) -> Option<u64> {
    let d = ttl?;
    if d.is_zero() {
        return None;
    }
    Some(d.as_millis().max(1).min(u128::from(u64::MAX)) as u64)
}

/// Escapes the Redis glob metacharacters (`*`, `?`, `[`, `]`, `\`) in a
/// literal key prefix so `delete_prefix("a*b")` only matches keys that
/// literally begin with `a*b`, not any key starting with `a`. The trailing
/// `*` wildcard is appended by the caller after escaping.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Wraps a [`redis::RedisError`] as the cache port's
/// [`CacheError::Backend`].
fn backend_err(e: redis::RedisError) -> CacheError {
    CacheError::Backend(format!("firefly/cache-redis: {e}"))
}
