//! Typed state-machine circuit breaker.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::ResilienceError;

/// `CircuitState` enumerates the canonical states. Transitions:
///
/// ```text
/// Closed   →(failures ≥ threshold)→ Open
/// Open     →(after open duration)→  HalfOpen
/// HalfOpen →(success)→              Closed
/// HalfOpen →(failure)→              Open
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
}

impl Default for CircuitConfig {
    /// Returns the 5-failure / 30 s open / 1 s window config — the Rust
    /// analogue of the Go port's `DefaultCircuitConfig()`.
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            window: Duration::from_secs(1),
            open_duration: Duration::from_secs(30),
            now: None,
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
            .finish()
    }
}

/// Mutable breaker state guarded by the mutex.
struct BreakerInner {
    state: CircuitState,
    failures: Vec<Instant>,
    opened_at: Option<Instant>,
}

/// `CircuitBreaker` is the typed state-machine breaker — it shields callers
/// from cascading failure of a slow or failing dependency by short-circuiting
/// calls while the downstream is sick.
pub struct CircuitBreaker {
    failure_threshold: usize,
    window: Duration,
    open_duration: Duration,
    now: Clock,
    inner: Mutex<BreakerInner>,
}

impl fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("failure_threshold", &self.failure_threshold)
            .field("window", &self.window)
            .field("open_duration", &self.open_duration)
            .field("state", &self.state())
            .finish()
    }
}

impl CircuitBreaker {
    /// Returns a closed breaker. A zero `failure_threshold` falls back to 5
    /// and a zero `open_duration` falls back to 30 seconds, mirroring the Go
    /// port's `NewCircuitBreaker` validation.
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
        let now = cfg.now.unwrap_or_else(|| Arc::new(Instant::now));
        Self {
            failure_threshold,
            window: cfg.window,
            open_duration,
            now,
            inner: Mutex::new(BreakerInner {
                state: CircuitState::Closed,
                failures: Vec::new(),
                opened_at: None,
            }),
        }
    }

    /// Returns the current state — for inspection / metrics.
    pub fn state(&self) -> CircuitState {
        self.inner.lock().expect("breaker mutex poisoned").state
    }

    /// Runs `op` under breaker supervision. Returns
    /// [`ResilienceError::CircuitOpen`] if the breaker has tripped; otherwise
    /// propagates `op`'s return value, recording its outcome.
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        if !self.allow() {
            return Err(ResilienceError::CircuitOpen);
        }
        let result = op().await;
        self.record(result.is_err());
        result
    }

    /// Gate: decides whether a call may proceed, transitioning
    /// `Open → HalfOpen` once the open duration has elapsed. In `HalfOpen`
    /// exactly one trial is allowed; subsequent calls are gated.
    fn allow(&self) -> bool {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        let now = (self.now)();
        match inner.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let opened_at = inner
                    .opened_at
                    .expect("open breaker always records opened_at");
                if now.duration_since(opened_at) >= self.open_duration {
                    inner.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => false,
        }
    }

    /// Records a call outcome, applying the state-transition rules.
    fn record(&self, failed: bool) {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        let now = (self.now)();
        if failed {
            if inner.state == CircuitState::HalfOpen {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.failures.clear();
                return;
            }
            inner.failures.push(now);
            if !self.window.is_zero() {
                if let Some(cutoff) = now.checked_sub(self.window) {
                    inner.failures.retain(|t| *t >= cutoff);
                }
            }
            if inner.failures.len() >= self.failure_threshold {
                inner.state = CircuitState::Open;
                inner.opened_at = Some(now);
                inner.failures.clear();
            }
            return;
        }
        // Success.
        if inner.state == CircuitState::HalfOpen {
            inner.state = CircuitState::Closed;
            inner.failures.clear();
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
}
