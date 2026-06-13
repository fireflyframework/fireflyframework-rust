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

//! A standalone, reusable retry combinator — the Rust spelling of pyfly's
//! `client.RetryPolicy`.
//!
//! Where [`RestBuilder`](crate::RestBuilder) bakes retry into one HTTP
//! client, [`RetryPolicy`] is a free-standing value a caller constructs once
//! and wraps around **any** fallible async operation (an HTTP call, a SOAP
//! invocation, a downstream-SDK method, …), so the same exponential-backoff
//! semantics a pyfly user reached for via `client.RetryPolicy` are available
//! in Rust without re-implementing the loop per call site.
//!
//! ```
//! # use std::time::Duration;
//! # use std::sync::atomic::{AtomicU32, Ordering};
//! # use firefly_client::RetryPolicy;
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let policy = RetryPolicy::new()
//!     .with_max_attempts(3)
//!     .with_base_delay(Duration::from_millis(1));
//!
//! let calls = AtomicU32::new(0);
//! let result: Result<&str, &str> = policy
//!     .execute(|| async {
//!         // Fail twice, then succeed.
//!         if calls.fetch_add(1, Ordering::SeqCst) < 2 {
//!             Err("transient")
//!         } else {
//!             Ok("ok")
//!         }
//!     })
//!     .await;
//! assert_eq!(result, Ok("ok"));
//! assert_eq!(calls.load(Ordering::SeqCst), 3);
//! # }
//! ```

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Default attempt budget (3, including the first) — pyfly's
/// `RetryPolicy(max_attempts=3)`.
const DEFAULT_MAX_ATTEMPTS: usize = 3;
/// Default base delay (1 s), doubled per retry — pyfly's
/// `base_delay=timedelta(seconds=1)`.
const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);

/// A type-erased "is this error retryable?" predicate, shared behind an
/// [`Arc`] so a configured [`RetryPolicy`] stays cheap to [`Clone`].
type RetryPredicate = Arc<dyn Fn(&dyn std::any::Any) -> bool + Send + Sync>;

/// Retry with exponential backoff for transient failures — the Rust port of
/// pyfly's `client.RetryPolicy`.
///
/// An operation is attempted up to [`max_attempts`](RetryPolicy::with_max_attempts)
/// times; between attempts the delay starts at
/// [`base_delay`](RetryPolicy::with_base_delay) and **doubles** each time
/// (`base_delay * 2^attempt`), exactly as pyfly computes
/// `base_delay * (2 ** attempt)`. A failure is retried only when the
/// [`retry_on`](RetryPolicy::with_retry_on) predicate accepts the error; the
/// default predicate retries **every** error (pyfly's
/// `retry_on=(Exception,)`). When the budget is exhausted the last error is
/// returned.
///
/// [`RetryPolicy`] is cheap to [`Clone`] (the predicate is held behind an
/// [`Arc`]) so one configured policy can be shared across call sites.
#[derive(Clone)]
pub struct RetryPolicy {
    max_attempts: usize,
    base_delay: Duration,
    retry_on: Option<RetryPredicate>,
}

impl RetryPolicy {
    /// Returns a policy with pyfly's defaults: 3 attempts, a 1 s base delay
    /// (doubling), and "retry on every error".
    pub fn new() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_BASE_DELAY,
            retry_on: None,
        }
    }

    /// Overrides the total attempt budget (default 3) — pyfly's
    /// `max_attempts`. The value is the number of attempts, not retries: `1`
    /// means a single attempt with no retry. A value of `0` is treated as `1`
    /// (the operation always runs at least once).
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: usize) -> Self {
        self.max_attempts = max_attempts.max(1);
        self
    }

    /// Overrides the base delay between retries (default 1 s), which doubles
    /// each attempt — pyfly's `base_delay`.
    #[must_use]
    pub fn with_base_delay(mut self, base_delay: Duration) -> Self {
        self.base_delay = base_delay;
        self
    }

    /// Restricts retries to errors matching `predicate` — the Rust analog of
    /// pyfly's `retry_on=(SomeException, ...)`. An error the predicate
    /// rejects is returned immediately without consuming the rest of the
    /// budget. Without this call every error is retried (pyfly's default).
    #[must_use]
    pub fn with_retry_on<E, F>(mut self, predicate: F) -> Self
    where
        E: 'static,
        F: Fn(&E) -> bool + Send + Sync + 'static,
    {
        self.retry_on = Some(Arc::new(move |err: &dyn std::any::Any| {
            // Only errors of type `E` can be judged by this predicate; an
            // error of any other type is conservatively treated as retryable
            // (matching the permissive default), so a mistyped predicate
            // never silently swallows an unexpected error class.
            match err.downcast_ref::<E>() {
                Some(e) => predicate(e),
                None => true,
            }
        }));
        self
    }

    /// Returns the configured attempt budget.
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// Returns the configured base delay.
    pub fn base_delay(&self) -> Duration {
        self.base_delay
    }

    /// The backoff before the retry following attempt index `attempt`
    /// (0-based): `base_delay * 2^attempt`, saturating rather than
    /// overflowing — pyfly's `base_delay * (2 ** attempt)`.
    fn backoff(&self, attempt: usize) -> Duration {
        let factor = 1u32.checked_shl(u32::try_from(attempt).unwrap_or(u32::MAX));
        match factor {
            Some(f) => self.base_delay.saturating_mul(f),
            None => Duration::MAX,
        }
    }

    /// Runs `op`, retrying on a matching error with exponential backoff, and
    /// returns its `Result` — the Rust port of pyfly's `RetryPolicy.execute`.
    ///
    /// `op` is a closure producing a fresh future on every attempt (so the
    /// operation can be re-issued cleanly). On `Ok` the value is returned at
    /// once. On `Err`, when more attempts remain **and** the
    /// [`retry_on`](RetryPolicy::with_retry_on) predicate accepts the error,
    /// the policy sleeps for the (doubling) backoff and tries again;
    /// otherwise the error is returned.
    pub async fn execute<T, E, F, Fut>(&self, mut op: F) -> Result<T, E>
    where
        E: 'static,
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let mut attempt = 0usize;
        loop {
            match op().await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let is_last = attempt + 1 >= self.max_attempts;
                    let retryable = self
                        .retry_on
                        .as_ref()
                        .map(|p| p(&err as &dyn std::any::Any))
                        .unwrap_or(true);
                    if is_last || !retryable {
                        return Err(err);
                    }
                    tokio::time::sleep(self.backoff(attempt)).await;
                    attempt += 1;
                }
            }
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RetryPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryPolicy")
            .field("max_attempts", &self.max_attempts)
            .field("base_delay", &self.base_delay)
            .field("retry_on", &self.retry_on.as_ref().map(|_| "<predicate>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn defaults_match_pyfly() {
        let p = RetryPolicy::new();
        assert_eq!(p.max_attempts(), 3);
        assert_eq!(p.base_delay(), Duration::from_secs(1));
    }

    #[test]
    fn zero_attempts_is_clamped_to_one() {
        assert_eq!(RetryPolicy::new().with_max_attempts(0).max_attempts(), 1);
    }

    #[test]
    fn backoff_doubles_and_saturates() {
        let p = RetryPolicy::new().with_base_delay(Duration::from_millis(10));
        assert_eq!(p.backoff(0), Duration::from_millis(10));
        assert_eq!(p.backoff(1), Duration::from_millis(20));
        assert_eq!(p.backoff(2), Duration::from_millis(40));
        // A huge attempt index saturates rather than panicking.
        assert_eq!(p.backoff(usize::MAX), Duration::MAX);
    }

    #[tokio::test]
    async fn succeeds_on_first_attempt_without_retrying() {
        let calls = AtomicU32::new(0);
        let r: Result<u32, ()> = RetryPolicy::new()
            .with_base_delay(Duration::from_millis(1))
            .execute(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            })
            .await;
        assert_eq!(r, Ok(42));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_succeeds() {
        let calls = AtomicU32::new(0);
        let r: Result<&str, &str> = RetryPolicy::new()
            .with_max_attempts(3)
            .with_base_delay(Duration::from_millis(1))
            .execute(|| async {
                if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err("transient")
                } else {
                    Ok("ok")
                }
            })
            .await;
        assert_eq!(r, Ok("ok"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn returns_last_error_when_budget_exhausted() {
        let calls = AtomicU32::new(0);
        let r: Result<(), u32> = RetryPolicy::new()
            .with_max_attempts(2)
            .with_base_delay(Duration::from_millis(1))
            .execute(|| async { Err(calls.fetch_add(1, Ordering::SeqCst)) })
            .await;
        // Two attempts; the second (index 1) error is returned.
        assert_eq!(r, Err(1));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retry_on_predicate_short_circuits_non_retryable() {
        // Only "retry" errors are retried; a "fatal" error returns at once.
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy::new()
            .with_max_attempts(5)
            .with_base_delay(Duration::from_millis(1))
            .with_retry_on(|e: &&str| *e == "retry");

        let r: Result<(), &str> = policy
            .execute(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("fatal")
            })
            .await;
        assert_eq!(r, Err("fatal"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "fatal must not retry");
    }

    #[tokio::test]
    async fn retry_on_predicate_retries_matching_errors() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy::new()
            .with_max_attempts(3)
            .with_base_delay(Duration::from_millis(1))
            .with_retry_on(|e: &&str| *e == "retry");
        let r: Result<(), &str> = policy
            .execute(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("retry")
            })
            .await;
        assert_eq!(r, Err("retry"));
        assert_eq!(calls.load(Ordering::SeqCst), 3, "retryable consumes budget");
    }

    #[test]
    fn policy_is_cloneable_and_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RetryPolicy>();
        let p = RetryPolicy::new().with_retry_on(|_e: &String| true);
        let _clone = p.clone();
    }
}
