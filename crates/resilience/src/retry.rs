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

//! Retry combinator — port of pyfly's `@retry` decorator (Spring Retry /
//! Resilience4j `@Retry` equivalent).
//!
//! Where pyfly's `retry(...)` is a decorator that re-invokes the wrapped
//! callable, the Rust analogue is the [`Retry`] type: it re-runs a
//! **re-runnable** async closure (`Fn() -> Future`) up to
//! [`max_attempts`](RetryConfig::max_attempts) times while the failure is
//! [retryable](Retry::retry_on), sleeping `delay * backoff^attempt` (capped at
//! [`max_delay`](RetryConfig::max_delay), with optional ±jitter) between
//! attempts, then surfacing the last error.
//!
//! ```
//! use std::sync::atomic::{AtomicU32, Ordering};
//! use std::time::Duration;
//! use firefly_resilience::{Retry, ResilienceError};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let policy = Retry::new()
//!     .max_attempts(4)
//!     .delay(Duration::from_millis(1))
//!     .backoff(2.0);
//!
//! // The operation is a re-runnable `Fn`, so attempt state lives behind
//! // interior mutability (each attempt produces a fresh future).
//! let tries = AtomicU32::new(0);
//! let out: Result<&str, _> = policy
//!     .execute(|| {
//!         let n = tries.fetch_add(1, Ordering::SeqCst) + 1;
//!         async move {
//!             if n < 3 {
//!                 Err(ResilienceError::operation("flaky"))
//!             } else {
//!                 Ok("ok")
//!             }
//!         }
//!     })
//!     .await;
//! assert_eq!(out.unwrap(), "ok");
//! assert_eq!(tries.load(Ordering::SeqCst), 3);
//! # }
//! ```
//!
//! # Composing with `Chain`
//!
//! Because retry must re-run the guarded call, it operates on a re-runnable
//! `Fn` closure rather than the single-shot [`Operation`](crate::Operation) a
//! [`Chain`](crate::Chain) decorator receives. To retry a whole guarded chain,
//! wrap the chain's execution in [`Retry::execute`] — the inner closure is
//! re-runnable and each attempt drives a fresh pass through the chain:
//!
//! ```
//! use std::time::Duration;
//! use firefly_resilience::{Chain, Retry, Timeout, ResilienceError};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let chain = Chain::new().with(Timeout::new(Duration::from_secs(1)));
//! let retry = Retry::new().max_attempts(3);
//!
//! // Each retry attempt runs the full chain again.
//! let out = retry
//!     .execute(|| chain.execute(|| async { Ok::<(), ResilienceError>(()) }))
//!     .await;
//! assert!(out.is_ok());
//! # }
//! ```

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::error::ResilienceError;

/// Default total attempt budget — matches pyfly's `max_attempts=3`.
pub const DEFAULT_MAX_ATTEMPTS: usize = 3;

/// `RetryConfig` is the plain-data tuning for a [`Retry`] — the field-for-field
/// counterpart of pyfly's `retry(max_attempts, delay, backoff, max_delay,
/// jitter, exceptions)` keyword arguments.
///
/// The per-attempt wait (for the retry that follows the 0-based `attempt`-th
/// failure) is `delay * backoff^attempt`, optionally jittered by
/// `±jitter * wait` and capped at [`max_delay`](Self::max_delay), then floored
/// at zero — byte-for-byte the formula pyfly's `_wait` uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryConfig {
    /// Total attempts including the first (must be `>= 1`; pyfly raises
    /// `ValueError` otherwise — the Rust port clamps to `1`). Default `3`.
    pub max_attempts: usize,

    /// Base delay before the first retry. Default [`Duration::ZERO`]
    /// (pyfly's `delay=0.0`), i.e. retry immediately.
    pub delay: Duration,

    /// Multiplier applied to the delay each subsequent attempt
    /// (`delay * backoff^attempt`). Default `1.0` (constant delay), matching
    /// pyfly's `backoff=1.0`.
    pub backoff: f64,

    /// Optional ceiling on the per-attempt delay. `None` (the default) leaves
    /// the computed delay uncapped, mirroring pyfly's `max_delay=None`.
    pub max_delay: Option<Duration>,

    /// Randomization fraction in `[0, 1]` applied to each wait
    /// (`±jitter * wait`) to avoid thundering-herd retries. Default `0.0`
    /// (no jitter), matching pyfly's `jitter=0.0`.
    pub jitter: f64,
}

impl Default for RetryConfig {
    /// pyfly's defaults: `max_attempts=3`, `delay=0`, `backoff=1.0`,
    /// `max_delay=None`, `jitter=0.0`.
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            delay: Duration::ZERO,
            backoff: 1.0,
            max_delay: None,
            jitter: 0.0,
        }
    }
}

impl RetryConfig {
    /// Computes the wait before the retry that follows the 0-based
    /// `attempt`-th failure, with `sample` supplying the jitter draw
    /// (`uniform(-jitter, jitter)`). Mirrors pyfly's `_wait`:
    /// `delay * backoff^attempt`, jittered, capped at `max_delay`, floored at
    /// zero.
    fn wait_with(&self, attempt: usize, sample: f64) -> Duration {
        let base = self.delay.as_secs_f64();
        let mut computed = base * self.backoff.powi(attempt as i32);
        if self.jitter != 0.0 {
            computed += sample * computed;
        }
        let capped = match self.max_delay {
            Some(cap) => computed.min(cap.as_secs_f64()),
            None => computed,
        };
        let secs = capped.max(0.0);
        if secs.is_finite() {
            Duration::try_from_secs_f64(secs).unwrap_or(Duration::ZERO)
        } else {
            Duration::ZERO
        }
    }
}

/// Injectable jitter source — returns a draw in `[-jitter, jitter]` for the
/// given `jitter` fraction. Defaults to a uniform random draw; tests substitute
/// a deterministic sampler, playing the same role as the
/// [`Clock`](crate::Clock) hook on [`CircuitBreaker`](crate::CircuitBreaker).
pub type JitterFn = Arc<dyn Fn(f64) -> f64 + Send + Sync>;

/// The error-matching predicate — pyfly's `exceptions=(ExceptionType, ...)`
/// tuple, deciding which failures trigger a retry. Errors the predicate
/// rejects propagate immediately.
type Predicate = Arc<dyn Fn(&ResilienceError) -> bool + Send + Sync>;

/// `Retry` re-runs a re-runnable async operation while it fails retryably,
/// applying exponential backoff with optional jitter — the ergonomic,
/// composable port of pyfly's `@retry`.
///
/// Construct it fluently with [`Retry::new`] and the chainable setters, or from
/// a [`RetryConfig`] with [`Retry::from_config`]. Restrict which failures
/// trigger a retry with [`retry_on`](Self::retry_on) (pyfly's `exceptions=`);
/// by default **every** error is retried (pyfly's `exceptions=(Exception,)`).
#[derive(Clone)]
pub struct Retry {
    config: RetryConfig,
    retry_on: Predicate,
    jitter_fn: JitterFn,
}

impl fmt::Debug for Retry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Retry")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Default for Retry {
    fn default() -> Self {
        Self::from_config(RetryConfig::default())
    }
}

impl Retry {
    /// Returns a retry policy with pyfly's defaults (3 attempts, no delay, no
    /// jitter, retry on every error).
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a retry policy from an explicit [`RetryConfig`]. `max_attempts`
    /// is clamped to at least `1` (pyfly raises `ValueError` on `< 1`; the
    /// infallible Rust constructor clamps instead).
    pub fn from_config(mut config: RetryConfig) -> Self {
        if config.max_attempts < 1 {
            config.max_attempts = 1;
        }
        Self {
            config,
            retry_on: Arc::new(|_| true),
            jitter_fn: default_jitter(),
        }
    }

    /// Returns the effective [`RetryConfig`].
    pub fn config(&self) -> RetryConfig {
        self.config
    }

    /// Sets the total attempt budget (pyfly's `max_attempts`). Values `< 1`
    /// are clamped to `1`.
    #[must_use]
    pub fn max_attempts(mut self, attempts: usize) -> Self {
        self.config.max_attempts = attempts.max(1);
        self
    }

    /// Sets the base delay before the first retry (pyfly's `delay`).
    #[must_use]
    pub fn delay(mut self, delay: Duration) -> Self {
        self.config.delay = delay;
        self
    }

    /// Sets the per-attempt backoff multiplier (pyfly's `backoff`).
    #[must_use]
    pub fn backoff(mut self, backoff: f64) -> Self {
        self.config.backoff = backoff;
        self
    }

    /// Sets the per-attempt delay ceiling (pyfly's `max_delay`).
    #[must_use]
    pub fn max_delay(mut self, max_delay: Duration) -> Self {
        self.config.max_delay = Some(max_delay);
        self
    }

    /// Sets the jitter fraction in `[0, 1]` (pyfly's `jitter`). Values outside
    /// the range are clamped.
    #[must_use]
    pub fn jitter(mut self, jitter: f64) -> Self {
        self.config.jitter = jitter.clamp(0.0, 1.0);
        self
    }

    /// Restricts retries to errors matched by `predicate` — pyfly's
    /// `exceptions=(ExceptionType, ...)` filter. Unmatched failures propagate
    /// immediately without consuming further attempts. The sentinel helpers
    /// compose directly: `.retry_on(|e| !e.is_circuit_open())`.
    #[must_use]
    pub fn retry_on<P>(mut self, predicate: P) -> Self
    where
        P: Fn(&ResilienceError) -> bool + Send + Sync + 'static,
    {
        self.retry_on = Arc::new(predicate);
        self
    }

    /// Overrides the jitter source. The supplied function receives the
    /// configured jitter fraction and must return a draw in `[-jitter,
    /// jitter]`; the default draws uniformly. Primarily for deterministic
    /// tests (the [`Clock`](crate::Clock) analogue for retry timing).
    #[must_use]
    pub fn with_jitter_fn<J>(mut self, sample: J) -> Self
    where
        J: Fn(f64) -> f64 + Send + Sync + 'static,
    {
        self.jitter_fn = Arc::new(sample);
        self
    }

    /// Returns the wait before the retry that follows the 0-based
    /// `attempt`-th failure (no jitter applied — the deterministic component
    /// only). Useful for assertions and dashboards.
    pub fn delay_for(&self, attempt: usize) -> Duration {
        self.config.wait_with(attempt, 0.0)
    }

    /// Runs `op`, re-invoking it up to [`max_attempts`](RetryConfig::max_attempts)
    /// times while it fails an error the [`retry_on`](Self::retry_on)
    /// predicate accepts, sleeping the backoff delay between attempts. Returns
    /// the first `Ok`, or the last error once attempts are exhausted (or the
    /// first non-retryable error, immediately) — pyfly's `@retry` semantics.
    ///
    /// `op` is a re-runnable `Fn` (not `FnOnce`) because each attempt must
    /// produce a fresh future.
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, ResilienceError>>,
    {
        let attempts = self.config.max_attempts.max(1);
        let mut last: Option<ResilienceError> = None;
        for attempt in 0..attempts {
            match op().await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    // A failure the predicate rejects propagates immediately.
                    if !(self.retry_on)(&err) {
                        return Err(err);
                    }
                    last = Some(err);
                    // No sleep after the final attempt — break and resurface.
                    if attempt + 1 >= attempts {
                        break;
                    }
                    let sample = if self.config.jitter != 0.0 {
                        (self.jitter_fn)(self.config.jitter)
                    } else {
                        0.0
                    };
                    let wait = self.config.wait_with(attempt, sample);
                    if !wait.is_zero() {
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }
        // The loop always sets `last` before breaking (attempts >= 1).
        Err(last.expect("retry loop records the last failure before exhausting attempts"))
    }
}

/// Builds a [`Retry`] from pyfly-style arguments — the closest analogue of
/// calling `retry(max_attempts, delay=..., backoff=..., ...)` in pyfly. Prefer
/// the fluent [`Retry::new`] setters for readability; this free function exists
/// so a migrating pyfly user can keep the `retry(...)` call shape.
///
/// ```
/// use std::time::Duration;
/// use firefly_resilience::retry;
///
/// let policy = retry(5)
///     .delay(Duration::from_millis(50))
///     .backoff(2.0)
///     .jitter(0.1);
/// assert_eq!(policy.config().max_attempts, 5);
/// ```
pub fn retry(max_attempts: usize) -> Retry {
    Retry::new().max_attempts(max_attempts)
}

/// The default uniform jitter source: draws a value in `[-jitter, jitter]`.
fn default_jitter() -> JitterFn {
    Arc::new(|jitter: f64| {
        use rand::Rng;
        if jitter == 0.0 {
            return 0.0;
        }
        rand::thread_rng().gen_range(-jitter..=jitter)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Port of pyfly `test_succeeds_first_try` — no retry when the op
    /// succeeds immediately.
    #[tokio::test]
    async fn succeeds_first_try() {
        let calls = AtomicUsize::new(0);
        let out: Result<&str, _> = Retry::new()
            .execute(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok("ok") }
            })
            .await;
        assert_eq!(out.unwrap(), "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// Port of pyfly `test_retries_until_success`.
    #[tokio::test]
    async fn retries_until_success() {
        let calls = AtomicUsize::new(0);
        let out: Result<u32, _> = Retry::new()
            .max_attempts(5)
            .execute(|| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(ResilienceError::operation("flaky"))
                    } else {
                        Ok(n as u32)
                    }
                }
            })
            .await;
        assert_eq!(out.unwrap(), 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// Port of pyfly `test_exhausts_attempts_and_reraises` — the last error is
    /// surfaced after exactly `max_attempts` invocations.
    #[tokio::test]
    async fn exhausts_attempts_and_resurfaces_last_error() {
        let calls = AtomicUsize::new(0);
        let out: Result<(), _> = Retry::new()
            .max_attempts(3)
            .execute(|| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move { Err(ResilienceError::operation(format!("boom {n}"))) }
            })
            .await;
        let err = out.unwrap_err();
        assert_eq!(err.to_string(), "boom 3");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// `max_attempts(1)` means a single try with no retry.
    #[tokio::test]
    async fn single_attempt_does_not_retry() {
        let calls = AtomicUsize::new(0);
        let out: Result<(), _> = Retry::new()
            .max_attempts(1)
            .execute(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(ResilienceError::operation("once")) }
            })
            .await;
        assert!(out.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// `max_attempts(0)` is clamped to `1` (pyfly raises; the Rust port
    /// clamps).
    #[test]
    fn zero_attempts_clamped_to_one() {
        assert_eq!(Retry::new().max_attempts(0).config().max_attempts, 1);
        assert_eq!(
            Retry::from_config(RetryConfig {
                max_attempts: 0,
                ..RetryConfig::default()
            })
            .config()
            .max_attempts,
            1
        );
    }

    /// Port of pyfly `test_only_specified_exceptions` — a non-retryable error
    /// propagates immediately without consuming further attempts.
    #[tokio::test]
    async fn non_retryable_error_propagates_immediately() {
        let calls = AtomicUsize::new(0);
        let out: Result<(), _> = Retry::new()
            .max_attempts(5)
            // Retry operation errors, but never the circuit-open sentinel.
            .retry_on(|e| !e.is_circuit_open())
            .execute(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(ResilienceError::CircuitOpen) }
            })
            .await;
        assert!(out.unwrap_err().is_circuit_open());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must not retry");
    }

    /// The backoff schedule matches pyfly's `delay * backoff^attempt`.
    #[test]
    fn backoff_schedule_matches_pyfly() {
        let policy = Retry::new().delay(Duration::from_millis(100)).backoff(2.0);
        assert_eq!(policy.delay_for(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(2), Duration::from_millis(400));
    }

    /// `max_delay` caps the per-attempt wait.
    #[test]
    fn max_delay_caps_the_wait() {
        let policy = Retry::new()
            .delay(Duration::from_millis(100))
            .backoff(10.0)
            .max_delay(Duration::from_millis(250));
        assert_eq!(policy.delay_for(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for(1), Duration::from_millis(250)); // 1000 -> capped
        assert_eq!(policy.delay_for(2), Duration::from_millis(250)); // 10000 -> capped
    }

    /// Jitter widens the wait by `±jitter * wait`; with a deterministic
    /// sampler the result is exact. Mirrors pyfly's `_wait` jitter branch.
    #[test]
    fn jitter_applies_via_injected_sampler() {
        // Sampler returns the +jitter extreme: wait becomes wait * (1 + jitter).
        let policy = Retry::new()
            .delay(Duration::from_millis(100))
            .jitter(0.5)
            .with_jitter_fn(|jitter| jitter); // returns +0.5
        let waited = policy.config().wait_with(0, 0.5);
        assert_eq!(waited, Duration::from_millis(150));

        // The -jitter extreme shrinks the wait.
        let shrunk = policy.config().wait_with(0, -0.5);
        assert_eq!(shrunk, Duration::from_millis(50));
    }

    /// Negative jittered waits floor at zero (pyfly's `max(0.0, capped)`).
    #[test]
    fn jittered_wait_floors_at_zero() {
        let policy = Retry::new().delay(Duration::from_millis(100)).jitter(1.0);
        // A draw of -2.0 (beyond range, but exercises the floor) yields a
        // negative computed wait that must clamp to zero.
        let waited = policy.config().wait_with(0, -2.0);
        assert_eq!(waited, Duration::ZERO);
    }

    /// `delay(ZERO)` retries immediately (no sleep) — the default fast path.
    #[tokio::test]
    async fn zero_delay_retries_without_sleeping() {
        let calls = AtomicUsize::new(0);
        let out: Result<(), _> = Retry::new()
            .max_attempts(3)
            .execute(|| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if n < 3 {
                        Err(ResilienceError::operation("retry me"))
                    } else {
                        Ok(())
                    }
                }
            })
            .await;
        assert!(out.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// The `retry(n)` free function mirrors pyfly's `retry(...)` call shape.
    #[test]
    fn retry_free_function_sets_attempts() {
        let policy = retry(7);
        assert_eq!(policy.config().max_attempts, 7);
    }

    /// `Retry::execute` composes around a whole `Chain` via the documented
    /// pattern (each attempt re-runs the chain). `Chain::execute` is
    /// unit-valued, so the guarded call recovers with `Ok(())`.
    #[tokio::test]
    async fn composes_around_a_chain() {
        use crate::chain::Chain;
        let calls = AtomicUsize::new(0);
        let chain = Chain::new();
        let out: Result<(), _> = retry(4)
            .execute(|| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                chain.execute(move || async move {
                    if n < 2 {
                        Err(ResilienceError::operation("warmup"))
                    } else {
                        Ok(())
                    }
                })
            })
            .await;
        assert!(out.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// A real (uniform) jitter draw stays within `±jitter * wait` — exercises
    /// the production sampler without flaking on the bound.
    #[tokio::test]
    async fn default_jitter_stays_within_bounds() {
        let policy = Retry::new().delay(Duration::from_millis(40)).jitter(0.25);
        // delay_for ignores jitter; the jittered path is bounded by
        // [wait*(1-j), wait*(1+j)] = [30ms, 50ms].
        for _ in 0..100 {
            let sample = (policy.jitter_fn)(0.25);
            assert!(
                (-0.25..=0.25).contains(&sample),
                "sample out of range: {sample}"
            );
        }
    }

    #[test]
    fn retry_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Retry>();
        assert_send_sync::<RetryConfig>();
    }
}
