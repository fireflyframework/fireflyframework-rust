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

//! Typed state-machine circuit breaker.

use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::ResilienceError;

/// `CircuitState` enumerates the canonical states. Transitions:
///
/// ```text
/// Closed   →(failures ≥ threshold, or windowed failure rate ≥ rate)→ Open
/// Open     →(after open duration)→  HalfOpen
/// HalfOpen →(half_open_max_calls successes)→ Closed
/// HalfOpen →(any failure)→          Open
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CircuitState {
    /// Calls flow through; failures are being counted.
    Closed,
    /// Calls are short-circuited with [`ResilienceError::CircuitOpen`].
    Open,
    /// Exactly one trial call is allowed; its outcome decides the next state.
    HalfOpen,
}

impl fmt::Display for CircuitState {
    /// Renders the state in human-readable form — `closed`, `open`, or
    /// `half-open`, byte-identical to the Go port's `String()`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half-open",
        })
    }
}

/// `Clock` is the injectable time source — defaults to [`Instant::now`].
/// Tests substitute a fake clock to drive state transitions deterministically,
/// playing the role of the Go port's `CircuitConfig.Now` hook.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// `CircuitConfig` tunes [`CircuitBreaker::new`].
#[derive(Clone)]
pub struct CircuitConfig {
    /// Number of consecutive failures (or failures within [`window`](Self::window))
    /// that trip the breaker. Zero falls back to 5.
    pub failure_threshold: usize,

    /// Rolling window during which failures are counted.
    /// [`Duration::ZERO`] means consecutive-only counting.
    pub window: Duration,

    /// How long the breaker stays in [`CircuitState::Open`] before a trial
    /// half-open call. Zero falls back to 30 seconds.
    pub open_duration: Duration,

    /// The clock — `None` defaults to [`Instant::now`].
    pub now: Option<Clock>,

    /// When set, the breaker switches from consecutive-failure counting to
    /// pyfly's **count-based failure-rate window** (Resilience4j
    /// `COUNT_BASED`): the outcomes of the last
    /// [`window_size`](Self::window_size) calls are kept in a ring buffer and
    /// the breaker opens once the window is **full** and the failure fraction
    /// reaches this threshold (`0.0..=1.0`). `None` (the default) keeps the
    /// original consecutive / time-window mode for back-compat.
    pub failure_rate_threshold: Option<f64>,

    /// Size of the count-based outcome ring buffer used by
    /// [`failure_rate_threshold`](Self::failure_rate_threshold).
    /// Zero falls back to 10 (the pyfly default).
    pub window_size: usize,

    /// Number of trial calls admitted while [`CircuitState::HalfOpen`]; that
    /// many *successes* close the circuit, any failure re-opens it.
    /// Zero falls back to 1 (the historical single-trial behavior).
    pub half_open_max_calls: usize,
}

impl Default for CircuitConfig {
    /// Returns the 5-failure / 30 s open / 1 s window config — the Rust
    /// analogue of the Go port's `DefaultCircuitConfig()` — with the pyfly
    /// extensions defaulted to consecutive mode (`failure_rate_threshold:
    /// None`, `window_size: 10`, `half_open_max_calls: 1`).
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            window: Duration::from_secs(1),
            open_duration: Duration::from_secs(30),
            now: None,
            failure_rate_threshold: None,
            window_size: 10,
            half_open_max_calls: 1,
        }
    }
}

impl fmt::Debug for CircuitConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitConfig")
            .field("failure_threshold", &self.failure_threshold)
            .field("window", &self.window)
            .field("open_duration", &self.open_duration)
            .field("now", &self.now.as_ref().map_or("<system>", |_| "<custom>"))
            .field("failure_rate_threshold", &self.failure_rate_threshold)
            .field("window_size", &self.window_size)
            .field("half_open_max_calls", &self.half_open_max_calls)
            .finish()
    }
}

/// Mutable breaker state guarded by the mutex.
struct BreakerInner {
    state: CircuitState,
    failures: Vec<Instant>,
    opened_at: Option<Instant>,
    /// Count-based outcome ring (`true` = success), capped at `window_size`.
    outcomes: VecDeque<bool>,
    /// Probes admitted in the current half-open phase.
    half_open_calls: usize,
    /// Probe successes in the current half-open phase.
    half_open_successes: usize,
}

/// `CircuitBreaker` is the typed state-machine breaker — it shields callers
/// from cascading failure of a slow or failing dependency by short-circuiting
/// calls while the downstream is sick.
pub struct CircuitBreaker {
    failure_threshold: usize,
    window: Duration,
    open_duration: Duration,
    failure_rate_threshold: Option<f64>,
    window_size: usize,
    half_open_max_calls: usize,
    now: Clock,
    inner: Mutex<BreakerInner>,
}

impl fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("failure_threshold", &self.failure_threshold)
            .field("window", &self.window)
            .field("open_duration", &self.open_duration)
            .field("failure_rate_threshold", &self.failure_rate_threshold)
            .field("window_size", &self.window_size)
            .field("half_open_max_calls", &self.half_open_max_calls)
            .field("state", &self.state())
            .finish()
    }
}

impl CircuitBreaker {
    /// Returns a closed breaker. A zero `failure_threshold` falls back to 5
    /// and a zero `open_duration` falls back to 30 seconds, mirroring the Go
    /// port's `NewCircuitBreaker` validation; a zero `window_size` falls back
    /// to 10 and a zero `half_open_max_calls` to 1, mirroring pyfly's
    /// constructor defaults.
    pub fn new(cfg: CircuitConfig) -> Self {
        let failure_threshold = if cfg.failure_threshold == 0 {
            5
        } else {
            cfg.failure_threshold
        };
        let open_duration = if cfg.open_duration.is_zero() {
            Duration::from_secs(30)
        } else {
            cfg.open_duration
        };
        let window_size = if cfg.window_size == 0 {
            10
        } else {
            cfg.window_size
        };
        let half_open_max_calls = cfg.half_open_max_calls.max(1);
        let now = cfg.now.unwrap_or_else(|| Arc::new(Instant::now));
        Self {
            failure_threshold,
            window: cfg.window,
            open_duration,
            failure_rate_threshold: cfg.failure_rate_threshold,
            window_size,
            half_open_max_calls,
            now,
            inner: Mutex::new(BreakerInner {
                state: CircuitState::Closed,
                failures: Vec::new(),
                opened_at: None,
                outcomes: VecDeque::new(),
                half_open_calls: 0,
                half_open_successes: 0,
            }),
        }
    }

    /// Returns the current state — for inspection / metrics. This is a pure
    /// read: unlike pyfly's `state` property it never performs the
    /// `Open → HalfOpen` transition (that happens on the next gated call).
    pub fn state(&self) -> CircuitState {
        self.inner.lock().expect("breaker mutex poisoned").state
    }

    /// The effective consecutive-failure threshold (post-default).
    pub fn failure_threshold(&self) -> usize {
        self.failure_threshold
    }

    /// The effective open (recovery) duration (post-default) — pyfly's
    /// `recovery_timeout`.
    pub fn open_duration(&self) -> Duration {
        self.open_duration
    }

    /// The configured failure-rate threshold, if the breaker runs in
    /// count-based failure-rate mode.
    pub fn failure_rate_threshold(&self) -> Option<f64> {
        self.failure_rate_threshold
    }

    /// The effective count-based window size (post-default).
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// The effective half-open probe budget (post-default).
    pub fn half_open_max_calls(&self) -> usize {
        self.half_open_max_calls
    }

    /// Runs `op` under breaker supervision. Returns
    /// [`ResilienceError::CircuitOpen`] if the breaker has tripped; otherwise
    /// propagates `op`'s return value, recording its outcome.
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        self.before_call()?;
        let result = op().await;
        if result.is_err() {
            self.on_failure();
        } else {
            self.on_success();
        }
        result
    }

    /// Gate for manual instrumentation — the analogue of pyfly's
    /// `before_call`. Returns [`ResilienceError::CircuitOpen`] while the
    /// circuit is open or the half-open probe budget is exhausted; `Ok(())`
    /// admits the call (consuming a probe slot when half-open). Pair every
    /// `Ok` with exactly one [`on_success`](Self::on_success) /
    /// [`on_failure`](Self::on_failure). [`execute`](Self::execute) does this
    /// automatically.
    pub fn before_call(&self) -> Result<(), ResilienceError> {
        if self.allow() {
            Ok(())
        } else {
            Err(ResilienceError::CircuitOpen)
        }
    }

    /// Records a successful call — the analogue of pyfly's `on_success`.
    /// While half-open, [`half_open_max_calls`](Self::half_open_max_calls)
    /// successes close the circuit.
    pub fn on_success(&self) {
        self.record(false);
    }

    /// Records a failed call — the analogue of pyfly's `on_failure`. While
    /// half-open any failure re-opens the circuit immediately.
    pub fn on_failure(&self) {
        self.record(true);
    }

    /// Gate: decides whether a call may proceed, transitioning
    /// `Open → HalfOpen` once the open duration has elapsed. In `HalfOpen`
    /// up to `half_open_max_calls` trials are admitted; further calls are
    /// gated.
    fn allow(&self) -> bool {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        let now = (self.now)();
        if inner.state == CircuitState::Open {
            let opened_at = inner
                .opened_at
                .expect("open breaker always records opened_at");
            if now.duration_since(opened_at) >= self.open_duration {
                inner.state = CircuitState::HalfOpen;
                inner.half_open_calls = 0;
                inner.half_open_successes = 0;
            }
        }
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::Open => false,
            CircuitState::HalfOpen => {
                if inner.half_open_calls < self.half_open_max_calls {
                    inner.half_open_calls += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Whether the closed-state trip condition is met — consecutive count
    /// (default) or windowed failure rate (pyfly / Resilience4j
    /// `COUNT_BASED`). The rate mode requires a *full* window before judging.
    fn tripped(&self, inner: &BreakerInner) -> bool {
        if let Some(rate) = self.failure_rate_threshold {
            if inner.outcomes.len() < self.window_size {
                return false;
            }
            let failed = inner.outcomes.iter().filter(|ok| !**ok).count();
            (failed as f64 / inner.outcomes.len() as f64) >= rate
        } else {
            inner.failures.len() >= self.failure_threshold
        }
    }

    /// Records a call outcome, applying the state-transition rules.
    fn record(&self, failed: bool) {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        let now = (self.now)();
        // Count-based outcome ring (pyfly window) — bounded at window_size.
        if inner.outcomes.len() == self.window_size {
            inner.outcomes.pop_front();
        }
        inner.outcomes.push_back(!failed);
        if failed {
            if inner.state == CircuitState::HalfOpen {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.failures.clear();
                inner.half_open_calls = 0;
                inner.half_open_successes = 0;
                return;
            }
            if self.failure_rate_threshold.is_none() {
                inner.failures.push(now);
                if !self.window.is_zero() {
                    if let Some(cutoff) = now.checked_sub(self.window) {
                        inner.failures.retain(|t| *t >= cutoff);
                    }
                }
            }
            if self.tripped(&inner) {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.failures.clear();
            }
            return;
        }
        // Success.
        if inner.state == CircuitState::HalfOpen {
            inner.half_open_successes += 1;
            if inner.half_open_successes >= self.half_open_max_calls {
                inner.state = CircuitState::Closed;
                inner.failures.clear();
                inner.outcomes.clear();
                inner.half_open_calls = 0;
                inner.half_open_successes = 0;
            }
            return;
        }
        if self.window.is_zero() {
            // Consecutive mode — any success resets.
            inner.failures.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::Notify;

    /// Fake clock: a fixed base instant plus an atomically adjustable offset.
    fn fake_clock() -> (Clock, Arc<AtomicU64>) {
        let base = Instant::now();
        let offset_ms = Arc::new(AtomicU64::new(0));
        let handle = offset_ms.clone();
        let clock: Clock =
            Arc::new(move || base + Duration::from_millis(handle.load(Ordering::SeqCst)));
        (clock, offset_ms)
    }

    fn breaker(threshold: usize, window: Duration, open: Duration, clock: Clock) -> CircuitBreaker {
        CircuitBreaker::new(CircuitConfig {
            failure_threshold: threshold,
            window,
            open_duration: open,
            now: Some(clock),
            ..CircuitConfig::default()
        })
    }

    async fn fail(cb: &CircuitBreaker) -> Result<(), ResilienceError> {
        cb.execute(|| async { Err(ResilienceError::operation("boom")) })
            .await
    }

    async fn succeed(cb: &CircuitBreaker) -> Result<(), ResilienceError> {
        cb.execute(|| async { Ok(()) }).await
    }

    /// Port of Go `TestCircuitBreakerTripsAndRecovers`.
    #[tokio::test]
    async fn trips_and_recovers() {
        let (clock, offset) = fake_clock();
        let cb = breaker(3, Duration::ZERO, Duration::from_secs(1), clock);

        for _ in 0..3 {
            let _ = fail(&cb).await;
        }
        assert_eq!(cb.state(), CircuitState::Open);

        let err = succeed(&cb).await.unwrap_err();
        assert!(err.is_circuit_open(), "want CircuitOpen: {err}");

        // Advance past open duration → half-open trial allowed.
        offset.store(1001, Ordering::SeqCst);
        succeed(&cb).await.expect("trial call should run");
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn half_open_failure_reopens() {
        let (clock, offset) = fake_clock();
        let cb = breaker(1, Duration::ZERO, Duration::from_secs(1), clock);

        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Open);

        // Trial call fails → straight back to Open.
        offset.store(1001, Ordering::SeqCst);
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Open);

        // And the new open period gates calls again.
        let err = succeed(&cb).await.unwrap_err();
        assert!(err.is_circuit_open());
    }

    #[tokio::test]
    async fn window_prunes_old_failures() {
        let (clock, offset) = fake_clock();
        let cb = breaker(
            3,
            Duration::from_millis(100),
            Duration::from_secs(30),
            clock,
        );

        // Two failures at t=0.
        let _ = fail(&cb).await;
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Closed);

        // 200 ms later the old failures fall outside the window.
        offset.store(200, Ordering::SeqCst);
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Closed, "old failures pruned");

        // Two more inside the window trip it.
        let _ = fail(&cb).await;
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn success_resets_consecutive_failures() {
        let (clock, _) = fake_clock();
        let cb = breaker(3, Duration::ZERO, Duration::from_secs(30), clock);

        let _ = fail(&cb).await;
        let _ = fail(&cb).await;
        succeed(&cb).await.unwrap();
        let _ = fail(&cb).await;
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Closed, "success reset the count");
    }

    #[tokio::test]
    async fn half_open_allows_exactly_one_trial() {
        let (clock, offset) = fake_clock();
        let cb = Arc::new(breaker(1, Duration::ZERO, Duration::from_secs(1), clock));

        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Open);
        offset.store(1001, Ordering::SeqCst);

        let started = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let trial = {
            let cb = cb.clone();
            let started = started.clone();
            let gate = gate.clone();
            tokio::spawn(async move {
                cb.execute(|| async {
                    started.notify_one();
                    gate.notified().await;
                    Ok(())
                })
                .await
            })
        };

        started.notified().await;
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // While the trial is in flight, further calls are gated.
        let err = succeed(&cb).await.unwrap_err();
        assert!(err.is_circuit_open(), "second trial must be gated: {err}");

        gate.notify_one();
        trial.await.unwrap().unwrap();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn zero_config_falls_back_to_defaults() {
        let (clock, _) = fake_clock();
        // threshold 0 → 5, open 0 → 30 s (window 0 stays consecutive-only).
        let cb = breaker(0, Duration::ZERO, Duration::ZERO, clock);
        for _ in 0..4 {
            let _ = fail(&cb).await;
        }
        assert_eq!(cb.state(), CircuitState::Closed);
        let _ = fail(&cb).await;
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn execute_propagates_operation_error() {
        let (clock, _) = fake_clock();
        let cb = breaker(5, Duration::ZERO, Duration::from_secs(30), clock);
        let err = fail(&cb).await.unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn state_renders_go_strings() {
        assert_eq!(CircuitState::Closed.to_string(), "closed");
        assert_eq!(CircuitState::Open.to_string(), "open");
        assert_eq!(CircuitState::HalfOpen.to_string(), "half-open");
    }

    // ------------------------------------------------------------------
    // pyfly parity — failure-rate window + half-open probe budget
    // (port of tests/resilience/test_resilience_tuning.py)
    // ------------------------------------------------------------------

    /// Port of pyfly `test_failure_rate_window_opens_on_rate`.
    #[test]
    fn failure_rate_window_opens_on_rate() {
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_rate_threshold: Some(0.5),
            window_size: 4,
            ..CircuitConfig::default()
        });
        // Partial window (3 calls) never trips, even with failures.
        cb.on_success();
        cb.on_failure();
        cb.on_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        // 4th call completes the window [S, F, S, F] -> 50% failures -> open.
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    /// Below-threshold failure rate keeps the circuit closed even with a
    /// full window.
    #[test]
    fn failure_rate_below_threshold_stays_closed() {
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_rate_threshold: Some(0.5),
            window_size: 4,
            ..CircuitConfig::default()
        });
        // Full window [S, S, S, F] -> 25% failures < 50% -> closed.
        cb.on_success();
        cb.on_success();
        cb.on_success();
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
        // Ring slides: [S, S, F, F] -> 50% -> open.
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    /// Rate mode ignores the consecutive threshold — pyfly judges only the
    /// windowed rate once `failure_rate_threshold` is set.
    #[test]
    fn failure_rate_mode_ignores_consecutive_threshold() {
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            failure_rate_threshold: Some(0.9),
            window_size: 10,
            ..CircuitConfig::default()
        });
        for _ in 0..5 {
            cb.on_failure(); // would trip consecutive threshold 1
        }
        assert_eq!(cb.state(), CircuitState::Closed, "window not full yet");
    }

    /// Port of pyfly `test_half_open_requires_configured_successes`.
    #[test]
    fn half_open_requires_configured_successes() {
        let (clock, offset) = fake_clock();
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            open_duration: Duration::from_secs(1),
            now: Some(clock),
            half_open_max_calls: 2,
            ..CircuitConfig::default()
        });
        cb.on_failure(); // consecutive threshold 1 -> OPEN
        assert_eq!(cb.state(), CircuitState::Open);
        offset.store(1001, Ordering::SeqCst);
        cb.before_call().expect("probe 1 admitted"); // -> HALF_OPEN, probe 1
        cb.on_success(); // 1 success < 2 -> still probing
        assert_eq!(cb.state(), CircuitState::HalfOpen);
        cb.before_call().expect("probe 2 admitted");
        cb.on_success(); // 2 successes >= 2 -> CLOSED
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    /// Port of pyfly `test_half_open_failure_reopens_immediately`.
    #[test]
    fn half_open_failure_reopens_immediately() {
        let (clock, offset) = fake_clock();
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            open_duration: Duration::from_secs(1),
            now: Some(clock),
            half_open_max_calls: 3,
            ..CircuitConfig::default()
        });
        cb.on_failure(); // OPEN
        offset.store(1001, Ordering::SeqCst);
        cb.before_call().expect("probe 1 admitted"); // HALF_OPEN, probe 1
        cb.on_failure(); // any half-open failure -> OPEN
        assert_eq!(cb.state(), CircuitState::Open);
    }

    /// pyfly `before_call` raises once the half-open probe budget is spent.
    #[test]
    fn half_open_probe_budget_gates_excess_calls() {
        let (clock, offset) = fake_clock();
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 1,
            window: Duration::ZERO,
            open_duration: Duration::from_secs(1),
            now: Some(clock),
            half_open_max_calls: 2,
            ..CircuitConfig::default()
        });
        cb.on_failure();
        offset.store(1001, Ordering::SeqCst);
        cb.before_call().expect("probe 1");
        cb.before_call().expect("probe 2");
        let err = cb.before_call().unwrap_err();
        assert!(err.is_circuit_open(), "probe limit reached: {err}");
    }

    /// Closing after a half-open recovery clears the rate window, so stale
    /// failures cannot instantly re-trip the breaker (pyfly `_close`).
    #[tokio::test]
    async fn recovery_clears_rate_window() {
        let (clock, offset) = fake_clock();
        let cb = CircuitBreaker::new(CircuitConfig {
            open_duration: Duration::from_secs(1),
            now: Some(clock),
            failure_rate_threshold: Some(0.5),
            window_size: 2,
            ..CircuitConfig::default()
        });
        cb.on_failure();
        cb.on_failure(); // window [F, F] -> 100% -> OPEN
        assert_eq!(cb.state(), CircuitState::Open);
        offset.store(1001, Ordering::SeqCst);
        cb.execute(|| async { Ok(()) })
            .await
            .expect("half-open trial");
        assert_eq!(cb.state(), CircuitState::Closed);
        // The window was cleared on close — one failure is a partial window.
        cb.on_failure();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    /// Getters expose the effective (post-default) configuration.
    #[test]
    fn getters_expose_effective_config() {
        let cb = CircuitBreaker::new(CircuitConfig {
            failure_threshold: 3,
            open_duration: Duration::from_secs(10),
            failure_rate_threshold: Some(0.5),
            window_size: 8,
            half_open_max_calls: 2,
            ..CircuitConfig::default()
        });
        assert_eq!(cb.failure_threshold(), 3);
        assert_eq!(cb.open_duration(), Duration::from_secs(10));
        assert_eq!(cb.failure_rate_threshold(), Some(0.5));
        assert_eq!(cb.window_size(), 8);
        assert_eq!(cb.half_open_max_calls(), 2);

        // Zero-value fallbacks mirror pyfly constructor defaults.
        let cb = CircuitBreaker::new(CircuitConfig {
            window_size: 0,
            half_open_max_calls: 0,
            ..CircuitConfig::default()
        });
        assert_eq!(cb.window_size(), 10);
        assert_eq!(cb.half_open_max_calls(), 1);
    }
}
