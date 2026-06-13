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

//! Orchestration health contributor — pyfly's
//! `pyfly.transactional.health.OrchestrationHealthIndicator`.
//!
//! [`OrchestrationHealthIndicator`] wraps an
//! [`PersistenceProvider`](crate::PersistenceProvider) and reports `UP` /
//! `DOWN` from its [`is_healthy`](crate::PersistenceProvider) probe, attaching
//! a `persistence` detail (`ok` / `unreachable`). Register it with the
//! framework health composite (`firefly_observability::Composite`) so
//! orchestration contributes a component to `/actuator/health`.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_orchestration::{MemoryPersistence, OrchestrationHealthIndicator};
//! use firefly_observability::{Indicator, Status};
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let persistence = Arc::new(MemoryPersistence::default());
//! let indicator = OrchestrationHealthIndicator::new(persistence);
//! assert_eq!(indicator.check().await.status, Status::Up);
//! # });
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use firefly_observability::{HealthResult, Indicator};

use crate::PersistenceProvider;

/// The default health-indicator id, matching pyfly's `orchestration`
/// health component key.
pub const ORCHESTRATION_HEALTH_INDICATOR_NAME: &str = "orchestration";

/// A [`firefly_observability::Indicator`] over an orchestration
/// [`PersistenceProvider`](crate::PersistenceProvider) — pyfly's
/// `OrchestrationHealthIndicator`.
///
/// Reports `UP` with `{persistence: "ok"}` when the provider's
/// [`is_healthy`](crate::PersistenceProvider) probe succeeds, else `DOWN`
/// with `{persistence: "unreachable"}`.
pub struct OrchestrationHealthIndicator {
    name: String,
    persistence: Arc<dyn PersistenceProvider>,
}

impl OrchestrationHealthIndicator {
    /// Wraps `persistence` as an indicator reported under
    /// [`ORCHESTRATION_HEALTH_INDICATOR_NAME`].
    pub fn new(persistence: Arc<dyn PersistenceProvider>) -> Self {
        Self {
            name: ORCHESTRATION_HEALTH_INDICATOR_NAME.to_string(),
            persistence,
        }
    }

    /// Wraps `persistence` as an indicator reported under a custom `name`.
    pub fn with_name(name: impl Into<String>, persistence: Arc<dyn PersistenceProvider>) -> Self {
        Self {
            name: name.into(),
            persistence,
        }
    }
}

#[async_trait]
impl Indicator for OrchestrationHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        if self.persistence.is_healthy().await {
            HealthResult::up().with_detail("persistence", "ok")
        } else {
            HealthResult::down("persistence provider unreachable")
                .with_detail("persistence", "unreachable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionState, PersistenceError};
    use firefly_observability::Status;

    #[tokio::test]
    async fn up_when_persistence_healthy() {
        let persistence = Arc::new(crate::MemoryPersistence::default());
        let indicator = OrchestrationHealthIndicator::new(persistence);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Up);
        assert_eq!(
            result.details.get("persistence"),
            Some(&serde_json::json!("ok"))
        );
        assert_eq!(indicator.name(), "orchestration");
    }

    /// A provider whose probe reports unhealthy.
    struct UnhealthyPersistence;

    #[async_trait]
    impl PersistenceProvider for UnhealthyPersistence {
        async fn save(&self, _state: ExecutionState) -> Result<(), PersistenceError> {
            Ok(())
        }
        async fn load(
            &self,
            _correlation_id: &str,
        ) -> Result<Option<ExecutionState>, PersistenceError> {
            Ok(None)
        }
        async fn list(
            &self,
            _filter: crate::ExecutionFilter,
        ) -> Result<Vec<ExecutionState>, PersistenceError> {
            Ok(Vec::new())
        }
        async fn list_stale(
            &self,
            _before: chrono::DateTime<chrono::Utc>,
        ) -> Result<Vec<ExecutionState>, PersistenceError> {
            Ok(Vec::new())
        }
        async fn delete(&self, _correlation_id: &str) -> Result<bool, PersistenceError> {
            Ok(false)
        }
        async fn cleanup(&self, _older_than: chrono::Duration) -> Result<usize, PersistenceError> {
            Ok(0)
        }
        async fn is_healthy(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn down_when_persistence_unhealthy() {
        let indicator = OrchestrationHealthIndicator::new(Arc::new(UnhealthyPersistence));
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Down);
        assert_eq!(
            result.details.get("persistence"),
            Some(&serde_json::json!("unreachable"))
        );
    }
}
