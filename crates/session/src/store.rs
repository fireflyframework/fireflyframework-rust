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

//! [`SessionStore`] port + [`MemorySessionStore`] and [`CacheSessionStore`]
//! adapters — the Rust port of pyfly's `SessionStore` protocol with its
//! `InMemorySessionStore` (and, via the cache bridge, any future backend).
//!
//! `save` takes a [`std::time::Duration`] TTL (pyfly: `ttl: int` seconds).
//! [`MemorySessionStore`] evicts on expiry lazily on read (matching pyfly's
//! `time.monotonic()` check) and also exposes [`MemorySessionStore::sweep`]
//! for eager removal. [`CacheSessionStore`] bridges any
//! [`firefly_cache::Adapter`] (JSON-serialized bytes, key-prefixed) so a
//! Redis/other cache backend can persist sessions without this crate taking
//! a hard Redis dependency — the analog of pyfly's `RedisSessionStore`,
//! made safe by serde typing instead of an importlib allowlist.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use firefly_cache::{Adapter, CacheError};
use serde_json::Value;
use tokio::sync::Mutex;

/// The raw session payload persisted by a store: the attribute map
/// including internal metadata keys.
pub type SessionData = HashMap<String, Value>;

/// Errors a [`SessionStore`] may report. Persistence backends surface
/// transport failures as [`SessionStoreError::Backend`]; (de)serialization
/// failures as [`SessionStoreError::Codec`].
#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    /// JSON (de)serialization failure.
    #[error("firefly/session: codec: {0}")]
    Codec(#[from] serde_json::Error),

    /// Backend / connectivity failure reported by an adapter.
    #[error("firefly/session: backend: {0}")]
    Backend(String),
}

/// Abstract session persistence interface — the Rust port of pyfly's
/// `SessionStore` protocol. All backends (in-memory, cache-bridged, Redis)
/// implement it.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Retrieves session data, or `None` if missing or expired.
    async fn get(&self, session_id: &str) -> Result<Option<SessionData>, SessionStoreError>;

    /// Stores session data with a time-to-live.
    async fn save(
        &self,
        session_id: &str,
        data: &SessionData,
        ttl: Duration,
    ) -> Result<(), SessionStoreError>;

    /// Removes a session (idempotent).
    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError>;

    /// Reports whether a session exists and is not expired.
    async fn exists(&self, session_id: &str) -> Result<bool, SessionStoreError> {
        Ok(self.get(session_id).await?.is_some())
    }
}

#[async_trait]
impl<S: SessionStore + ?Sized> SessionStore for Arc<S> {
    async fn get(&self, session_id: &str) -> Result<Option<SessionData>, SessionStoreError> {
        (**self).get(session_id).await
    }

    async fn save(
        &self,
        session_id: &str,
        data: &SessionData,
        ttl: Duration,
    ) -> Result<(), SessionStoreError> {
        (**self).save(session_id, data, ttl).await
    }

    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        (**self).delete(session_id).await
    }

    async fn exists(&self, session_id: &str) -> Result<bool, SessionStoreError> {
        (**self).exists(session_id).await
    }
}

struct Entry {
    data: SessionData,
    expires_at: Instant,
}

/// In-memory [`SessionStore`] with TTL eviction — the Rust port of pyfly's
/// `InMemorySessionStore`. Suitable for development, testing, and
/// single-process applications. Expired entries are dropped lazily on read
/// (and eagerly by [`Self::sweep`]).
#[derive(Default)]
pub struct MemorySessionStore {
    entries: Mutex<HashMap<String, Entry>>,
}

impl MemorySessionStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Eagerly removes all expired entries and returns how many were
    /// evicted. The lazy path on [`SessionStore::get`] suffices for
    /// correctness; this exists for callers that want to reclaim memory.
    pub async fn sweep(&self) -> usize {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        let before = entries.len();
        entries.retain(|_, e| e.expires_at > now);
        before - entries.len()
    }

    /// The number of (possibly-expired) entries currently held.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Whether the store holds no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.lock().await.is_empty()
    }
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn get(&self, session_id: &str) -> Result<Option<SessionData>, SessionStoreError> {
        let mut entries = self.entries.lock().await;
        match entries.get(session_id) {
            None => Ok(None),
            Some(entry) if Instant::now() > entry.expires_at => {
                entries.remove(session_id);
                Ok(None)
            }
            Some(entry) => Ok(Some(entry.data.clone())),
        }
    }

    async fn save(
        &self,
        session_id: &str,
        data: &SessionData,
        ttl: Duration,
    ) -> Result<(), SessionStoreError> {
        let expires_at = Instant::now() + ttl;
        self.entries.lock().await.insert(
            session_id.to_string(),
            Entry {
                data: data.clone(),
                expires_at,
            },
        );
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        self.entries.lock().await.remove(session_id);
        Ok(())
    }

    async fn exists(&self, session_id: &str) -> Result<bool, SessionStoreError> {
        let mut entries = self.entries.lock().await;
        match entries.get(session_id) {
            None => Ok(false),
            Some(entry) if Instant::now() > entry.expires_at => {
                entries.remove(session_id);
                Ok(false)
            }
            Some(_) => Ok(true),
        }
    }
}

/// The default key prefix used by [`CacheSessionStore`], mirroring pyfly's
/// `RedisSessionStore` `pyfly:session:` namespace.
pub const DEFAULT_CACHE_PREFIX: &str = "firefly:session:";

/// A [`SessionStore`] that persists JSON-serialized session data into any
/// [`firefly_cache::Adapter`] — the bridge that lets a Redis (or other)
/// cache backend store sessions, the Rust analog of pyfly's
/// `RedisSessionStore`. Keys are namespaced with a configurable prefix.
pub struct CacheSessionStore<A: Adapter> {
    adapter: A,
    prefix: String,
}

impl<A: Adapter> CacheSessionStore<A> {
    /// Wraps `adapter` with the [`DEFAULT_CACHE_PREFIX`] key namespace.
    pub fn new(adapter: A) -> Self {
        Self {
            adapter,
            prefix: DEFAULT_CACHE_PREFIX.to_string(),
        }
    }

    /// Wraps `adapter` with a custom key `prefix`.
    pub fn with_prefix(adapter: A, prefix: impl Into<String>) -> Self {
        Self {
            adapter,
            prefix: prefix.into(),
        }
    }

    fn key(&self, session_id: &str) -> String {
        format!("{}{}", self.prefix, session_id)
    }
}

#[async_trait]
impl<A: Adapter> SessionStore for CacheSessionStore<A> {
    async fn get(&self, session_id: &str) -> Result<Option<SessionData>, SessionStoreError> {
        match self.adapter.get(&self.key(session_id)).await {
            Ok(bytes) => {
                let data: SessionData = serde_json::from_slice(&bytes)?;
                Ok(Some(data))
            }
            Err(CacheError::NotFound) => Ok(None),
            Err(CacheError::Codec(e)) => Err(SessionStoreError::Codec(e)),
            Err(CacheError::Backend(e)) => Err(SessionStoreError::Backend(e)),
        }
    }

    async fn save(
        &self,
        session_id: &str,
        data: &SessionData,
        ttl: Duration,
    ) -> Result<(), SessionStoreError> {
        let bytes = serde_json::to_vec(data)?;
        self.adapter
            .set(&self.key(session_id), &bytes, Some(ttl))
            .await
            .map_err(map_cache_err)
    }

    async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        self.adapter
            .delete(&self.key(session_id))
            .await
            .map_err(map_cache_err)
    }

    async fn exists(&self, session_id: &str) -> Result<bool, SessionStoreError> {
        self.adapter
            .exists(&self.key(session_id))
            .await
            .map_err(map_cache_err)
    }
}

fn map_cache_err(e: CacheError) -> SessionStoreError {
    match e {
        CacheError::NotFound => SessionStoreError::Backend("not found".to_string()),
        CacheError::Codec(e) => SessionStoreError::Codec(e),
        CacheError::Backend(e) => SessionStoreError::Backend(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_cache::MemoryAdapter;

    fn data(k: &str, v: i64) -> SessionData {
        let mut m = HashMap::new();
        m.insert(k.to_string(), Value::from(v));
        m
    }

    #[tokio::test]
    async fn save_get_delete_exists() {
        // pyfly: TestInMemorySessionStore.test_save_get_delete_exists
        let store = MemorySessionStore::new();
        store
            .save("sid", &data("a", 1), Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.get("sid").await.unwrap(), Some(data("a", 1)));
        assert!(store.exists("sid").await.unwrap());
        store.delete("sid").await.unwrap();
        assert_eq!(store.get("sid").await.unwrap(), None);
        assert!(!store.exists("sid").await.unwrap());
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        // pyfly: TestInMemorySessionStore.test_get_missing_returns_none
        let store = MemorySessionStore::new();
        assert_eq!(store.get("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_entry_is_evicted() {
        // pyfly: TestInMemorySessionStore.test_expired_entry_is_evicted
        let store = MemorySessionStore::new();
        // Zero TTL: already expired the instant it is read.
        store
            .save("sid", &data("a", 1), Duration::from_nanos(0))
            .await
            .unwrap();
        assert_eq!(store.get("sid").await.unwrap(), None);
        assert!(!store.exists("sid").await.unwrap());
    }

    #[tokio::test]
    async fn sweep_removes_expired() {
        let store = MemorySessionStore::new();
        store
            .save("a", &data("a", 1), Duration::from_nanos(0))
            .await
            .unwrap();
        store
            .save("b", &data("b", 2), Duration::from_secs(60))
            .await
            .unwrap();
        let removed = store.sweep().await;
        assert_eq!(removed, 1);
        assert_eq!(store.len().await, 1);
        assert!(store.exists("b").await.unwrap());
    }

    #[tokio::test]
    async fn arc_store_delegates() {
        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        store
            .save("sid", &data("a", 1), Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.get("sid").await.unwrap(), Some(data("a", 1)));
    }

    #[tokio::test]
    async fn cache_bridge_roundtrip() {
        let store = CacheSessionStore::new(MemoryAdapter::new());
        store
            .save("sid", &data("user", 42), Duration::from_secs(60))
            .await
            .unwrap();
        assert_eq!(store.get("sid").await.unwrap(), Some(data("user", 42)));
        assert!(store.exists("sid").await.unwrap());
        store.delete("sid").await.unwrap();
        assert_eq!(store.get("sid").await.unwrap(), None);
    }

    #[tokio::test]
    async fn cache_bridge_uses_prefix() {
        let adapter = MemoryAdapter::new();
        let store = CacheSessionStore::with_prefix(adapter, "app:sess:");
        store
            .save("sid", &data("a", 1), Duration::from_secs(60))
            .await
            .unwrap();
        // Confirm namespacing via the store API (the prefix is internal).
        assert_eq!(store.get("sid").await.unwrap(), Some(data("a", 1)));
        assert_eq!(store.get("other").await.unwrap(), None);
    }
}
