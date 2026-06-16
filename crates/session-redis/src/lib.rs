// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-session-redis — a Redis-backed [`firefly_session::SessionRegistry`].
//!
//! [`RedisSessionRegistry`] is the Rust port of pyfly's `RedisSessionRegistry`
//! (`pyfly.session.adapters.redis_registry`). It is a **distributed**, shared
//! per-principal index of live sessions: every application instance reads and
//! writes the same Redis keys, so the per-principal concurrency cap enforced by
//! [`firefly_session::SessionConcurrencyController`] holds across the whole
//! cluster — not just within one process (the limit of the in-process
//! [`firefly_session::MemorySessionRegistry`]).
//!
//! # Data model
//!
//! Each principal's live sessions are a single Redis **sorted set** keyed
//! `firefly:session:user:<principal>` (the [`DEFAULT_KEY_PREFIX`] plus the
//! principal). The sorted-set *score* is the session's `created_at`
//! (epoch-millis) and the *member* is the session id:
//!
//! | [`SessionRegistry`] method | Redis command(s)                              |
//! |----------------------------|-----------------------------------------------|
//! | `register`                 | `ZADD key <created_at> <session_id>` + `EXPIRE key <ttl>` |
//! | `deregister`               | `ZREM key <session_id>`                       |
//! | `list_sessions`            | `ZRANGE key 0 -1 WITHSCORES` (ascending → oldest-first) |
//! | `count`                    | `ZCARD key`                                   |
//!
//! Storing `created_at` as the score means [`SessionRegistry::list_sessions`]
//! is naturally **oldest-first** (`ZRANGE` is ascending) with no client-side
//! sort, and `deregister` of the last member leaves an empty set that Redis
//! drops automatically — so a principal's key disappears once they have no live
//! sessions, exactly like pyfly's adapter and the in-process registry's bucket
//! pruning.
//!
//! Although the task summary describes the index as a Redis *set*
//! (`SADD`/`SMEMBERS`/`SREM`), a plain set cannot record each session's
//! `created_at` nor return entries oldest-first — both of which the
//! [`SessionRegistry`] contract requires for the evict-oldest strategy — so a
//! **sorted set** (score = `created_at`) is used. This is faithful to pyfly's
//! actual implementation, which uses `ZADD`/`ZRANGE`/`ZREM`/`ZCARD`.
//!
//! # TTL — bounding orphan growth
//!
//! `register` slides an `EXPIRE` on the principal's key (default
//! [`DEFAULT_TTL_SECS`], 24h) on every login. This bounds the growth of
//! orphaned index entries (e.g. a crashed instance that never deregistered):
//! if a principal stops logging in entirely, their stale index self-expires
//! rather than lingering forever. The TTL slides forward on each `register`,
//! so an actively-used principal's index never expires out from under them.
//!
//! # Lifecycle
//!
//! Unlike pyfly — whose registry is handed an already-connected
//! `redis.asyncio.Redis` client — [`RedisSessionRegistry`] takes a connection
//! **URL** (or a pre-built [`redis::aio::MultiplexedConnection`]) and
//! establishes the multiplexed connection lazily on first use, matching the
//! rest of the Rust port's adapter-crate convention (cf. `firefly-cache-redis`,
//! `firefly-eda-redis`). There is no `start`/`stop`: construction is connection
//! setup and `Drop` is teardown.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use firefly_session::{SessionRegistry, SessionConcurrencyController, ConcurrencyPolicy, Strategy};
//! use firefly_session_redis::RedisSessionRegistry;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = Arc::new(RedisSessionRegistry::connect("redis://127.0.0.1:6379/0").await?);
//! // Plug the distributed registry into the cluster-wide concurrency cap.
//! let controller = SessionConcurrencyController::new(
//!     registry.clone(),
//!     ConcurrencyPolicy { max_sessions: 2, strategy: Strategy::EvictOldest },
//! );
//! controller.on_login("alice", "session-1", 1_700_000_000_000).await;
//! assert_eq!(registry.count("alice").await, 1);
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use firefly_session::SessionRegistry;
use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, Client};
use tokio::sync::Mutex;

/// Framework version stamp.
pub const VERSION: &str = "26.6.15";

/// The default key prefix for a principal's session sorted set — pyfly's
/// `pyfly:session:user:` under the Rust framework's `firefly:` namespace. The
/// full key is `<prefix><principal>`, e.g. `firefly:session:user:alice`.
pub const DEFAULT_KEY_PREFIX: &str = "firefly:session:user:";

/// The default sliding TTL (seconds) applied to a principal's session index on
/// every [`register`](SessionRegistry::register) — 24 hours, matching pyfly's
/// `86400`. Bounds orphan growth without ever expiring an active principal's
/// index (the TTL slides forward on each login).
pub const DEFAULT_TTL_SECS: i64 = 86_400;

/// A distributed [`firefly_session::SessionRegistry`] backed by Redis sorted
/// sets — one sorted set per principal, shared by every application instance.
///
/// See the [crate docs](crate) for the data model and command mapping. The
/// registry holds a cloneable [`MultiplexedConnection`] behind a [`Mutex`] so
/// concurrent callers serialize their pipelined requests over the one
/// connection (multiplexed connections are cheap to share this way), exactly
/// like `firefly-cache-redis`.
#[derive(Debug)]
pub struct RedisSessionRegistry {
    conn: Mutex<MultiplexedConnection>,
    key_prefix: String,
    ttl_secs: i64,
}

impl RedisSessionRegistry {
    /// Connects to Redis at `url` (e.g. `redis://127.0.0.1:6379/0`) and returns
    /// a ready registry using the default [`DEFAULT_KEY_PREFIX`] and
    /// [`DEFAULT_TTL_SECS`].
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] if the URL is malformed or the
    /// initial multiplexed connection cannot be established.
    pub async fn connect(url: &str) -> Result<Self, RegistryError> {
        Self::connect_with(url, DEFAULT_KEY_PREFIX, DEFAULT_TTL_SECS).await
    }

    /// Like [`connect`](RedisSessionRegistry::connect) but with a custom key
    /// `prefix` and sliding `ttl_secs`. A `ttl_secs <= 0` disables the
    /// per-principal expiry entirely (the index then only shrinks via
    /// [`deregister`](SessionRegistry::deregister)).
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] if the URL is malformed or the
    /// initial multiplexed connection cannot be established.
    pub async fn connect_with(
        url: &str,
        prefix: &str,
        ttl_secs: i64,
    ) -> Result<Self, RegistryError> {
        let client = Client::open(url).map_err(backend_err)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(backend_err)?;
        Ok(Self::from_connection_with(conn, prefix, ttl_secs))
    }

    /// Wraps an already-established [`MultiplexedConnection`] with the default
    /// prefix and TTL — the dependency-injection entry point, paralleling
    /// pyfly's `RedisSessionRegistry(client)`.
    #[must_use]
    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self::from_connection_with(conn, DEFAULT_KEY_PREFIX, DEFAULT_TTL_SECS)
    }

    /// Wraps an already-established [`MultiplexedConnection`] with a custom key
    /// `prefix` and sliding `ttl_secs` (a `ttl_secs <= 0` disables expiry).
    #[must_use]
    pub fn from_connection_with(conn: MultiplexedConnection, prefix: &str, ttl_secs: i64) -> Self {
        Self {
            conn: Mutex::new(conn),
            key_prefix: prefix.to_owned(),
            ttl_secs,
        }
    }

    /// The key prefix this registry uses (the default [`DEFAULT_KEY_PREFIX`]
    /// unless built with a `_with` constructor).
    #[must_use]
    pub fn key_prefix(&self) -> &str {
        &self.key_prefix
    }

    /// The sliding TTL (seconds) applied on each `register` (the default
    /// [`DEFAULT_TTL_SECS`] unless built with a `_with` constructor); `<= 0`
    /// means no expiry.
    #[must_use]
    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    /// The Redis key holding `principal`'s session sorted set.
    #[must_use]
    pub fn key(&self, principal: &str) -> String {
        principal_key(&self.key_prefix, principal)
    }

    /// Reports whether Redis answers `PING` — the fail-soft health probe. A
    /// transport failure is reported as `false` rather than an error so callers
    /// can degrade gracefully.
    pub async fn is_available(&self) -> bool {
        let mut conn = self.conn.lock().await;
        redis::cmd("PING")
            .query_async::<()>(&mut *conn)
            .await
            .is_ok()
    }
}

#[async_trait]
impl SessionRegistry for RedisSessionRegistry {
    /// `ZADD key <created_at> <session_id>` then a sliding `EXPIRE key <ttl>` —
    /// pyfly's `zadd` + `expire`. A backend failure is logged and swallowed
    /// (the [`SessionRegistry`] trait is infallible by contract); the
    /// concurrency cap simply isn't enforced for this login rather than the
    /// login failing.
    async fn register(&self, principal: &str, session_id: &str, created_at: i64) {
        let key = self.key(principal);
        // Pipeline ZADD + the sliding EXPIRE into a single round-trip so the
        // connection lock is never held across two awaits (which would
        // serialize unrelated principals' logins behind one another's network
        // latency). ZADD's score is the creation time so ZRANGE stays
        // oldest-first; the EXPIRE slides the orphan-bounding TTL forward on
        // every login (only when a positive TTL is configured).
        let mut pipe = redis::pipe();
        pipe.zadd(&key, session_id, created_at).ignore();
        if self.ttl_secs > 0 {
            pipe.expire(&key, self.ttl_secs).ignore();
        }
        let mut conn = self.conn.lock().await;
        if let Err(e) = pipe.query_async::<()>(&mut *conn).await {
            tracing::warn!(principal, session_id, error = %e, "session-redis: register pipeline (ZADD/EXPIRE) failed; concurrency cap not enforced for this login");
        }
    }

    /// `ZREM key <session_id>` — pyfly's `zrem`. Removing the last member
    /// leaves an empty sorted set, which Redis deletes automatically (so the
    /// principal's key disappears, matching the in-process registry's bucket
    /// pruning). Idempotent and infallible by contract; a backend failure is
    /// logged and swallowed.
    async fn deregister(&self, principal: &str, session_id: &str) {
        let key = self.key(principal);
        let mut conn = self.conn.lock().await;
        if let Err(e) = conn.zrem::<_, _, ()>(&key, session_id).await {
            tracing::warn!(principal, session_id, error = %e, "session-redis: ZREM failed");
        }
    }

    /// `ZRANGE key 0 -1 WITHSCORES` — pyfly's `zrange(..., withscores=True)`.
    /// `ZRANGE` is ascending, so members come back **oldest-first** (lowest
    /// score = earliest `created_at`) with no client-side sort. The score is
    /// returned as the session's epoch-millis `created_at`. A backend failure
    /// is logged and yields an empty list (the trait is infallible).
    async fn list_sessions(&self, principal: &str) -> Vec<(String, i64)> {
        let key = self.key(principal);
        let mut conn = self.conn.lock().await;
        // Redis returns scores as floating-point bulk strings; the redis crate
        // deserialises them to f64, so collect (member, score) pairs and cast
        // the score back to the i64 epoch-millis we stored.
        let raw: Vec<(String, f64)> = match conn.zrange_withscores(&key, 0, -1).await {
            Ok(pairs) => pairs,
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-redis: ZRANGE failed");
                return Vec::new();
            }
        };
        raw.into_iter()
            .map(|(member, score)| (member, score as i64))
            .collect()
    }

    /// `ZCARD key` — pyfly's `zcard`. The number of live sessions for
    /// `principal`. A backend failure is logged and yields `0`.
    async fn count(&self, principal: &str) -> usize {
        let key = self.key(principal);
        let mut conn = self.conn.lock().await;
        match conn.zcard::<_, i64>(&key).await {
            Ok(n) => n.max(0) as usize,
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-redis: ZCARD failed");
                0
            }
        }
    }
}

/// The error type surfaced by [`RedisSessionRegistry`]'s **constructors**
/// (connection setup). The [`SessionRegistry`] trait methods themselves are
/// infallible by contract, so a per-operation Redis failure there is logged and
/// swallowed rather than returned (see each method's docs); this type only
/// reports failures establishing the initial connection.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// A Redis transport / protocol error (malformed URL, connection refused,
    /// auth failure, …).
    #[error("firefly/session-redis backend error: {0}")]
    Backend(String),
}

/// Joins a key `prefix` and `principal` into the Redis key holding that
/// principal's session sorted set, e.g. `firefly:session:user:alice`. Kept as a
/// free function so the (pure) key-formatting logic is unit-testable without a
/// live connection.
#[must_use]
fn principal_key(prefix: &str, principal: &str) -> String {
    format!("{prefix}{principal}")
}

/// Wraps a [`redis::RedisError`] as [`RegistryError::Backend`].
fn backend_err(e: redis::RedisError) -> RegistryError {
    RegistryError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_joins_prefix_and_principal() {
        assert_eq!(
            principal_key(DEFAULT_KEY_PREFIX, "alice"),
            "firefly:session:user:alice"
        );
        assert_eq!(principal_key("app:sess:", "bob"), "app:sess:bob");
    }

    #[test]
    fn defaults_match_pyfly() {
        assert_eq!(DEFAULT_KEY_PREFIX, "firefly:session:user:");
        assert_eq!(DEFAULT_TTL_SECS, 86_400);
    }

    #[test]
    fn registry_is_object_safe_and_send_sync() {
        // The registry must compose behind `Arc<dyn SessionRegistry>` (that is
        // how `SessionConcurrencyController` holds it) — naming the type proves
        // the trait is object-safe. No live connection is needed.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RedisSessionRegistry>();
        let _erased: Option<std::sync::Arc<dyn SessionRegistry>> = None;
    }

    #[test]
    fn backend_err_wraps_message() {
        let e: redis::RedisError = redis::RedisError::from((redis::ErrorKind::IoError, "boom"));
        let wrapped = backend_err(e);
        assert!(matches!(wrapped, RegistryError::Backend(_)));
        assert!(wrapped.to_string().contains("session-redis"));
    }
}
