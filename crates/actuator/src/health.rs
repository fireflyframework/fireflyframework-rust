//! Composite health model consulted by `GET /actuator/health`.
//!
//! Mirrors the Go port's `observability` health primitives — [`HealthStatus`],
//! [`HealthResult`], the [`HealthIndicator`] probe trait, and the
//! [`HealthComposite`] aggregator — adapted to async Rust: indicators are
//! `async_trait` probes awaited sequentially, exactly as the Go composite
//! runs its indicators one by one.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Canonical Firefly health states. Wire-compatible with the Java
/// `HealthIndicator.Status`, the .NET `HealthStatus` enum, and the Go
/// `observability.Status` string — every port emits `"UP"`, `"DOWN"`,
/// `"DEGRADED"`, or `"UNKNOWN"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HealthStatus {
    /// The component is fully operational.
    Up,
    /// The component is unavailable; `/actuator/health` answers 503.
    Down,
    /// The component works but with reduced capability; still 200.
    Degraded,
    /// The component's state could not be determined.
    Unknown,
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HealthStatus::Up => "UP",
            HealthStatus::Down => "DOWN",
            HealthStatus::Degraded => "DEGRADED",
            HealthStatus::Unknown => "UNKNOWN",
        })
    }
}

/// The value returned by a [`HealthIndicator`] check.
///
/// Serializes to the exact JSON shape the Go port emits: `status`,
/// `message` (omitted when empty), `details` (omitted when absent),
/// `duration` (integer nanoseconds, like Go's `time.Duration`), and
/// `time` (RFC 3339 UTC timestamp of when the check started).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResult {
    /// Outcome of the check.
    pub status: HealthStatus,
    /// Optional human-readable detail; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Optional structured payload; omitted from JSON when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
    /// How long the check took. Serialized as integer nanoseconds, the
    /// same wire encoding Go gives `time.Duration`.
    #[serde(with = "duration_nanos")]
    pub duration: Duration,
    /// Wall-clock instant at which the check started (UTC).
    pub time: DateTime<Utc>,
}

impl HealthResult {
    /// Returns a result with the given status, no message, no details.
    pub fn new(status: HealthStatus) -> Self {
        Self {
            status,
            message: String::new(),
            details: None,
            duration: Duration::ZERO,
            time: Utc::now(),
        }
    }

    /// Convenience constructor for [`HealthStatus::Up`].
    pub fn up() -> Self {
        Self::new(HealthStatus::Up)
    }

    /// Convenience constructor for [`HealthStatus::Down`] with a message.
    pub fn down(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::new(HealthStatus::Down)
        }
    }

    /// Convenience constructor for [`HealthStatus::Degraded`] with a message.
    pub fn degraded(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::new(HealthStatus::Degraded)
        }
    }

    /// Convenience constructor for [`HealthStatus::Unknown`].
    pub fn unknown() -> Self {
        Self::new(HealthStatus::Unknown)
    }

    /// Attaches a structured details payload (builder-style).
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Map<String, serde_json::Value>) -> Self {
        self.details = Some(details);
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

/// A single named health probe, the async analog of Go's
/// `observability.Indicator` interface.
#[async_trait]
pub trait HealthIndicator: Send + Sync {
    /// Stable name under which the result is reported on `/actuator/health`.
    fn name(&self) -> &str;
    /// Runs the probe and reports its outcome.
    async fn check(&self) -> HealthResult;
}

/// Adapts a plain async closure to the [`HealthIndicator`] trait — the
/// counterpart of Go's `observability.IndicatorFunc`.
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
impl<F, Fut> HealthIndicator for IndicatorFn<F>
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
/// the same precedence the Go `observability.Composite` applies.
#[derive(Default)]
pub struct HealthComposite {
    indicators: RwLock<Vec<Arc<dyn HealthIndicator>>>,
}

impl HealthComposite {
    /// Returns an empty composite (overall status `UP`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an indicator.
    pub fn add<I: HealthIndicator + 'static>(&self, indicator: I) {
        self.add_arc(Arc::new(indicator));
    }

    /// Registers an already-shared indicator.
    pub fn add_arc(&self, indicator: Arc<dyn HealthIndicator>) {
        self.indicators
            .write()
            .expect("health indicator lock poisoned")
            .push(indicator);
    }

    /// Runs every registered indicator (sequentially, like the Go
    /// composite) and returns the overall status plus a per-indicator
    /// map. Each result is stamped with its check duration and the
    /// UTC wall-clock instant at which the check started.
    pub async fn check_all(&self) -> (HealthStatus, BTreeMap<String, HealthResult>) {
        let indicators: Vec<Arc<dyn HealthIndicator>> = self
            .indicators
            .read()
            .expect("health indicator lock poisoned")
            .clone();

        let mut out = BTreeMap::new();
        let mut overall = HealthStatus::Up;
        for indicator in indicators {
            let wall_start = Utc::now();
            let start = Instant::now();
            let mut result = indicator.check().await;
            result.duration = start.elapsed();
            result.time = wall_start;
            match result.status {
                HealthStatus::Down => overall = HealthStatus::Down,
                HealthStatus::Degraded => {
                    if overall != HealthStatus::Down {
                        overall = HealthStatus::Degraded;
                    }
                }
                HealthStatus::Up | HealthStatus::Unknown => {}
            }
            out.insert(indicator.name().to_string(), result);
        }
        (overall, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_to_uppercase_wire_names() {
        assert_eq!(serde_json::to_string(&HealthStatus::Up).unwrap(), "\"UP\"");
        assert_eq!(
            serde_json::to_string(&HealthStatus::Down).unwrap(),
            "\"DOWN\""
        );
        assert_eq!(
            serde_json::to_string(&HealthStatus::Degraded).unwrap(),
            "\"DEGRADED\""
        );
        assert_eq!(
            serde_json::to_string(&HealthStatus::Unknown).unwrap(),
            "\"UNKNOWN\""
        );
        assert_eq!(HealthStatus::Degraded.to_string(), "DEGRADED");
    }

    #[test]
    fn health_result_serde_round_trip() {
        let mut details = serde_json::Map::new();
        details.insert("latencyMs".into(), serde_json::json!(12));
        let original = HealthResult {
            status: HealthStatus::Degraded,
            message: "slow".into(),
            details: Some(details),
            duration: Duration::from_millis(7),
            time: Utc::now(),
        };
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
        let c = HealthComposite::new();
        c.add(IndicatorFn::new("db", || async { HealthResult::up() }));
        c.add(IndicatorFn::new("cache", || async { HealthResult::up() }));
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, HealthStatus::Up);
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn composite_down_wins_over_degraded() {
        let c = HealthComposite::new();
        c.add(IndicatorFn::new("a", || async {
            HealthResult::degraded("meh")
        }));
        c.add(IndicatorFn::new("b", || async {
            HealthResult::down("dead")
        }));
        c.add(IndicatorFn::new("c", || async {
            HealthResult::degraded("meh")
        }));
        let (overall, _) = c.check_all().await;
        assert_eq!(overall, HealthStatus::Down);
    }

    #[tokio::test]
    async fn composite_degraded_wins_over_up() {
        let c = HealthComposite::new();
        c.add(IndicatorFn::new("a", || async { HealthResult::up() }));
        c.add(IndicatorFn::new("b", || async {
            HealthResult::degraded("slow")
        }));
        let (overall, _) = c.check_all().await;
        assert_eq!(overall, HealthStatus::Degraded);
    }

    #[tokio::test]
    async fn composite_unknown_does_not_degrade_overall() {
        let c = HealthComposite::new();
        c.add(IndicatorFn::new("a", || async { HealthResult::unknown() }));
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, HealthStatus::Up);
        assert_eq!(results["a"].status, HealthStatus::Unknown);
    }

    #[tokio::test]
    async fn composite_empty_is_up() {
        let c = HealthComposite::new();
        let (overall, results) = c.check_all().await;
        assert_eq!(overall, HealthStatus::Up);
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn composite_stamps_duration_and_time() {
        let before = Utc::now();
        let c = HealthComposite::new();
        c.add(IndicatorFn::new("db", || async { HealthResult::up() }));
        let (_, results) = c.check_all().await;
        let r = &results["db"];
        assert!(r.time >= before);
        assert!(r.time <= Utc::now());
    }
}
