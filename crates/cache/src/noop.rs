use std::time::Duration;

use async_trait::async_trait;

use crate::adapter::{Adapter, CacheError};

/// The disabled cache: every `get` misses, every `set`/`delete`/`clear`
/// silently succeeds. Used as the default in services that opt out of
/// caching, mirroring the Java/.NET/Go NoOp adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpAdapter;

#[async_trait]
impl Adapter for NoOpAdapter {
    /// Always returns [`CacheError::NotFound`].
    async fn get(&self, _key: &str) -> Result<Vec<u8>, CacheError> {
        Err(CacheError::NotFound)
    }

    /// A no-op.
    async fn set(
        &self,
        _key: &str,
        _value: &[u8],
        _ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        Ok(())
    }

    /// A no-op.
    async fn delete(&self, _key: &str) -> Result<(), CacheError> {
        Ok(())
    }

    /// A no-op.
    async fn clear(&self) -> Result<(), CacheError> {
        Ok(())
    }

    fn name(&self) -> String {
        "noop".to_owned()
    }

    async fn health_check(&self) -> Result<(), CacheError> {
        Ok(())
    }
}
