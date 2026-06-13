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

//! CQRS health contributor — pyfly's `CqrsHealthIndicator`
//! (`cqrs/actuator/health.py`).
//!
//! [`CqrsHealthIndicator`] reports `UP` when the [`Bus`](crate::Bus) has at
//! least one registered handler, else `UNKNOWN` — the same UP/UNKNOWN rule
//! pyfly's `CqrsHealthIndicator.health()` applies. Register it with the
//! framework's health composite (`firefly_observability::Composite` /
//! `firefly_actuator`) so CQRS contributes a `cqrs` component to
//! `/actuator/health`.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_cqrs::{Bus, CqrsError, CqrsHealthIndicator, Message};
//! use firefly_observability::{Indicator, Status};
//! use serde::Serialize;
//!
//! #[derive(Clone, Serialize)]
//! struct Ping;
//! impl Message for Ping {}
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let bus = Arc::new(Bus::new());
//! let indicator = CqrsHealthIndicator::new(bus.clone());
//! // No handlers yet → UNKNOWN.
//! assert_eq!(indicator.check().await.status, Status::Unknown);
//!
//! bus.register(|_p: Ping| async move { Ok::<_, CqrsError>(()) });
//! assert_eq!(indicator.check().await.status, Status::Up);
//! # });
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use firefly_observability::{HealthResult, Indicator};

use crate::Bus;

/// The default health-indicator id, matching pyfly / Spring Boot's `cqrs`
/// health component key.
pub const CQRS_HEALTH_INDICATOR_NAME: &str = "cqrs";

/// A [`firefly_observability::Indicator`] reporting CQRS bus health — pyfly's
/// `CqrsHealthIndicator`.
///
/// Reports `UP` with a `handlers` detail (the registered handler count) when
/// the bus has at least one handler, else `UNKNOWN` — the same rule pyfly
/// applies (`UP` when `handlers > 0` else `UNKNOWN`).
pub struct CqrsHealthIndicator {
    name: String,
    bus: Arc<Bus>,
}

impl CqrsHealthIndicator {
    /// Wraps `bus` as an indicator reported under
    /// [`CQRS_HEALTH_INDICATOR_NAME`].
    pub fn new(bus: Arc<Bus>) -> Self {
        Self {
            name: CQRS_HEALTH_INDICATOR_NAME.to_string(),
            bus,
        }
    }

    /// Wraps `bus` as an indicator reported under a custom `name`.
    pub fn with_name(name: impl Into<String>, bus: Arc<Bus>) -> Self {
        Self {
            name: name.into(),
            bus,
        }
    }
}

#[async_trait]
impl Indicator for CqrsHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        let handlers = self.bus.handler_names().len();
        if handlers > 0 {
            HealthResult::up().with_detail("handlers", handlers as i64)
        } else {
            HealthResult::unknown().with_detail("handlers", 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CqrsError, Message};
    use firefly_observability::Status;

    #[derive(Clone, serde::Serialize)]
    struct Ping;
    impl Message for Ping {}

    #[tokio::test]
    async fn unknown_without_handlers() {
        let bus = Arc::new(Bus::new());
        let indicator = CqrsHealthIndicator::new(bus);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Unknown);
        assert_eq!(result.details.get("handlers"), Some(&serde_json::json!(0)));
    }

    #[tokio::test]
    async fn up_with_handlers() {
        let bus = Arc::new(Bus::new());
        bus.register(|_p: Ping| async move { Ok::<_, CqrsError>(()) });
        let indicator = CqrsHealthIndicator::new(bus);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Up);
        assert_eq!(result.details.get("handlers"), Some(&serde_json::json!(1)));
        assert_eq!(indicator.name(), "cqrs");
    }
}
