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

//! Process & system meters with Micrometer/Spring Boot names — the Rust
//! port of pyfly's `pyfly.observability.process_metrics`.
//!
//! Spring Boot auto-instruments a JVM with `process.uptime`,
//! `system.cpu.count` (etc.); pyfly emits the closest stdlib equivalents
//! under the SAME Prometheus names so Spring Boot dashboards/alerts work.
//! This port covers the cross-port core — uptime, start time and CPU
//! count — using the `sysinfo` crate to read the real OS process start
//! time (pyfly's psutil refinement) with an import-time fallback:
//!
//! * `process_uptime_seconds` — the uptime of the process
//! * `process_start_time_seconds` — start time since the unix epoch
//! * `system_cpu_count` — the number of processors available

use std::time::{SystemTime, UNIX_EPOCH};

use sysinfo::{get_current_pid, ProcessesToUpdate, System};

use crate::metrics::MetricsRegistry;

/// Micrometer-named gauge: the uptime of the process.
pub const PROCESS_UPTIME_SECONDS: &str = "process_uptime_seconds";
/// Micrometer-named gauge: start time of the process since the unix epoch.
pub const PROCESS_START_TIME_SECONDS: &str = "process_start_time_seconds";
/// Micrometer-named gauge: the number of processors available.
pub const SYSTEM_CPU_COUNT: &str = "system_cpu_count";

fn now_epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Emits Micrometer-named process/system gauges — the Rust port of
/// pyfly's `ProcessMetricsCollector` (prometheus_client collector).
///
/// Construction reads the real OS process start time via `sysinfo`
/// (pyfly: psutil's `Process.create_time()`), falling back to the
/// construction instant when the platform refuses (pyfly: module import
/// time). Call [`ProcessMetricsCollector::collect`] to refresh the gauges
/// in a [`MetricsRegistry`]; values are recomputed on every call, like a
/// pull-model Prometheus collector.
///
/// ```
/// use firefly_observability::{MetricsRegistry, ProcessMetricsCollector};
///
/// let registry = MetricsRegistry::isolated();
/// let collector = ProcessMetricsCollector::new();
/// collector.collect(&registry);
/// assert!(collector.cpu_count() >= 1);
/// assert!(registry.prometheus_text().contains("process_uptime_seconds"));
/// ```
#[derive(Debug, Clone)]
pub struct ProcessMetricsCollector {
    start_epoch_seconds: f64,
    cpu_count: usize,
}

impl Default for ProcessMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMetricsCollector {
    /// Creates a collector, sampling the process start time and CPU count
    /// once (both are immutable for the life of the process).
    pub fn new() -> Self {
        Self {
            start_epoch_seconds: Self::os_start_time().unwrap_or_else(now_epoch_seconds),
            cpu_count: Self::os_cpu_count(),
        }
    }

    /// The real OS start time of the current process, in epoch seconds.
    fn os_start_time() -> Option<f64> {
        let pid = get_current_pid().ok()?;
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        let start = sys.process(pid)?.start_time();
        (start > 0).then_some(start as f64)
    }

    /// The number of logical processors available to the process.
    fn os_cpu_count() -> usize {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
            .max(1)
    }

    /// Start time of the process since the unix epoch, in seconds —
    /// the value of `process_start_time_seconds`.
    pub fn start_time_seconds(&self) -> f64 {
        self.start_epoch_seconds
    }

    /// The uptime of the process, in seconds — the value of
    /// `process_uptime_seconds` (pyfly: `now - _START_EPOCH`, clamped ≥ 0).
    pub fn uptime_seconds(&self) -> f64 {
        (now_epoch_seconds() - self.start_epoch_seconds).max(0.0)
    }

    /// The number of processors — the value of `system_cpu_count`.
    pub fn cpu_count(&self) -> usize {
        self.cpu_count
    }

    /// Writes the current values into `registry` as Micrometer-named
    /// gauges. Call on each scrape (the analog of prometheus_client
    /// invoking `collect()` on registered collectors).
    pub fn collect(&self, registry: &MetricsRegistry) {
        registry
            .gauge(PROCESS_UPTIME_SECONDS, "The uptime of the process", &[])
            .set(self.uptime_seconds());
        registry
            .gauge(
                PROCESS_START_TIME_SECONDS,
                "Start time of the process since unix epoch",
                &[],
            )
            .set(self.start_time_seconds());
        registry
            .gauge(
                SYSTEM_CPU_COUNT,
                "The number of processors available to the process",
                &[],
            )
            .set(self.cpu_count as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_is_plausible() {
        let c = ProcessMetricsCollector::new();
        let now = now_epoch_seconds();
        assert!(c.start_time_seconds() > 0.0);
        assert!(
            c.start_time_seconds() <= now + 1.0,
            "start {} vs now {now}",
            c.start_time_seconds()
        );
    }

    #[test]
    fn uptime_is_non_negative_and_grows_from_start() {
        let c = ProcessMetricsCollector::new();
        let uptime = c.uptime_seconds();
        assert!(uptime >= 0.0, "uptime {uptime}");
        // The process started before this test ran, so uptime is bounded
        // by (now - start); sanity-check against a day to catch unit bugs.
        assert!(uptime < 86_400.0 * 365.0, "uptime {uptime}");
    }

    #[test]
    fn cpu_count_at_least_one() {
        assert!(ProcessMetricsCollector::new().cpu_count() >= 1);
    }

    #[test]
    fn collect_writes_micrometer_named_gauges() {
        let registry = MetricsRegistry::isolated();
        let c = ProcessMetricsCollector::new();
        c.collect(&registry);
        let text = registry.prometheus_text();
        for name in [
            PROCESS_UPTIME_SECONDS,
            PROCESS_START_TIME_SECONDS,
            SYSTEM_CPU_COUNT,
        ] {
            assert!(text.contains(name), "missing {name} in:\n{text}");
        }
        let cpus = registry.gauge(SYSTEM_CPU_COUNT, "", &[]).value();
        assert!(cpus >= 1.0);
    }
}
