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
