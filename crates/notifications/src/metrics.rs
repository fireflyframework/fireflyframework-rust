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

//! Metric hooks for the notification send path (pyfly parity).
//!
//! The Rust counterpart of pyfly's metric wiring in
//! `pyfly.notifications.services`. Where pyfly creates three Prometheus
//! counters in each service's `__init__` and calls
//! `counter.labels(...).inc()`, the Rust services accept an optional
//! `Arc<dyn NotificationMetrics>` and call the matching hook method.
//!
//! The three counters are:
//!
//! * `pyfly_notifications_sent_total` (labels `channel`, `provider`) — on SENT;
//! * `pyfly_notifications_failed_total` (labels `channel`, `provider`) — on FAILED;
//! * `pyfly_notifications_suppressed_total` (label `channel`) — per suppressed
//!   recipient, and on a fully-suppressed send.
//!
//! Implementors wire these to whatever registry they use (e.g.
//! firefly-actuator's `MetricRegistry`); the supplied
//! [`InMemoryNotificationMetrics`] records label maps for tests, mirroring the
//! pyfly `FakeMetricsRecorder`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Hook invoked by the default services on each send outcome.
///
/// All methods default to no-ops so implementors only override what they need.
/// The `channel` is one of `"email"`, `"sms"`, `"push"`; `provider` is the
/// provider name from the [`NotificationResult`](crate::NotificationResult).
pub trait NotificationMetrics: Send + Sync {
    /// Records one successful (`SENT`) send for `channel` / `provider`.
    fn record_sent(&self, channel: &str, provider: &str) {
        let _ = (channel, provider);
    }

    /// Records one failed (`FAILED`) send for `channel` / `provider`.
    fn record_failed(&self, channel: &str, provider: &str) {
        let _ = (channel, provider);
    }

    /// Records one suppressed recipient / send for `channel`.
    fn record_suppressed(&self, channel: &str) {
        let _ = channel;
    }
}

/// A counter handle recording the label maps of every increment.
///
/// The Rust counterpart of pyfly's test `FakeCounter`.
#[derive(Default)]
struct RecordingCounter {
    calls: Vec<HashMap<String, String>>,
}

/// In-memory [`NotificationMetrics`] that records each increment's labels.
///
/// Mirrors the pyfly `FakeMetricsRecorder`: query
/// [`sent_calls`](InMemoryNotificationMetrics::sent_calls),
/// [`failed_calls`](InMemoryNotificationMetrics::failed_calls), and
/// [`suppressed_calls`](InMemoryNotificationMetrics::suppressed_calls) to assert
/// which labels were incremented.
#[derive(Default)]
pub struct InMemoryNotificationMetrics {
    sent: Mutex<RecordingCounter>,
    failed: Mutex<RecordingCounter>,
    suppressed: Mutex<RecordingCounter>,
}

impl InMemoryNotificationMetrics {
    /// Returns a fresh, empty metrics recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the label map of every `sent` increment, in order.
    pub fn sent_calls(&self) -> Vec<HashMap<String, String>> {
        self.sent
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .clone()
    }

    /// Returns the label map of every `failed` increment, in order.
    pub fn failed_calls(&self) -> Vec<HashMap<String, String>> {
        self.failed
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .clone()
    }

    /// Returns the label map of every `suppressed` increment, in order.
    pub fn suppressed_calls(&self) -> Vec<HashMap<String, String>> {
        self.suppressed
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .clone()
    }
}

impl NotificationMetrics for InMemoryNotificationMetrics {
    fn record_sent(&self, channel: &str, provider: &str) {
        self.sent
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .push(HashMap::from([
                ("channel".to_string(), channel.to_string()),
                ("provider".to_string(), provider.to_string()),
            ]));
    }

    fn record_failed(&self, channel: &str, provider: &str) {
        self.failed
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .push(HashMap::from([
                ("channel".to_string(), channel.to_string()),
                ("provider".to_string(), provider.to_string()),
            ]));
    }

    fn record_suppressed(&self, channel: &str) {
        self.suppressed
            .lock()
            .expect("metrics lock poisoned")
            .calls
            .push(HashMap::from([(
                "channel".to_string(),
                channel.to_string(),
            )]));
    }
}
