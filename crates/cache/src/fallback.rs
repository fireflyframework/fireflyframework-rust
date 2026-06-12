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

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::{Adapter, CacheError, CacheStats};

/// Chains a primary adapter with a secondary so that any failure from the
/// primary (other than [`CacheError::NotFound`]) demotes the request to the
/// secondary. The .NET port calls this pattern "primary + fallback".
///
/// `FallbackAdapter` is itself an [`Adapter`], so consumers stay insulated
/// from the failover behaviour.
#[derive(Clone)]
pub struct FallbackAdapter {
    /// The preferred adapter, consulted first on every operation.
    pub primary: Arc<dyn Adapter>,
    /// The adapter requests demote to when the primary fails.
    pub secondary: Arc<dyn Adapter>,
}

impl FallbackAdapter {
    /// Returns a `FallbackAdapter` wrapping `primary` and `secondary`.
    #[must_use]
    pub fn new(primary: Arc<dyn Adapter>, secondary: Arc<dyn Adapter>) -> Self {
        Self { primary, secondary }
    }
}

#[async_trait]
impl Adapter for FallbackAdapter {
    /// Tries primary first; on transport failure falls through to secondary.
    /// [`CacheError::NotFound`] from the primary is treated as a hit-miss and
    /// the secondary is consulted as well — the union of both stores is the
    /// effective view.
    async fn get(&self, key: &str) -> Result<Vec<u8>, CacheError> {
        match self.primary.get(key).await {
            Ok(v) => Ok(v),
            Err(err) => {
                if !err.is_not_found() && !is_transport(&err) {
                    // Unknown error class — surface it.
                    return Err(err);
                }
                self.secondary.get(key).await
            }
        }
    }

    /// Writes to both adapters; the first non-transport error wins.
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<(), CacheError> {
        if let Err(err) = self.primary.set(key, value, ttl).await {
            if !is_transport(&err) {
                return Err(err);
            }
        }
        self.secondary.set(key, value, ttl).await
    }

    /// Removes from both adapters. The first non-transport error wins.
    async fn delete(&self, key: &str) -> Result<(), CacheError> {
        if let Err(err) = self.primary.delete(key).await {
            if !is_transport(&err) {
                return Err(err);
            }
        }
        self.secondary.delete(key).await
    }

    /// Clears both adapters. The first non-transport error wins.
    async fn clear(&self) -> Result<(), CacheError> {
        if let Err(err) = self.primary.clear().await {
            if !is_transport(&err) {
                return Err(err);
            }
        }
        self.secondary.clear().await
    }

    fn name(&self) -> String {
        format!(
            "fallback({}+{})",
            self.primary.name(),
            self.secondary.name()
        )
    }

    /// Reports healthy when at least one of the adapters is healthy.
    async fn health_check(&self) -> Result<(), CacheError> {
        if self.primary.health_check().await.is_ok() {
            return Ok(());
        }
        self.secondary.health_check().await
    }

    /// Mirrors the conditional write to both adapters. Returns `true` if
    /// *either* adapter recorded a fresh write — pyfly's `CacheManager
    /// .put_if_absent` (`result or fallback_result`). A primary transport
    /// failure is swallowed so the secondary still gets the write.
    async fn set_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, CacheError> {
        let primary = match self.primary.set_if_absent(key, value, ttl).await {
            Ok(stored) => stored,
            Err(err) if is_transport(&err) => false,
            Err(err) => return Err(err),
        };
        let secondary = self.secondary.set_if_absent(key, value, ttl).await?;
        Ok(primary || secondary)
    }

    /// Union existence: `true` when *either* adapter holds the key —
    /// pyfly's `CacheManager.exists`. A primary transport failure demotes
    /// to the secondary.
    async fn exists(&self, key: &str) -> Result<bool, CacheError> {
        match self.primary.exists(key).await {
            Ok(true) => Ok(true),
            Ok(false) => self.secondary.exists(key).await,
            Err(err) if is_transport(&err) => self.secondary.exists(key).await,
            Err(err) => Err(err),
        }
    }

    /// Evicts the prefix from both adapters and returns the *summed* count —
    /// pyfly's `CacheManager.evict_by_prefix` (`primary_count +
    /// fallback_count`). A primary transport failure contributes `0`.
    async fn delete_prefix(&self, prefix: &str) -> Result<u64, CacheError> {
        let primary = match self.primary.delete_prefix(prefix).await {
            Ok(n) => n,
            Err(err) if is_transport(&err) => 0,
            Err(err) => return Err(err),
        };
        let secondary = self.secondary.delete_prefix(prefix).await?;
        Ok(primary + secondary)
    }

    /// Returns the primary's stats when available, otherwise the
    /// secondary's — the composite has no counters of its own.
    async fn stats(&self) -> Option<CacheStats> {
        match self.primary.stats().await {
            Some(s) => Some(s),
            None => self.secondary.stats().await,
        }
    }
}

/// Hook for distinguishing transport / connectivity failures from logical
/// errors. The default treats every non-[`CacheError::NotFound`] error as
/// transport, demoting aggressively to the secondary.
fn is_transport(err: &CacheError) -> bool {
    !err.is_not_found()
}
