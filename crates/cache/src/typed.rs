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

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::adapter::{Adapter, CacheError};

/// Wraps an [`Adapter`] with JSON-encoded read/write helpers for any type
/// `T` — the canonical way services interact with the cache.
///
/// Encoding goes through `serde_json`, so the stored bytes are wire-compatible
/// with the Go port's `encoding/json` output for equivalently-tagged types.
pub struct Typed<T> {
    /// The underlying byte-level cache adapter.
    pub adapter: Arc<dyn Adapter>,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Typed<T> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
            _marker: PhantomData,
        }
    }
}

impl<T> Typed<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Returns a `Typed<T>` over `adapter`.
    #[must_use]
    pub fn new(adapter: Arc<dyn Adapter>) -> Self {
        Self {
            adapter,
            _marker: PhantomData,
        }
    }

    /// Fetches and JSON-decodes the value at `key`.
    pub async fn get(&self, key: &str) -> Result<T, CacheError> {
        let raw = self.adapter.get(key).await?;
        Ok(serde_json::from_slice(&raw)?)
    }

    /// JSON-encodes `value` and writes it under `key`.
    pub async fn set(&self, key: &str, value: &T, ttl: Option<Duration>) -> Result<(), CacheError> {
        let raw = serde_json::to_vec(value)?;
        self.adapter.set(key, &raw, ttl).await
    }

    /// Always writes `value` under `key` for `ttl` and returns it — the
    /// always-execute-then-store path pyfly's `@cache_put` decorator
    /// takes. Unlike [`get_or_set`](Typed::get_or_set), no read happens
    /// first and any existing entry is overwritten; the write error
    /// surfaces (the value is not returned on failure).
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::time::Duration;
    /// use firefly_cache::{MemoryAdapter, Typed};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), firefly_cache::CacheError> {
    /// let typed: Typed<u32> = Typed::new(Arc::new(MemoryAdapter::new()));
    /// let stored = typed.put("counter", 7, Some(Duration::from_secs(60))).await?;
    /// assert_eq!(stored, 7);
    /// assert_eq!(typed.get("counter").await?, 7);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn put(&self, key: &str, value: T, ttl: Option<Duration>) -> Result<T, CacheError> {
        self.set(key, &value, ttl).await?;
        Ok(value)
    }

    /// Removes the entry at `key` — the single-key form of pyfly's
    /// `@cache_evict`. A missing key is a no-op (returns `Ok`), matching
    /// the [`Adapter::delete`](crate::Adapter) contract. A typed
    /// convenience passthrough so call sites that hold a [`Typed`] need
    /// not reach for `self.adapter`.
    pub async fn delete(&self, key: &str) -> Result<(), CacheError> {
        self.adapter.delete(key).await
    }

    /// Removes every entry whose key starts with `prefix`, returning the
    /// number removed — the prefix (`all_entries`-style) form of pyfly's
    /// `@cache_evict`, delegating to
    /// [`Adapter::delete_prefix`](crate::Adapter). Backends without prefix
    /// support surface [`CacheError::Backend`].
    pub async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError> {
        self.adapter.delete_prefix(prefix).await
    }

    /// Returns the cached value or, on miss, computes it via `loader`,
    /// caches it for `ttl` and returns it. A miss is signalled by
    /// [`CacheError::NotFound`]; any other read error surfaces unchanged.
    ///
    /// A caching failure after a successful load does **not** mask the
    /// loaded value — the value is returned and the write error dropped.
    pub async fn get_or_set<F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        loader: F,
    ) -> Result<T, CacheError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, CacheError>>,
    {
        match self.get(key).await {
            Ok(v) => return Ok(v),
            Err(err) if !err.is_not_found() => return Err(err),
            Err(_) => {}
        }
        let loaded = loader().await?;
        // Caching failure should not mask successful load.
        let _ = self.set(key, &loaded, ttl).await;
        Ok(loaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryAdapter;

    #[tokio::test]
    async fn put_always_stores_and_overwrites() {
        let typed: Typed<u32> = Typed::new(Arc::new(MemoryAdapter::new()));
        assert_eq!(typed.put("k", 1, None).await.unwrap(), 1);
        assert_eq!(typed.get("k").await.unwrap(), 1);
        // Overwrites unconditionally (no read-first), unlike get_or_set.
        assert_eq!(typed.put("k", 2, None).await.unwrap(), 2);
        assert_eq!(typed.get("k").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn delete_removes_entry_and_is_noop_when_absent() {
        let typed: Typed<String> = Typed::new(Arc::new(MemoryAdapter::new()));
        typed.set("k", &"v".to_string(), None).await.unwrap();
        typed.delete("k").await.unwrap();
        assert!(typed.get("k").await.unwrap_err().is_not_found());
        // Deleting a missing key is a no-op.
        typed.delete("missing").await.unwrap();
    }

    #[tokio::test]
    async fn delete_prefix_evicts_matching_entries() {
        let typed: Typed<u32> = Typed::new(Arc::new(MemoryAdapter::new()));
        typed.set("user:1", &1, None).await.unwrap();
        typed.set("user:2", &2, None).await.unwrap();
        typed.set("order:1", &9, None).await.unwrap();
        let removed = typed.delete_prefix("user:").await.unwrap();
        assert_eq!(removed, 2);
        assert!(typed.get("user:1").await.unwrap_err().is_not_found());
        assert_eq!(typed.get("order:1").await.unwrap(), 9);
    }
}
