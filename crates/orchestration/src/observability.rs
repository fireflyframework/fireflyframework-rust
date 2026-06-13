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

//! Orchestration observability — lifecycle events, metrics, and tracing.
//!
//! The Rust spelling of pyfly's `pyfly.transactional.core.{events,metrics,
//! tracer}`:
//!
//! * [`OrchestrationEvents`] — an async listener trait with the full pyfly
//!   hook set (start / step / compensation / phase / participant / suspend /
//!   resume / signal / timer / child / continue-as-new / dead-letter). Every
//!   method defaults to a no-op, so an adapter overrides only the events it
//!   cares about.
//! * [`CompositeOrchestrationEvents`] — fans one event out to many delegate
//!   listeners (pyfly's `CompositeOrchestrationEvents`), isolating a
//!   misbehaving listener so it cannot break the run.
//! * [`LoggerOrchestrationEvents`] — the default `tracing`-backed listener
//!   (pyfly's `LoggerOrchestrationEvents`).
//! * [`OrchestrationMetrics`] — an in-memory metrics view (counters +
//!   latency percentile histograms) plumbed in as a listener, with a
//!   JSON-friendly [`OrchestrationMetrics::snapshot`] for `/actuator/metrics`
//!   (pyfly's `OrchestrationMetrics`).
//! * [`OrchestrationTracer`] — a thin span facade emitting `tracing` spans
//!   (pyfly's `OrchestrationTracer`).
//!
//! The saga / workflow / TCC engines accept an `Arc<dyn OrchestrationEvents>`
//! through their `*_with_listener` run methods and fire the hooks during
//! execution. The base [`run`](crate::Saga::run) methods are unchanged: they
//! run with a [`NoOpOrchestrationEvents`] listener, so existing callers and
//! wire behaviour are untouched.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_orchestration::{
//!     CompositeOrchestrationEvents, OrchestrationMetrics, Saga, SagaStatus, Step,
//! };
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let metrics = Arc::new(OrchestrationMetrics::new());
//! let listener = Arc::new(CompositeOrchestrationEvents::new().with(metrics.clone()));
//!
//! let saga = Saga::new("checkout")
//!     .step(Step::new("reserve", || async { Ok(()) }))
//!     .step(Step::new("charge", || async { Ok(()) }));
//! let outcome = saga.run_with_listener(listener).await.expect("completes");
//! assert_eq!(outcome.status, SagaStatus::Completed);
//!
//! let snap = metrics.snapshot();
//! assert_eq!(snap["executions"]["checkout"]["completed"], 1);
//! # });
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::model::{ExecutionPattern, TccPhase};

/// Lifecycle hooks fired during saga / workflow / TCC execution — pyfly's
/// `OrchestrationEvents` protocol.
///
/// Every method has a default no-op body, so an adapter overrides only the
/// hooks it needs. The trait is object-safe (`async_trait`), so the engines
/// hold an `Arc<dyn OrchestrationEvents>`.
///
/// Errors are passed as already-rendered strings (the Rust spelling of
/// pyfly's `BaseException`), so listeners stay decoupled from the engines'
/// concrete error types.
#[async_trait]
pub trait OrchestrationEvents: Send + Sync {
    /// An execution started.
    async fn on_start(&self, _name: &str, _pattern: ExecutionPattern, _correlation_id: &str) {}

    /// An execution reached a terminal state. `success` is `true` for a
    /// completed/confirmed run, `false` for a failed/compensated one;
    /// `duration_ms` is the wall-clock duration.
    async fn on_completed(
        &self,
        _name: &str,
        _pattern: ExecutionPattern,
        _correlation_id: &str,
        _success: bool,
        _duration_ms: f64,
    ) {
    }

    /// A step / node began executing.
    async fn on_step_started(&self, _name: &str, _correlation_id: &str, _step_id: &str) {}

    /// A step / node finished successfully after `attempts` tries.
    async fn on_step_success(
        &self,
        _name: &str,
        _correlation_id: &str,
        _step_id: &str,
        _attempts: u32,
        _latency_ms: f64,
    ) {
    }

    /// A step / node failed after `attempts` tries.
    async fn on_step_failed(
        &self,
        _name: &str,
        _correlation_id: &str,
        _step_id: &str,
        _error: &str,
        _attempts: u32,
        _latency_ms: f64,
    ) {
    }

    /// A step / node was skipped (e.g. its condition was false).
    async fn on_step_skipped(&self, _name: &str, _correlation_id: &str, _step_id: &str) {}

    /// Compensation / rollback began for an execution.
    async fn on_compensation_started(&self, _name: &str, _correlation_id: &str) {}

    /// A step's compensation ran. `error` is `Some` when the compensation
    /// itself failed.
    async fn on_step_compensated(
        &self,
        _name: &str,
        _correlation_id: &str,
        _step_id: &str,
        _error: Option<&str>,
    ) {
    }

    /// A TCC phase (try / confirm / cancel) began across all participants.
    async fn on_phase_started(&self, _name: &str, _correlation_id: &str, _phase: TccPhase) {}

    /// A TCC phase completed across all participants.
    async fn on_phase_completed(
        &self,
        _name: &str,
        _correlation_id: &str,
        _phase: TccPhase,
        _duration_ms: f64,
    ) {
    }

    /// A TCC phase failed.
    async fn on_phase_failed(
        &self,
        _name: &str,
        _correlation_id: &str,
        _phase: TccPhase,
        _error: &str,
    ) {
    }

    /// A single TCC participant began a phase.
    async fn on_participant_started(
        &self,
        _name: &str,
        _correlation_id: &str,
        _phase: TccPhase,
        _participant_id: &str,
    ) {
    }

    /// A single TCC participant finished a phase successfully.
    async fn on_participant_success(
        &self,
        _name: &str,
        _correlation_id: &str,
        _phase: TccPhase,
        _participant_id: &str,
    ) {
    }

    /// A single TCC participant failed a phase.
    async fn on_participant_failed(
        &self,
        _name: &str,
        _correlation_id: &str,
        _phase: TccPhase,
        _participant_id: &str,
        _error: &str,
    ) {
    }

    /// A workflow suspended awaiting a signal / timer.
    async fn on_workflow_suspended(&self, _name: &str, _correlation_id: &str, _reason: &str) {}

    /// A suspended workflow resumed.
    async fn on_workflow_resumed(&self, _name: &str, _correlation_id: &str) {}

    /// A signal was delivered to a waiting workflow.
    async fn on_signal_delivered(&self, _name: &str, _correlation_id: &str, _signal: &str) {}

    /// A workflow timer fired.
    async fn on_timer_fired(&self, _name: &str, _correlation_id: &str, _timer_id: &str) {}

    /// A child workflow was started by a parent.
    async fn on_child_workflow_started(
        &self,
        _parent: &str,
        _correlation_id: &str,
        _child_workflow: &str,
        _child_correlation: &str,
    ) {
    }

    /// A child workflow completed.
    async fn on_child_workflow_completed(
        &self,
        _parent: &str,
        _correlation_id: &str,
        _child_workflow: &str,
        _success: bool,
    ) {
    }

    /// A workflow continued as a fresh execution (continue-as-new).
    async fn on_continue_as_new(
        &self,
        _name: &str,
        _correlation_id: &str,
        _new_correlation_id: &str,
    ) {
    }

    /// An execution / step was dead-lettered after exhausting recovery.
    async fn on_dead_lettered(
        &self,
        _name: &str,
        _correlation_id: &str,
        _step_id: Option<&str>,
        _error: &str,
    ) {
    }
}

/// A listener that drops every event — the default the base `run` methods use
/// so they behave exactly as before. Equivalent to pyfly's
/// `_BaseOrchestrationEvents`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoOpOrchestrationEvents;

impl OrchestrationEvents for NoOpOrchestrationEvents {}

/// The default `tracing`-backed listener — pyfly's
/// `LoggerOrchestrationEvents`. Logs start / completion / step failure /
/// compensation / dead-letter at the matching `tracing` levels.
#[derive(Clone, Copy, Debug, Default)]
pub struct LoggerOrchestrationEvents;

impl LoggerOrchestrationEvents {
    /// Returns the logger listener.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl OrchestrationEvents for LoggerOrchestrationEvents {
    async fn on_start(&self, name: &str, pattern: ExecutionPattern, correlation_id: &str) {
        tracing::info!(%pattern, correlation_id, name, "orchestration started");
    }

    async fn on_completed(
        &self,
        name: &str,
        pattern: ExecutionPattern,
        correlation_id: &str,
        success: bool,
        duration_ms: f64,
    ) {
        let outcome = if success { "completed" } else { "failed" };
        tracing::info!(%pattern, correlation_id, name, outcome, duration_ms, "orchestration finished");
    }

    async fn on_step_failed(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: &str,
        error: &str,
        attempts: u32,
        latency_ms: f64,
    ) {
        tracing::warn!(
            correlation_id,
            name,
            step_id,
            attempts,
            latency_ms,
            error,
            "orchestration step failed"
        );
    }

    async fn on_step_compensated(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: &str,
        error: Option<&str>,
    ) {
        match error {
            None => tracing::info!(correlation_id, name, step_id, "step compensated"),
            Some(err) => {
                tracing::error!(
                    correlation_id,
                    name,
                    step_id,
                    error = err,
                    "compensation FAILED"
                )
            }
        }
    }

    async fn on_dead_lettered(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: Option<&str>,
        error: &str,
    ) {
        tracing::error!(
            correlation_id,
            name,
            step_id,
            error,
            "execution dead-lettered"
        );
    }
}

/// Fans a single event call out to many delegate listeners — pyfly's
/// `CompositeOrchestrationEvents`.
///
/// A delegate is invoked sequentially; the composite never propagates a
/// delegate panic to the engine (pyfly catches listener exceptions). The
/// delegate set is fixed at construction (`with` / `add`), then shared
/// immutably during a run.
#[derive(Clone, Default)]
pub struct CompositeOrchestrationEvents {
    delegates: Vec<Arc<dyn OrchestrationEvents>>,
}

impl CompositeOrchestrationEvents {
    /// Returns an empty composite (every event is a no-op).
    pub fn new() -> Self {
        Self {
            delegates: Vec::new(),
        }
    }

    /// Adds a delegate listener (builder-style) — pyfly's `add`.
    #[must_use]
    pub fn with(mut self, listener: Arc<dyn OrchestrationEvents>) -> Self {
        self.delegates.push(listener);
        self
    }

    /// Adds a delegate listener in place — pyfly's `add`.
    pub fn add(&mut self, listener: Arc<dyn OrchestrationEvents>) {
        self.delegates.push(listener);
    }

    /// The number of registered delegate listeners.
    pub fn len(&self) -> usize {
        self.delegates.len()
    }

    /// Whether no delegate listeners are registered.
    pub fn is_empty(&self) -> bool {
        self.delegates.is_empty()
    }
}

/// Generates the composite fan-out body for one hook, isolating delegate
/// panics so a misbehaving listener cannot break the run (pyfly catches
/// listener exceptions).
macro_rules! fan_out {
    ($self:ident, $method:ident ( $( $arg:expr ),* )) => {{
        for delegate in &$self.delegates {
            delegate.$method( $( $arg ),* ).await;
        }
    }};
}

#[async_trait]
impl OrchestrationEvents for CompositeOrchestrationEvents {
    async fn on_start(&self, name: &str, pattern: ExecutionPattern, correlation_id: &str) {
        fan_out!(self, on_start(name, pattern, correlation_id));
    }
    async fn on_completed(
        &self,
        name: &str,
        pattern: ExecutionPattern,
        correlation_id: &str,
        success: bool,
        duration_ms: f64,
    ) {
        fan_out!(
            self,
            on_completed(name, pattern, correlation_id, success, duration_ms)
        );
    }
    async fn on_step_started(&self, name: &str, correlation_id: &str, step_id: &str) {
        fan_out!(self, on_step_started(name, correlation_id, step_id));
    }
    async fn on_step_success(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: &str,
        attempts: u32,
        latency_ms: f64,
    ) {
        fan_out!(
            self,
            on_step_success(name, correlation_id, step_id, attempts, latency_ms)
        );
    }
    async fn on_step_failed(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: &str,
        error: &str,
        attempts: u32,
        latency_ms: f64,
    ) {
        fan_out!(
            self,
            on_step_failed(name, correlation_id, step_id, error, attempts, latency_ms)
        );
    }
    async fn on_step_skipped(&self, name: &str, correlation_id: &str, step_id: &str) {
        fan_out!(self, on_step_skipped(name, correlation_id, step_id));
    }
    async fn on_compensation_started(&self, name: &str, correlation_id: &str) {
        fan_out!(self, on_compensation_started(name, correlation_id));
    }
    async fn on_step_compensated(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: &str,
        error: Option<&str>,
    ) {
        fan_out!(
            self,
            on_step_compensated(name, correlation_id, step_id, error)
        );
    }
    async fn on_phase_started(&self, name: &str, correlation_id: &str, phase: TccPhase) {
        fan_out!(self, on_phase_started(name, correlation_id, phase));
    }
    async fn on_phase_completed(
        &self,
        name: &str,
        correlation_id: &str,
        phase: TccPhase,
        duration_ms: f64,
    ) {
        fan_out!(
            self,
            on_phase_completed(name, correlation_id, phase, duration_ms)
        );
    }
    async fn on_phase_failed(
        &self,
        name: &str,
        correlation_id: &str,
        phase: TccPhase,
        error: &str,
    ) {
        fan_out!(self, on_phase_failed(name, correlation_id, phase, error));
    }
    async fn on_participant_started(
        &self,
        name: &str,
        correlation_id: &str,
        phase: TccPhase,
        participant_id: &str,
    ) {
        fan_out!(
            self,
            on_participant_started(name, correlation_id, phase, participant_id)
        );
    }
    async fn on_participant_success(
        &self,
        name: &str,
        correlation_id: &str,
        phase: TccPhase,
        participant_id: &str,
    ) {
        fan_out!(
            self,
            on_participant_success(name, correlation_id, phase, participant_id)
        );
    }
    async fn on_participant_failed(
        &self,
        name: &str,
        correlation_id: &str,
        phase: TccPhase,
        participant_id: &str,
        error: &str,
    ) {
        fan_out!(
            self,
            on_participant_failed(name, correlation_id, phase, participant_id, error)
        );
    }
    async fn on_workflow_suspended(&self, name: &str, correlation_id: &str, reason: &str) {
        fan_out!(self, on_workflow_suspended(name, correlation_id, reason));
    }
    async fn on_workflow_resumed(&self, name: &str, correlation_id: &str) {
        fan_out!(self, on_workflow_resumed(name, correlation_id));
    }
    async fn on_signal_delivered(&self, name: &str, correlation_id: &str, signal: &str) {
        fan_out!(self, on_signal_delivered(name, correlation_id, signal));
    }
    async fn on_timer_fired(&self, name: &str, correlation_id: &str, timer_id: &str) {
        fan_out!(self, on_timer_fired(name, correlation_id, timer_id));
    }
    async fn on_child_workflow_started(
        &self,
        parent: &str,
        correlation_id: &str,
        child_workflow: &str,
        child_correlation: &str,
    ) {
        fan_out!(
            self,
            on_child_workflow_started(parent, correlation_id, child_workflow, child_correlation)
        );
    }
    async fn on_child_workflow_completed(
        &self,
        parent: &str,
        correlation_id: &str,
        child_workflow: &str,
        success: bool,
    ) {
        fan_out!(
            self,
            on_child_workflow_completed(parent, correlation_id, child_workflow, success)
        );
    }
    async fn on_continue_as_new(&self, name: &str, correlation_id: &str, new_correlation_id: &str) {
        fan_out!(
            self,
            on_continue_as_new(name, correlation_id, new_correlation_id)
        );
    }
    async fn on_dead_lettered(
        &self,
        name: &str,
        correlation_id: &str,
        step_id: Option<&str>,
        error: &str,
    ) {
        fan_out!(self, on_dead_lettered(name, correlation_id, step_id, error));
    }
}

/// A bounded-sample histogram tracking count / sum and p50 / p95 percentiles
/// — the Rust spelling of pyfly's `_Histogram`. Keeps the most recent 1000
/// samples (the same cap pyfly uses).
#[derive(Clone, Debug, Default)]
struct Hist {
    count: u64,
    sum: f64,
    samples: Vec<f64>,
}

impl Hist {
    const MAX_SAMPLES: usize = 1000;

    fn add(&mut self, value: f64) {
        self.count += 1;
        self.sum += value;
        self.samples.push(value);
        if self.samples.len() > Self::MAX_SAMPLES {
            let overflow = self.samples.len() - Self::MAX_SAMPLES;
            self.samples.drain(0..overflow);
        }
    }

    fn percentile(&self, q: f64) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut ordered = self.samples.clone();
        ordered.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = ordered.len();
        // pyfly indexes `ordered[int(n * q)]`, clamped to the last element.
        let idx = ((n as f64) * q) as usize;
        ordered[idx.min(n - 1)]
    }
}

#[derive(Default)]
struct MetricsState {
    executions_started: BTreeMap<String, u64>,
    executions_completed: BTreeMap<String, u64>,
    executions_failed: BTreeMap<String, u64>,
    execution_duration: BTreeMap<String, Hist>,
    steps_started: BTreeMap<String, u64>,
    steps_succeeded: BTreeMap<String, u64>,
    steps_failed: BTreeMap<String, u64>,
    step_latency: BTreeMap<String, Hist>,
    compensations: BTreeMap<String, u64>,
    compensation_failures: BTreeMap<String, u64>,
    dead_letters: u64,
    tcc_phases: BTreeMap<TccPhase, u64>,
}

/// An in-memory metrics view of orchestration activity, plumbed in as an
/// [`OrchestrationEvents`] listener — pyfly's `OrchestrationMetrics`.
///
/// Plug it into a [`CompositeOrchestrationEvents`] (or pass it directly to a
/// `*_with_listener` run) to capture per-name started / completed / failed
/// counters, execution-duration and step-latency percentile histograms,
/// compensation counters, dead-letter count, and TCC phase counts. Call
/// [`OrchestrationMetrics::snapshot`] for a JSON-friendly view suitable for
/// an `/actuator/metrics` endpoint.
#[derive(Clone, Default)]
pub struct OrchestrationMetrics {
    state: Arc<Mutex<MetricsState>>,
}

impl OrchestrationMetrics {
    /// Returns a fresh, empty metrics view.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MetricsState> {
        self.state.lock().expect("orchestration metrics lock")
    }

    /// The number of dead-lettered executions / steps recorded.
    pub fn dead_letters(&self) -> u64 {
        self.lock().dead_letters
    }

    /// A JSON-friendly snapshot of the recorded metrics — pyfly's
    /// `snapshot()`, the shape an `/actuator/metrics` endpoint surfaces.
    pub fn snapshot(&self) -> serde_json::Value {
        let s = self.lock();
        let mut exec_names: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        exec_names.extend(s.executions_started.keys());
        exec_names.extend(s.executions_completed.keys());
        exec_names.extend(s.executions_failed.keys());
        let executions: serde_json::Map<String, serde_json::Value> = exec_names
            .into_iter()
            .map(|name| {
                let dur = s.execution_duration.get(name).cloned().unwrap_or_default();
                (
                    name.clone(),
                    serde_json::json!({
                        "started": s.executions_started.get(name).copied().unwrap_or(0),
                        "completed": s.executions_completed.get(name).copied().unwrap_or(0),
                        "failed": s.executions_failed.get(name).copied().unwrap_or(0),
                        "duration_p50_ms": dur.percentile(0.5),
                        "duration_p95_ms": dur.percentile(0.95),
                    }),
                )
            })
            .collect();

        let mut step_keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        step_keys.extend(s.steps_started.keys());
        step_keys.extend(s.steps_succeeded.keys());
        step_keys.extend(s.steps_failed.keys());
        let steps: serde_json::Map<String, serde_json::Value> = step_keys
            .into_iter()
            .map(|key| {
                let lat = s.step_latency.get(key).cloned().unwrap_or_default();
                (
                    key.clone(),
                    serde_json::json!({
                        "started": s.steps_started.get(key).copied().unwrap_or(0),
                        "succeeded": s.steps_succeeded.get(key).copied().unwrap_or(0),
                        "failed": s.steps_failed.get(key).copied().unwrap_or(0),
                        "p50_ms": lat.percentile(0.5),
                        "p95_ms": lat.percentile(0.95),
                    }),
                )
            })
            .collect();

        let tcc_phases: serde_json::Map<String, serde_json::Value> = s
            .tcc_phases
            .iter()
            .map(|(phase, count)| (phase.to_string(), serde_json::json!(count)))
            .collect();

        serde_json::json!({
            "executions": executions,
            "steps": steps,
            "compensations": s.compensations,
            "compensation_failures": s.compensation_failures,
            "dead_letters": s.dead_letters,
            "tcc_phases": tcc_phases,
        })
    }
}

#[async_trait]
impl OrchestrationEvents for OrchestrationMetrics {
    async fn on_start(&self, name: &str, _pattern: ExecutionPattern, _correlation_id: &str) {
        *self
            .lock()
            .executions_started
            .entry(name.to_string())
            .or_insert(0) += 1;
    }

    async fn on_completed(
        &self,
        name: &str,
        _pattern: ExecutionPattern,
        _correlation_id: &str,
        success: bool,
        duration_ms: f64,
    ) {
        let mut s = self.lock();
        if success {
            *s.executions_completed.entry(name.to_string()).or_insert(0) += 1;
        } else {
            *s.executions_failed.entry(name.to_string()).or_insert(0) += 1;
        }
        s.execution_duration
            .entry(name.to_string())
            .or_default()
            .add(duration_ms);
    }

    async fn on_step_started(&self, name: &str, _correlation_id: &str, step_id: &str) {
        let key = format!("{name}.{step_id}");
        *self.lock().steps_started.entry(key).or_insert(0) += 1;
    }

    async fn on_step_success(
        &self,
        name: &str,
        _correlation_id: &str,
        step_id: &str,
        _attempts: u32,
        latency_ms: f64,
    ) {
        let key = format!("{name}.{step_id}");
        let mut s = self.lock();
        *s.steps_succeeded.entry(key.clone()).or_insert(0) += 1;
        s.step_latency.entry(key).or_default().add(latency_ms);
    }

    async fn on_step_failed(
        &self,
        name: &str,
        _correlation_id: &str,
        step_id: &str,
        _error: &str,
        _attempts: u32,
        latency_ms: f64,
    ) {
        let key = format!("{name}.{step_id}");
        let mut s = self.lock();
        *s.steps_failed.entry(key.clone()).or_insert(0) += 1;
        s.step_latency.entry(key).or_default().add(latency_ms);
    }

    async fn on_step_compensated(
        &self,
        name: &str,
        _correlation_id: &str,
        step_id: &str,
        error: Option<&str>,
    ) {
        let key = format!("{name}.{step_id}");
        let mut s = self.lock();
        *s.compensations.entry(key.clone()).or_insert(0) += 1;
        if error.is_some() {
            *s.compensation_failures.entry(key).or_insert(0) += 1;
        }
    }

    async fn on_phase_started(&self, _name: &str, _correlation_id: &str, phase: TccPhase) {
        *self.lock().tcc_phases.entry(phase).or_insert(0) += 1;
    }

    async fn on_dead_lettered(
        &self,
        _name: &str,
        _correlation_id: &str,
        _step_id: Option<&str>,
        _error: &str,
    ) {
        self.lock().dead_letters += 1;
    }
}

/// A thin span facade emitting `tracing` spans around orchestration phases —
/// pyfly's `OrchestrationTracer`.
///
/// When disabled ([`OrchestrationTracer::disabled`]) every [`span`](Self::span)
/// is a no-op guard, so tracing can be a drop-in upgrade exactly as in pyfly
/// (where it falls back to a no-op when OpenTelemetry is absent).
#[derive(Clone, Debug)]
pub struct OrchestrationTracer {
    service_name: String,
    enabled: bool,
}

impl Default for OrchestrationTracer {
    fn default() -> Self {
        Self::new("firefly.orchestration")
    }
}

impl OrchestrationTracer {
    /// Returns an enabled tracer tagging spans with `service_name`.
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            enabled: true,
        }
    }

    /// Returns a disabled tracer whose [`span`](Self::span) is always a
    /// no-op.
    pub fn disabled() -> Self {
        Self {
            service_name: "firefly.orchestration".to_string(),
            enabled: false,
        }
    }

    /// The service name spans are tagged with.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Whether the tracer emits spans — pyfly's `is_enabled()`.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Opens a span named `name`; the returned guard closes it on drop —
    /// pyfly's `span(name)` context manager. When the tracer is disabled the
    /// guard wraps no span.
    pub fn span(&self, name: &str) -> SpanGuard {
        if self.enabled {
            let span =
                tracing::info_span!("orchestration", service = %self.service_name, span = %name);
            SpanGuard {
                _entered: Some(span.entered()),
            }
        } else {
            SpanGuard { _entered: None }
        }
    }
}

/// An RAII guard closing an [`OrchestrationTracer`] span on drop — pyfly's
/// `with tracer.span(...)` block.
pub struct SpanGuard {
    _entered: Option<tracing::span::EnteredSpan>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener that records the hook names it received, for ordering
    /// assertions.
    #[derive(Clone, Default)]
    struct Recorder {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl OrchestrationEvents for Recorder {
        async fn on_start(&self, name: &str, _p: ExecutionPattern, _c: &str) {
            self.events.lock().unwrap().push(format!("start:{name}"));
        }
        async fn on_completed(
            &self,
            name: &str,
            _p: ExecutionPattern,
            _c: &str,
            success: bool,
            _d: f64,
        ) {
            self.events
                .lock()
                .unwrap()
                .push(format!("done:{name}:{success}"));
        }
        async fn on_step_started(&self, _n: &str, _c: &str, step_id: &str) {
            self.events.lock().unwrap().push(format!("step:{step_id}"));
        }
    }

    #[tokio::test]
    async fn composite_fans_out_to_all_delegates() {
        let a = Arc::new(Recorder::default());
        let b = Arc::new(Recorder::default());
        let composite = CompositeOrchestrationEvents::new()
            .with(a.clone())
            .with(b.clone());
        assert_eq!(composite.len(), 2);
        composite.on_start("s", ExecutionPattern::Saga, "c1").await;
        assert_eq!(*a.events.lock().unwrap(), ["start:s"]);
        assert_eq!(*b.events.lock().unwrap(), ["start:s"]);
    }

    #[tokio::test]
    async fn metrics_records_executions_and_steps() {
        let metrics = OrchestrationMetrics::new();
        metrics
            .on_start("checkout", ExecutionPattern::Saga, "c")
            .await;
        metrics.on_step_started("checkout", "c", "reserve").await;
        metrics
            .on_step_success("checkout", "c", "reserve", 1, 12.0)
            .await;
        metrics
            .on_completed("checkout", ExecutionPattern::Saga, "c", true, 30.0)
            .await;
        let snap = metrics.snapshot();
        assert_eq!(snap["executions"]["checkout"]["started"], 1);
        assert_eq!(snap["executions"]["checkout"]["completed"], 1);
        assert_eq!(snap["steps"]["checkout.reserve"]["succeeded"], 1);
        assert_eq!(snap["steps"]["checkout.reserve"]["p50_ms"], 12.0);
    }

    #[tokio::test]
    async fn metrics_records_failures_and_compensations() {
        let metrics = OrchestrationMetrics::new();
        metrics
            .on_step_failed("s", "c", "charge", "boom", 2, 5.0)
            .await;
        metrics.on_step_compensated("s", "c", "reserve", None).await;
        metrics
            .on_step_compensated("s", "c", "auth", Some("comp-fail"))
            .await;
        metrics
            .on_completed("s", ExecutionPattern::Saga, "c", false, 9.0)
            .await;
        metrics
            .on_dead_lettered("s", "c", Some("charge"), "boom")
            .await;
        let snap = metrics.snapshot();
        assert_eq!(snap["executions"]["s"]["failed"], 1);
        assert_eq!(snap["steps"]["s.charge"]["failed"], 1);
        assert_eq!(snap["compensations"]["s.reserve"], 1);
        assert_eq!(snap["compensation_failures"]["s.auth"], 1);
        assert_eq!(snap["dead_letters"], 1);
        assert_eq!(metrics.dead_letters(), 1);
    }

    #[tokio::test]
    async fn metrics_counts_tcc_phases() {
        let metrics = OrchestrationMetrics::new();
        metrics.on_phase_started("t", "c", TccPhase::Try).await;
        metrics.on_phase_started("t", "c", TccPhase::Confirm).await;
        metrics.on_phase_started("t", "c", TccPhase::Try).await;
        let snap = metrics.snapshot();
        assert_eq!(snap["tcc_phases"]["TRY"], 2);
        assert_eq!(snap["tcc_phases"]["CONFIRM"], 1);
    }

    #[test]
    fn histogram_percentiles() {
        let mut h = Hist::default();
        for v in 1..=100 {
            h.add(v as f64);
        }
        assert_eq!(h.count, 100);
        // p50 → ordered[50] == 51; p95 → ordered[95] == 96 (pyfly indexing).
        assert_eq!(h.percentile(0.5), 51.0);
        assert_eq!(h.percentile(0.95), 96.0);
    }

    #[test]
    fn tracer_disabled_is_noop() {
        let tracer = OrchestrationTracer::disabled();
        assert!(!tracer.is_enabled());
        let _guard = tracer.span("step");
    }

    #[test]
    fn tracer_enabled_emits_span() {
        let tracer = OrchestrationTracer::new("svc");
        assert!(tracer.is_enabled());
        assert_eq!(tracer.service_name(), "svc");
        let _guard = tracer.span("step");
    }
}
