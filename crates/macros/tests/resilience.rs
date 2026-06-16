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

//! End-to-end tests for the resilience **decorator** macros (`#[retry]`,
//! `#[circuit_breaker]`, `#[rate_limit]`, `#[bulkhead]`, `#[timeout]`), exercised
//! through the one-dependency `::firefly` facade exactly as a user crate would.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use firefly::resilience::ResilienceError;

/// A domain error that satisfies the decorator contract:
/// `Error + Send + Sync + 'static + From<ResilienceError>`. It distinguishes a
/// business failure (`Boom`, which must round-trip the guard intact) from a
/// resilience short-circuit (`Guard`, raised by `From<ResilienceError>`).
#[derive(Debug)]
enum TestError {
    Boom(String),
    Guard(ResilienceError),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Boom(m) => write!(f, "boom: {m}"),
            TestError::Guard(e) => write!(f, "guard: {e}"),
        }
    }
}

impl std::error::Error for TestError {}

impl From<ResilienceError> for TestError {
    fn from(e: ResilienceError) -> Self {
        TestError::Guard(e)
    }
}

impl TestError {
    fn is_boom(&self) -> bool {
        matches!(self, TestError::Boom(_))
    }
    fn guard(&self) -> Option<&ResilienceError> {
        match self {
            TestError::Guard(e) => Some(e),
            _ => None,
        }
    }
}

/// One bean whose decorated methods share an `Arc<AtomicUsize>` call counter, so
/// each test can assert exactly how many times the *body* ran (vs. how many
/// calls a guard short-circuited before the body).
#[derive(Clone, Default)]
struct Service {
    calls: Arc<AtomicUsize>,
}

impl Service {
    fn count(&self) -> usize {
        self.calls.load(SeqCst)
    }

    // ---- #[retry] ----------------------------------------------------------

    /// Fails the first two attempts, succeeds on the third — the re-runnable
    /// `Fn` closure the macro emits must re-borrow `&self` each attempt.
    #[firefly::retry(max_attempts = 5, delay = "0ms")]
    async fn flaky(&self) -> Result<u32, TestError> {
        let n = self.calls.fetch_add(1, SeqCst) + 1;
        if n < 3 {
            Err(TestError::Boom(format!("attempt {n}")))
        } else {
            Ok(n as u32)
        }
    }

    /// Always fails: the budget is exhausted and the *original* `Boom` must
    /// resurface (not a wrapped resilience error).
    #[firefly::retry(max_attempts = 3, delay = "0ms", backoff = 2.0, max_delay = "10ms")]
    async fn doomed(&self) -> Result<u32, TestError> {
        self.calls.fetch_add(1, SeqCst);
        Err(TestError::Boom("nope".into()))
    }

    // ---- #[timeout] --------------------------------------------------------

    #[firefly::timeout("40ms")]
    async fn slow(&self) -> Result<u32, TestError> {
        tokio::time::sleep(Duration::from_millis(400)).await;
        Ok(1)
    }

    #[firefly::timeout("400ms")]
    async fn quick(&self) -> Result<u32, TestError> {
        Ok(7)
    }

    // ---- #[circuit_breaker] ------------------------------------------------

    /// Always fails; the breaker (shared `static`) trips after two consecutive
    /// failures and short-circuits the third call before the body runs.
    #[firefly::circuit_breaker(failure_threshold = 2, open_duration = "10s")]
    async fn unstable(&self) -> Result<u32, TestError> {
        self.calls.fetch_add(1, SeqCst);
        Err(TestError::Boom("down".into()))
    }

    // ---- #[rate_limit] / #[bulkhead] (smoke) -------------------------------

    #[firefly::rate_limit(rate = 1000.0, burst = 4)]
    async fn limited(&self) -> Result<u32, TestError> {
        self.calls.fetch_add(1, SeqCst);
        Ok(1)
    }

    #[firefly::bulkhead(4)]
    async fn isolated(&self) -> Result<u32, TestError> {
        self.calls.fetch_add(1, SeqCst);
        Ok(1)
    }

    // ---- composition: #[retry] over #[circuit_breaker] ---------------------

    /// The decorators stack (retry outermost). The first attempt fails, retry
    /// re-runs through the breaker, the second succeeds — and the generous
    /// breaker threshold never trips.
    #[firefly::retry(max_attempts = 3, delay = "0ms")]
    #[firefly::circuit_breaker(failure_threshold = 10, open_duration = "10s")]
    async fn guarded(&self) -> Result<u32, TestError> {
        let n = self.calls.fetch_add(1, SeqCst) + 1;
        if n < 2 {
            Err(TestError::Boom("retry me".into()))
        } else {
            Ok(n as u32)
        }
    }
}

#[tokio::test]
async fn retry_re_runs_until_success() {
    let svc = Service::default();
    assert_eq!(svc.flaky().await.expect("succeeds by the 3rd try"), 3);
    assert_eq!(svc.count(), 3, "the body ran exactly three times");
}

#[tokio::test]
async fn retry_exhausted_preserves_the_original_error() {
    let svc = Service::default();
    let err = svc.doomed().await.expect_err("budget exhausted");
    assert!(
        err.is_boom(),
        "the domain error round-trips the guard: {err}"
    );
    assert_eq!(svc.count(), 3, "all three attempts ran");
}

#[tokio::test]
async fn timeout_short_circuits_a_slow_call() {
    let svc = Service::default();
    let err = svc.slow().await.expect_err("must exceed the 40ms budget");
    assert!(
        err.guard().is_some_and(ResilienceError::is_timeout),
        "a timeout surfaces through From<ResilienceError>: {err}"
    );
}

#[tokio::test]
async fn timeout_passes_a_fast_call_through() {
    let svc = Service::default();
    assert_eq!(svc.quick().await.expect("well within budget"), 7);
}

#[tokio::test]
async fn circuit_breaker_opens_after_the_threshold() {
    let svc = Service::default();
    // Two failures trip the breaker (the body runs both times).
    assert!(svc.unstable().await.is_err());
    assert!(svc.unstable().await.is_err());
    // The third call is short-circuited *before* the body — the count stays at 2.
    let err = svc.unstable().await.expect_err("breaker is open");
    assert!(
        err.guard().is_some_and(ResilienceError::is_circuit_open),
        "an open breaker surfaces CircuitOpen: {err}"
    );
    assert_eq!(svc.count(), 2, "the open breaker never ran the body again");
}

#[tokio::test]
async fn rate_limit_and_bulkhead_pass_a_call_through() {
    let svc = Service::default();
    assert_eq!(svc.limited().await.expect("within burst"), 1);
    assert_eq!(svc.isolated().await.expect("within permits"), 1);
    assert_eq!(svc.count(), 2);
}

#[tokio::test]
async fn stacked_decorators_compose() {
    let svc = Service::default();
    assert_eq!(
        svc.guarded()
            .await
            .expect("retry recovers the first failure"),
        2
    );
    assert_eq!(svc.count(), 2, "one failed attempt, then one success");
}
