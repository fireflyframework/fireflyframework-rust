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
