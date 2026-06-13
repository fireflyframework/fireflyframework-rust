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

//! Cache liveness as a [`firefly_observability::Indicator`].
//!
//! Mirrors pyfly's `cache.health.CacheHealthIndicator` (audit #74): an
//! *active* round-trip probe rather than a bare reachability ping. It
//! writes a sentinel value, reads it back, evicts it, and reports the
//! round-trip latency under a `latencyMs` detail.
//!
//! ## Status mapping
//!
//! | Outcome                          | Status               |
//! |----------------------------------|----------------------|
//! | round-trip < `threshold`         | [`Status::Up`]       |
//! | round-trip ≥ `threshold` (1000ms)| [`Status::Degraded`] |
//! | read-back value mismatch         | [`Status::Down`]     |
//! | adapter error                    | [`Status::Down`]     |
//!
//! pyfly returns `OUT_OF_SERVICE` for the slow-but-working case; the Rust
//! [`Status`] enum has no `OUT_OF_SERVICE` variant, so the framework maps
//! that "works but with reduced capability" case to [`Status::Degraded`] —
//! the semantically equivalent state in the composite rollup.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_cache::{CacheHealthIndicator, MemoryAdapter};
//! use firefly_observability::{Indicator, Status};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let indicator = CacheHealthIndicator::new(Arc::new(MemoryAdapter::new()));
//! let result = indicator.check().await;
//! assert_eq!(result.status, Status::Up);
//! assert!(result.details.contains_key("latencyMs"));
//! # });
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use firefly_observability::{HealthResult, Indicator};

use crate::Adapter;

/// The sentinel key the probe writes, reads, and evicts. Namespaced so it
/// cannot collide with application keys — the Rust analog of pyfly's
/// `__pyfly_cache_health_probe__`.
const PROBE_KEY: &str = "__firefly_cache_health_probe__";

/// The sentinel value the probe stores and reads back.
const PROBE_VALUE: &[u8] = b"ok";

/// The latency at or above which the cache is reported [`Status::Degraded`]
/// rather than [`Status::Up`] — pyfly's 1000ms `OUT_OF_SERVICE` threshold.
const DEFAULT_THRESHOLD: Duration = Duration::from_millis(1000);

/// A [`firefly_observability::Indicator`] that probes a cache
/// [`Adapter`](crate::Adapter) with a put/get/evict round-trip and reports
/// the latency — pyfly's `CacheHealthIndicator`.
///
/// Reports [`Status::Up`](firefly_observability::Status) on a fast,
/// correct round-trip; [`Status::Degraded`](firefly_observability::Status)
/// when the round-trip is correct but slow (≥ the configured threshold,
/// pyfly's `OUT_OF_SERVICE`); and
/// [`Status::Down`](firefly_observability::Status) when the read-back
/// value mismatches or the adapter errors. Every result carries an
/// `adapter` detail ([`Adapter::name`](crate::Adapter::name)) and, on a
/// completed probe, a `latencyMs` detail.
pub struct CacheHealthIndicator {
    name: String,
    adapter: Arc<dyn Adapter>,
    threshold: Duration,
}

impl CacheHealthIndicator {
    /// The default indicator id, matching pyfly / Spring Boot's `cache`
    /// health key.
    pub const DEFAULT_NAME: &'static str = "cache";

    /// Wraps `adapter` as an indicator reported under
    /// [`DEFAULT_NAME`](Self::DEFAULT_NAME) with the default 1000ms
    /// degraded threshold.
    pub fn new(adapter: Arc<dyn Adapter>) -> Self {
        Self {
            name: Self::DEFAULT_NAME.to_string(),
            adapter,
            threshold: DEFAULT_THRESHOLD,
        }
    }

    /// Wraps `adapter` under a custom indicator `name`.
    pub fn with_name(name: impl Into<String>, adapter: Arc<dyn Adapter>) -> Self {
        Self {
            name: name.into(),
            adapter,
            threshold: DEFAULT_THRESHOLD,
        }
    }

    /// Overrides the latency threshold above which the indicator reports
    /// [`Status::Degraded`](firefly_observability::Status) (default
    /// 1000ms). Builder-style.
    #[must_use]
    pub fn with_threshold(mut self, threshold: Duration) -> Self {
        self.threshold = threshold;
        self
    }
}

#[async_trait]
impl Indicator for CacheHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        let adapter_name = self.adapter.name();
        let started = Instant::now();

        if let Err(err) = self.adapter.set(PROBE_KEY, PROBE_VALUE, None).await {
            return probe_error(&adapter_name, "set", &err);
        }
        let read = match self.adapter.get(PROBE_KEY).await {
            Ok(v) => v,
            Err(err) => return probe_error(&adapter_name, "get", &err),
        };
        // Best-effort cleanup; an evict failure still leaves a working
        // read/write path, so it does not down the probe on its own — but
        // surface it as a detail.
        let evict_failed = self.adapter.delete(PROBE_KEY).await.is_err();
        let latency = started.elapsed();
        let latency_ms = round2(latency.as_secs_f64() * 1000.0);

        if read != PROBE_VALUE {
            return HealthResult::down("cache probe mismatch")
                .with_detail("adapter", adapter_name)
                .with_detail("error", "probe-mismatch");
        }

        let mut result = if latency >= self.threshold {
            // pyfly's OUT_OF_SERVICE maps to Degraded in the Rust status set.
            HealthResult::degraded("cache latency above threshold")
        } else {
            HealthResult::up()
        };
        result = result
            .with_detail("adapter", adapter_name)
            .with_detail("latencyMs", latency_ms);
        if evict_failed {
            result = result.with_detail("evict", "failed");
        }
        result
    }
}

/// Builds a `DOWN` result for an adapter error during a probe phase.
fn probe_error(adapter: &str, phase: &str, err: &crate::CacheError) -> HealthResult {
    HealthResult::down(err.to_string())
        .with_detail("adapter", adapter.to_string())
        .with_detail("phase", phase.to_string())
        .with_detail("error", err.to_string())
}

/// Rounds to two decimals, matching pyfly's `round(latency_ms, 2)`.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use firefly_observability::Status;

    use super::*;
    use crate::{CacheError, MemoryAdapter};

    #[tokio::test]
    async fn healthy_adapter_is_up_with_latency_detail() {
        let indicator = CacheHealthIndicator::new(Arc::new(MemoryAdapter::new()));
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Up);
        assert_eq!(indicator.name(), "cache");
        assert_eq!(
            result.details.get("adapter").and_then(|v| v.as_str()),
            Some("memory")
        );
        assert!(result.details.contains_key("latencyMs"));
    }

    #[tokio::test]
    async fn probe_does_not_leak_the_sentinel_key() {
        let adapter = Arc::new(MemoryAdapter::new());
        let indicator = CacheHealthIndicator::new(Arc::clone(&adapter) as Arc<dyn Adapter>);
        indicator.check().await;
        // The probe evicted its own key.
        assert!(adapter.get(PROBE_KEY).await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn failing_adapter_is_down() {
        struct DeadAdapter;
        #[async_trait]
        impl Adapter for DeadAdapter {
            async fn get(&self, _key: &str) -> Result<Vec<u8>, CacheError> {
                Err(CacheError::Backend("unreachable".into()))
            }
            async fn set(
                &self,
                _key: &str,
                _value: &[u8],
                _ttl: Option<Duration>,
            ) -> Result<(), CacheError> {
                Err(CacheError::Backend("unreachable".into()))
            }
            async fn delete(&self, _key: &str) -> Result<(), CacheError> {
                Ok(())
            }
            async fn clear(&self) -> Result<(), CacheError> {
                Ok(())
            }
            fn name(&self) -> String {
                "dead".into()
            }
            async fn health_check(&self) -> Result<(), CacheError> {
                Err(CacheError::Backend("unreachable".into()))
            }
        }
        let indicator = CacheHealthIndicator::new(Arc::new(DeadAdapter));
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Down);
        assert_eq!(
            result.details.get("phase").and_then(|v| v.as_str()),
            Some("set")
        );
    }

    #[tokio::test]
    async fn read_back_mismatch_is_down() {
        // An adapter whose get returns a value different from the probe.
        struct MismatchAdapter;
        #[async_trait]
        impl Adapter for MismatchAdapter {
            async fn get(&self, _key: &str) -> Result<Vec<u8>, CacheError> {
                Ok(b"corrupted".to_vec())
            }
            async fn set(
                &self,
                _key: &str,
                _value: &[u8],
                _ttl: Option<Duration>,
            ) -> Result<(), CacheError> {
                Ok(())
            }
            async fn delete(&self, _key: &str) -> Result<(), CacheError> {
                Ok(())
            }
            async fn clear(&self) -> Result<(), CacheError> {
                Ok(())
            }
            fn name(&self) -> String {
                "mismatch".into()
            }
            async fn health_check(&self) -> Result<(), CacheError> {
                Ok(())
            }
        }
        let indicator = CacheHealthIndicator::new(Arc::new(MismatchAdapter));
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Down);
        assert_eq!(
            result.details.get("error").and_then(|v| v.as_str()),
            Some("probe-mismatch")
        );
    }

    #[tokio::test]
    async fn zero_threshold_reports_degraded_when_slow() {
        // A zero threshold forces the slow-path branch deterministically
        // (any measured latency is >= 0ms), exercising pyfly's
        // OUT_OF_SERVICE → Degraded mapping without a real sleep.
        let indicator = CacheHealthIndicator::new(Arc::new(MemoryAdapter::new()))
            .with_threshold(Duration::ZERO);
        let result = indicator.check().await;
        assert_eq!(result.status, Status::Degraded);
        assert!(result.details.contains_key("latencyMs"));
    }
}
