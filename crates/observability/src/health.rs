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

//! Composable health indicators and the composite aggregator.
//!
//! Mirrors the Go port's `observability` health primitives: a [`Status`]
//! vocabulary (`UP` / `DOWN` / `DEGRADED` / `UNKNOWN`), a per-probe
//! [`HealthResult`], the [`Indicator`] trait, and a [`Composite`] that
//! rolls individual results up into one overall status. The JSON wire
//! shape is identical to the Java `HealthIndicator`, the .NET
//! `HealthStatus` payloads, and the Go `HealthResult` struct tags.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The canonical Firefly health states. Wire-compatible with the Java
/// `HealthIndicator.Status`, the .NET `HealthStatus` enum, and the Go
/// `observability.Status` string — every port emits `"UP"`, `"DOWN"`,
/// `"DEGRADED"`, or `"UNKNOWN"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    /// The component is fully operational.
    Up,
    /// The component is unavailable.
    Down,
    /// The component works but with reduced capability.
    Degraded,
    /// The component's state could not be determined. Neutral in the
    /// composite rollup: it neither downs nor degrades the overall status.
    Unknown,
}

impl Status {
    /// The wire name (`"UP"`, `"DOWN"`, `"DEGRADED"`, `"UNKNOWN"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Up => "UP",
            Status::Down => "DOWN",
            Status::Degraded => "DEGRADED",
            Status::Unknown => "UNKNOWN",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The value returned by an [`Indicator`] check.
///
/// Serializes to the exact JSON shape the Go port emits: `status`,
/// `message` (omitted when empty), `details` (omitted when empty),
/// `duration` (integer nanoseconds, the encoding Go's `encoding/json`
/// gives `time.Duration`), and `time` (RFC 3339 UTC timestamp of when
/// the check started).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResult {
    /// Outcome of the check.
    pub status: Status,
    /// Optional human-readable detail; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Optional structured payload; omitted from JSON when empty —
    /// matching Go's `json:"details,omitempty"` on a map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
    /// How long the check took; stamped by [`Composite::check_all`].
    /// Serialized as integer nanoseconds, like Go's `time.Duration`.
    #[serde(with = "duration_nanos")]
    pub duration: Duration,
    /// Wall-clock instant (UTC) at which the check started; stamped by
    /// [`Composite::check_all`].
    pub time: DateTime<Utc>,
}

impl HealthResult {
    /// Returns a result with the given status, no message, no details.
    pub fn new(status: Status) -> Self {
        Self {
            status,
            message: String::new(),
            details: BTreeMap::new(),
            duration: Duration::ZERO,
            time: Utc::now(),
        }
    }

    /// Convenience constructor for [`Status::Up`].
    pub fn up() -> Self {
        Self::new(Status::Up)
    }

    /// Convenience constructor for [`Status::Down`] with a message.
    pub fn down(message: impl Into<String>) -> Self {
        Self::new(Status::Down).with_message(message)
    }

    /// Convenience constructor for [`Status::Degraded`] with a message.
    pub fn degraded(message: impl Into<String>) -> Self {
        Self::new(Status::Degraded).with_message(message)
    }

    /// Convenience constructor for [`Status::Unknown`].
    pub fn unknown() -> Self {
        Self::new(Status::Unknown)
    }

    /// Sets the human-readable message (builder-style).
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// Adds one structured detail entry (builder-style).
    #[must_use]
    pub fn with_detail(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}

/// Serde adapter encoding [`Duration`] as integer nanoseconds — the wire
/// encoding Go's `encoding/json` gives `time.Duration`.
mod duration_nanos {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(d.as_nanos().min(i64::MAX as u128) as i64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let nanos = i64::deserialize(d)?;
        Ok(Duration::from_nanos(nanos.max(0) as u64))
    }
}

/// A single named health probe — the async analog of the Go
/// `observability.Indicator` interface. Cancellation flows through future
/// drop instead of Go's `context.Context` parameter.
#[async_trait]
pub trait Indicator: Send + Sync {
    /// Stable name under which the result is reported.
    fn name(&self) -> &str;
    /// Runs the probe and reports its outcome.
    async fn check(&self) -> HealthResult;
}

/// Adapts a plain async closure to the [`Indicator`] trait — the
/// counterpart of Go's `observability.IndicatorFunc`.
///
/// ```
/// use firefly_observability::{HealthResult, IndicatorFn};
///
/// let probe = IndicatorFn::new("db", || async { HealthResult::up() });
/// # let _ = probe;
/// ```
pub struct IndicatorFn<F> {
    name: String,
    f: F,
}

impl<F, Fut> IndicatorFn<F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = HealthResult> + Send,
{
    /// Wraps `f` as an indicator reporting under `name`.
    pub fn new(name: impl Into<String>, f: F) -> Self {
        Self {
            name: name.into(),
            f,
        }
    }
}

#[async_trait]
impl<F, Fut> Indicator for IndicatorFn<F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = HealthResult> + Send,
{
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        (self.f)().await
    }
}

/// Aggregates multiple indicators. The overall status is `DOWN` if any
/// indicator is `DOWN`, else `DEGRADED` if any is `DEGRADED`, else `UP` —
/// the same precedence the Go `observability.Composite` applies. An empty
/// composite reports `UP`.
///
/// Registration uses interior mutability (`&self`), mirroring the Go
/// struct's internal `sync.RWMutex`, so the composite can be shared as
/// `Arc<Composite>` between the actuator endpoint and startup wiring.
#[derive(Default)]
pub struct Composite {
    indicators: RwLock<Vec<Arc<dyn Indicator>>>,
}

impl Composite {
    /// Returns an empty composite (overall status `UP`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an indicator.
    pub fn add<I: Indicator + 'static>(&self, indicator: I) {
        self.add_arc(Arc::new(indicator));
    }

    /// Registers an already-shared indicator.
    pub fn add_arc(&self, indicator: Arc<dyn Indicator>) {
        self.lock_write().push(indicator);
    }

    /// Number of registered indicators.
    pub fn len(&self) -> usize {
        self.lock_read().len()
    }

    /// Reports whether no indicators are registered.
    pub fn is_empty(&self) -> bool {
        self.lock_read().is_empty()
    }

    /// Runs every registered indicator (sequentially, like the Go
    /// composite) and returns the overall status plus a per-indicator
    /// map. Each result is stamped with its check duration and the UTC
    /// wall-clock instant at which the check started — exactly what the
    /// Go `CheckAll` writes into `Duration` and `Time`.
    pub async fn check_all(&self) -> (Status, BTreeMap<String, HealthResult>) {
        let indicators: Vec<Arc<dyn Indicator>> = self.lock_read().clone();

        let mut out = BTreeMap::new();
        let mut overall = Status::Up;
        for indicator in indicators {
            let wall_start = Utc::now();
            let start = Instant::now();
            let mut result = indicator.check().await;
            result.duration = start.elapsed();
            result.time = wall_start;
            match result.status {
                Status::Down => overall = Status::Down,
                Status::Degraded => {
                    if overall != Status::Down {
                        overall = Status::Degraded;
                    }
                }
                Status::Up | Status::Unknown => {}
            }
            out.insert(indicator.name().to_string(), result);
        }
        (overall, out)
    }

    fn lock_read(&self) -> std::sync::RwLockReadGuard<'_, Vec<Arc<dyn Indicator>>> {
        self.indicators
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Arc<dyn Indicator>>> {
        self.indicators
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_to_uppercase_wire_names() {
        assert_eq!(serde_json::to_string(&Status::Up).unwrap(), "\"UP\"");
        assert_eq!(serde_json::to_string(&Status::Down).unwrap(), "\"DOWN\"");
        assert_eq!(
            serde_json::to_string(&Status::Degraded).unwrap(),
            "\"DEGRADED\""
        );
        assert_eq!(
            serde_json::to_string(&Status::Unknown).unwrap(),
            "\"UNKNOWN\""
        );
        assert_eq!(Status::Degraded.to_string(), "DEGRADED");
        assert_eq!(Status::Up.as_str(), "UP");
    }

    #[test]
    fn status_deserializes_from_wire_names() {
        let s: Status = serde_json::from_str("\"DEGRADED\"").unwrap();
        assert_eq!(s, Status::Degraded);
    }

    #[test]
    fn health_result_serde_round_trip() {
        let original = HealthResult::degraded("slow")
            .with_detail("latencyMs", 12)
            .with_message("slow");
        let json = serde_json::to_string(&original).unwrap();
        let back: HealthResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn health_result_omits_empty_message_and_details() {
        let value = serde_json::to_value(HealthResult::up()).unwrap();
        let obj = value.as_object().unwrap();
        assert!(!obj.contains_key("message"));
        assert!(!obj.contains_key("details"));
        assert_eq!(obj["status"], "UP");
        // duration is integer nanoseconds, like Go's time.Duration.
        assert!(obj["duration"].is_i64() || obj["duration"].is_u64());
        assert!(obj["time"].is_string());
    }

    #[tokio::test]
    async fn composite_overall_up_when_all_up() {
        let c = Composite::new();
        c.add(IndicatorFn::new("db", || async { HealthResult::up() }));
        c.add(IndicatorFn::new("cache", || async { HealthResult::up() }));
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, Status::Up);
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn composite_unknown_does_not_degrade_overall() {
        let c = Composite::new();
        c.add(IndicatorFn::new("a", || async { HealthResult::unknown() }));
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, Status::Up);
        assert_eq!(results["a"].status, Status::Unknown);
    }

    #[tokio::test]
    async fn composite_down_wins_even_when_degraded_comes_later() {
        let c = Composite::new();
        c.add(IndicatorFn::new("a", || async {
            HealthResult::down("dead")
        }));
        c.add(IndicatorFn::new("b", || async {
            HealthResult::degraded("meh")
        }));
        let (overall, _) = c.check_all().await;
        assert_eq!(overall, Status::Down);
    }

    #[tokio::test]
    async fn composite_empty_is_up() {
        let c = Composite::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, Status::Up);
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn composite_shared_as_arc_across_tasks() {
        let c = Arc::new(Composite::new());
        let c2 = Arc::clone(&c);
        tokio::spawn(async move {
            c2.add(IndicatorFn::new("db", || async { HealthResult::up() }));
        })
        .await
        .unwrap();
        assert_eq!(c.len(), 1);
        let (overall, _) = c.check_all().await;
        assert_eq!(overall, Status::Up);
    }
}
