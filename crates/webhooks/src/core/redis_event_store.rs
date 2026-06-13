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

//! Redis-backed webhook idempotency [`EventStore`] — the Rust spelling of
//! pyfly's `RedisWebhookEventStore`.
//!
//! Where [`MemoryEventStore`](crate::MemoryEventStore) keeps seen keys in a
//! single process for its lifetime, [`RedisEventStore`] persists each
//! idempotency key as a TTL-expiring Redis string, so a webhook redelivered
//! to a **different** instance is recognised as a duplicate and the store
//! self-prunes without a background job. This is the production analog for
//! multi-instance deployments that pyfly's in-memory store could not cover.
//!
//! | [`EventStore`] method | Redis command(s)                                   |
//! |-----------------------|----------------------------------------------------|
//! | `already_processed`   | `EXISTS <prefix><key>`                             |
//! | `remember`            | `SET <prefix><key> "1" EX <ttl>` (pyfly's `set ex`) |
//!
//! The key prefix (default `webhook:idem:`) and TTL (default 24 h) match
//! pyfly's `RedisWebhookEventStore` defaults exactly, so keys written by the
//! Python and Rust runtimes are wire-compatible against the same Redis.
//!
//! Like pyfly, the check-then-set pair (`already_processed` then `remember`)
//! is **not** atomic; for the overwhelming majority of webhook workloads the
//! window is negligible. Callers needing once-exactly semantics should
//! serialise the pair behind a distributed lock.
//!
//! Enable with the `redis` cargo feature.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), firefly_webhooks::WebhookError> {
//! use std::sync::Arc;
//!
//! use firefly_webhooks::{EventStore, Pipeline, MemoryDlq};
//! use firefly_webhooks::RedisEventStore;
//!
//! let store = RedisEventStore::connect("redis://127.0.0.1:6379/0").await?;
//! let pipeline = Pipeline::new(Arc::new(MemoryDlq::new()));
//! pipeline.register_event_store_arc(Arc::new(store));
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, Client, SetExpiry, SetOptions};
use tokio::sync::Mutex;

use crate::core::EventStore;
use crate::error::WebhookError;

/// The default Redis key prefix for stored idempotency keys —
/// pyfly's `RedisWebhookEventStore` `key_prefix` default.
pub const DEFAULT_KEY_PREFIX: &str = "webhook:idem:";

/// The default per-key TTL in seconds (24 h) — pyfly's
/// `RedisWebhookEventStore` `ttl_seconds` default.
pub const DEFAULT_TTL_SECONDS: u64 = 86_400;

/// A distributed, durable webhook idempotency [`EventStore`] backed by
/// Redis — the Rust port of pyfly's `RedisWebhookEventStore`.
///
/// Keys are stored as plain `SET … EX` strings under the configured
/// [`prefix`](RedisEventStore::with_key_prefix) so the store self-prunes via
/// TTL. See the [module docs](self) for the command mapping and defaults.
///
/// The adapter holds a cloneable [`MultiplexedConnection`] behind a
/// [`Mutex`] (the same convention as `firefly-cache-redis`): concurrent
/// callers serialise their pipelined requests over the one connection, which
/// is cheap for multiplexed connections.
#[derive(Debug)]
pub struct RedisEventStore {
    conn: Mutex<MultiplexedConnection>,
    prefix: String,
    ttl_seconds: u64,
}

impl RedisEventStore {
    /// Connects to Redis at `url` (e.g. `redis://127.0.0.1:6379/0`) and
    /// returns a ready store using the default prefix
    /// ([`DEFAULT_KEY_PREFIX`]) and TTL ([`DEFAULT_TTL_SECONDS`]).
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::Backend`] when the URL is malformed or the
    /// initial multiplexed connection cannot be established.
    pub async fn connect(url: &str) -> Result<Self, WebhookError> {
        let client = Client::open(url).map_err(backend_err)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(backend_err)?;
        Ok(Self::from_connection(conn))
    }

    /// Wraps an already-established [`MultiplexedConnection`] — the
    /// dependency-injection entry point, paralleling pyfly's
    /// `RedisWebhookEventStore(redis_client)`.
    #[must_use]
    pub fn from_connection(conn: MultiplexedConnection) -> Self {
        Self {
            conn: Mutex::new(conn),
            prefix: DEFAULT_KEY_PREFIX.to_owned(),
            ttl_seconds: DEFAULT_TTL_SECONDS,
        }
    }

    /// Overrides the Redis key prefix (default [`DEFAULT_KEY_PREFIX`]),
    /// builder-style — pyfly's `key_prefix` keyword argument.
    #[must_use]
    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Overrides the per-key TTL in seconds (default
    /// [`DEFAULT_TTL_SECONDS`]), builder-style — pyfly's `ttl_seconds`
    /// keyword argument.
    #[must_use]
    pub fn with_ttl_seconds(mut self, ttl_seconds: u64) -> Self {
        self.ttl_seconds = ttl_seconds;
        self
    }

    /// Returns the full Redis key for an idempotency `key` (prefix + key) —
    /// pyfly's `self._prefix + idempotency_key`.
    fn redis_key(&self, key: &str) -> String {
        redis_key(&self.prefix, key)
    }
}

/// Joins a `prefix` and an idempotency `key` into the stored Redis key —
/// pyfly's `self._prefix + idempotency_key`. Factored out so the keying
/// rule can be tested without a live connection.
fn redis_key(prefix: &str, key: &str) -> String {
    format!("{prefix}{key}")
}

#[async_trait]
impl EventStore for RedisEventStore {
    /// `EXISTS <prefix><key>` — pyfly's `await redis.exists(...)`.
    async fn already_processed(&self, idempotency_key: &str) -> Result<bool, WebhookError> {
        let key = self.redis_key(idempotency_key);
        let mut conn = self.conn.lock().await;
        let exists: bool = conn.exists(&key).await.map_err(backend_err)?;
        Ok(exists)
    }

    /// `SET <prefix><key> "1" EX <ttl>` — pyfly's
    /// `await redis.set(..., "1", ex=self._ttl)`.
    async fn remember(&self, idempotency_key: &str) -> Result<(), WebhookError> {
        let key = self.redis_key(idempotency_key);
        let opts = SetOptions::default().with_expiration(SetExpiry::EX(self.ttl_seconds));
        let mut conn = self.conn.lock().await;
        // `SET` returns OK; capture as `()` so the redis crate does not try to
        // decode a value that the option-form SET does not return.
        conn.set_options::<_, _, ()>(&key, "1", opts)
            .await
            .map_err(backend_err)?;
        Ok(())
    }
}

/// Maps a [`redis::RedisError`] to [`WebhookError::Backend`].
fn backend_err(e: redis::RedisError) -> WebhookError {
    WebhookError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_pyfly() {
        assert_eq!(DEFAULT_KEY_PREFIX, "webhook:idem:");
        assert_eq!(DEFAULT_TTL_SECONDS, 86_400);
    }

    #[test]
    fn redis_key_applies_prefix() {
        assert_eq!(redis_key(DEFAULT_KEY_PREFIX, "evt-1"), "webhook:idem:evt-1");
        assert_eq!(redis_key("wh:", "abc"), "wh:abc");
        // Empty prefix passes the key through verbatim.
        assert_eq!(redis_key("", "k"), "k");
    }
}
