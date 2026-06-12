use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::{Adapter, CacheError};

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
}

/// Hook for distinguishing transport / connectivity failures from logical
/// errors. The default treats every non-[`CacheError::NotFound`] error as
/// transport, demoting aggressively to the secondary.
fn is_transport(err: &CacheError) -> bool {
    !err.is_not_found()
}
