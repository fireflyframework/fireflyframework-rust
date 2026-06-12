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

//! Per-step retry / backoff / jitter / timeout enforcement — the Rust
//! spelling of pyfly's `StepInvoker`
//! (`pyfly.transactional.core.step_invoker`).
//!
//! pyfly applies a [`RetryPolicy`](crate::RetryPolicy) (max attempts,
//! exponential backoff, optional jitter, per-attempt timeout) to every
//! saga / workflow / TCC step. The Rust engines previously ran each step
//! exactly once; [`invoke_with_policy`] makes that policy enforceable for
//! any async action, and the saga / workflow / TCC engines opt in through
//! their `*_retry` builder variants.
//!
//! ```
//! use firefly_orchestration::{invoke_with_policy, RetryPolicy, StepContext};
//! use std::sync::atomic::{AtomicU32, Ordering};
//! use std::sync::Arc;
//!
//! let attempts = Arc::new(AtomicU32::new(0));
//! let a = attempts.clone();
//! let policy = RetryPolicy { max_attempts: 3, backoff_ms: 1, ..Default::default() };
//! let ctx = StepContext::new();
//! let result = tokio::runtime::Runtime::new().unwrap().block_on(
//!     invoke_with_policy("flaky", &policy, &ctx, move |_ctx| {
//!         let a = a.clone();
//!         async move {
//!             if a.fetch_add(1, Ordering::SeqCst) < 2 {
//!                 Err("transient".into())
//!             } else {
//!                 Ok(())
//!             }
//!         }
//!     }),
//! );
//! assert!(result.is_ok());
//! assert_eq!(attempts.load(Ordering::SeqCst), 3);
//! ```

use std::future::Future;
use std::time::Duration;

use crate::model::RetryPolicy;
use crate::step_context::StepContext;
use crate::BoxError;

/// Error returned by [`invoke_with_policy`] once a step has exhausted its
/// retries — the Rust analogue of pyfly's `StepFailedError`.
#[derive(Debug, thiserror::Error)]
pub enum StepInvokeError {
    /// The step failed on every attempt; `attempts` is the number of times
    /// the action ran and `source` is the final error it produced.
    #[error("step {step:?} failed after {attempts} attempt(s): {source}")]
    Failed {
        /// The step / node / participant id.
        step: String,
        /// How many times the action was invoked.
        attempts: u32,
        /// The error from the last attempt.
        #[source]
        source: BoxError,
    },
    /// The step exceeded its per-attempt timeout on the final attempt —
    /// pyfly's `StepTimeoutError` wrapped inside `StepFailedError`.
    #[error("step {step:?} timed out after {timeout_ms}ms on {attempts} attempt(s)")]
    TimedOut {
        /// The step / node / participant id.
        step: String,
        /// The per-attempt timeout that was exceeded, in milliseconds.
        timeout_ms: u64,
        /// How many times the action was invoked.
        attempts: u32,
    },
}

impl StepInvokeError {
    /// How many times the action was invoked before giving up.
    pub fn attempts(&self) -> u32 {
        match self {
            Self::Failed { attempts, .. } | Self::TimedOut { attempts, .. } => *attempts,
        }
    }

    /// `true` when the failure was a per-attempt timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::TimedOut { .. })
    }

    /// Consumes the error and returns the original underlying cause for a
    /// hard failure, or `None` for a timeout (which has no inner cause). Used
    /// by the engines to preserve their historical `step "name": <cause>`
    /// error-message shape when a single attempt failed.
    pub fn into_source(self) -> Option<BoxError> {
        match self {
            Self::Failed { source, .. } => Some(source),
            Self::TimedOut { .. } => None,
        }
    }
}

/// Per-attempt outcome — either a hard error or a timeout.
enum AttemptError {
    Failed(BoxError),
    TimedOut,
}

/// Invokes `action` under `policy`, returning its `Ok(())` on the first
/// success or [`StepInvokeError`] once attempts are exhausted.
///
/// Mirrors pyfly's `StepInvoker.invoke`:
/// * up to `policy.max_attempts` attempts (at least one);
/// * exponential backoff between attempts — `backoff_ms * 2^(attempt-1)`;
/// * optional jitter of up to `jitter_factor * base` added to the backoff;
/// * a per-attempt timeout (`timeout_ms`, `0` disables) that races each
///   attempt and treats a slow attempt as a (retryable) failure.
///
/// The `action` receives the run's [`StepContext`] so it can read prior
/// step results and write its own (inter-step data passing).
pub async fn invoke_with_policy<F, Fut>(
    step: &str,
    policy: &RetryPolicy,
    ctx: &StepContext,
    action: F,
) -> Result<(), StepInvokeError>
where
    F: Fn(&StepContext) -> Fut,
    Fut: Future<Output = Result<(), BoxError>>,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut last: Option<AttemptError> = None;

    for attempt in 1..=max_attempts {
        match run_attempt(policy.timeout_ms, ctx, &action).await {
            Ok(()) => return Ok(()),
            Err(err) => last = Some(err),
        }
        if attempt < max_attempts {
            let delay = compute_backoff(policy, attempt);
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
        }
    }

    Err(match last.expect("at least one attempt ran") {
        AttemptError::TimedOut => StepInvokeError::TimedOut {
            step: step.to_string(),
            timeout_ms: policy.timeout_ms,
            attempts: max_attempts,
        },
        AttemptError::Failed(source) => StepInvokeError::Failed {
            step: step.to_string(),
            attempts: max_attempts,
            source,
        },
    })
}

/// Runs a single attempt, applying the per-attempt timeout when configured.
async fn run_attempt<F, Fut>(
    timeout_ms: u64,
    ctx: &StepContext,
    action: &F,
) -> Result<(), AttemptError>
where
    F: Fn(&StepContext) -> Fut,
    Fut: Future<Output = Result<(), BoxError>>,
{
    if timeout_ms == 0 {
        return action(ctx).await.map_err(AttemptError::Failed);
    }
    match tokio::time::timeout(Duration::from_millis(timeout_ms), action(ctx)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(AttemptError::Failed(source)),
        Err(_elapsed) => Err(AttemptError::TimedOut),
    }
}

/// Computes the backoff before the next attempt — pyfly's
/// `_compute_backoff`: `backoff_ms * 2^(attempt-1)`, plus up to
/// `jitter_factor * base` of jitter when enabled.
fn compute_backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    if policy.backoff_ms == 0 {
        return Duration::ZERO;
    }
    let exponent = attempt.saturating_sub(1).min(20);
    let base = policy.backoff_ms.saturating_mul(1u64 << exponent);
    let mut millis = base as f64;
    if policy.jitter && policy.jitter_factor > 0.0 {
        // Deterministic, dependency-free jitter in [0, jitter_factor*base):
        // derive a pseudo-random fraction from the wall clock nanos. Keeps
        // tests fast (tiny backoff bases) without pulling in `rand`.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let frac = (nanos % 1000) as f64 / 1000.0;
        millis += base as f64 * policy.jitter_factor * frac;
    }
    Duration::from_millis(millis as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    // Port of pyfly test_step_invoker.py::test_retry_then_succeed.
    #[tokio::test]
    async fn retry_then_succeed() {
        let attempts = Arc::new(AtomicU32::new(0));
        let a = attempts.clone();
        let policy = RetryPolicy {
            max_attempts: 5,
            backoff_ms: 1,
            ..Default::default()
        };
        let ctx = StepContext::new();
        let out = invoke_with_policy("s", &policy, &ctx, move |_| {
            let a = a.clone();
            async move {
                if a.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err("transient".into())
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(out.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    // Port of pyfly test_step_invoker.py::test_retry_exhausted_raises.
    #[tokio::test]
    async fn retry_exhausted_raises() {
        let policy = RetryPolicy {
            max_attempts: 2,
            backoff_ms: 1,
            ..Default::default()
        };
        let ctx = StepContext::new();
        let err = invoke_with_policy("bang", &policy, &ctx, |_| async { Err("boom".into()) })
            .await
            .expect_err("should exhaust");
        assert_eq!(err.attempts(), 2);
        assert!(matches!(err, StepInvokeError::Failed { .. }));
        assert!(err.to_string().contains("\"bang\""));
        assert!(err.to_string().contains("2 attempt"));
    }

    // Port of pyfly test_step_invoker.py::test_timeout: a slow step exceeds
    // its per-attempt timeout and surfaces as a timeout failure.
    #[tokio::test]
    async fn slow_step_times_out() {
        let policy = RetryPolicy {
            max_attempts: 1,
            timeout_ms: 20,
            ..Default::default()
        };
        let ctx = StepContext::new();
        let err = invoke_with_policy("slow", &policy, &ctx, |_| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(())
        })
        .await
        .expect_err("should time out");
        assert!(err.is_timeout());
        assert!(matches!(err, StepInvokeError::TimedOut { .. }));
    }

    // Default policy (max_attempts=1) runs the action exactly once.
    #[tokio::test]
    async fn default_policy_runs_once() {
        let attempts = Arc::new(AtomicU32::new(0));
        let a = attempts.clone();
        let ctx = StepContext::new();
        invoke_with_policy("once", &RetryPolicy::default(), &ctx, move |_| {
            let a = a.clone();
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .await
        .expect("ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    // Exponential backoff grows: base*2^(attempt-1). With backoff_ms=10 and
    // three attempts the total wait is at least 10 + 20 = 30ms.
    #[tokio::test]
    async fn exponential_backoff_between_attempts() {
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff_ms: 10,
            ..Default::default()
        };
        let ctx = StepContext::new();
        let started = Instant::now();
        let _ = invoke_with_policy("b", &policy, &ctx, |_| async { Err("x".into()) }).await;
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    // The action can read prior-step results from the context it is handed.
    #[tokio::test]
    async fn action_reads_step_context() {
        let ctx = StepContext::new();
        ctx.set_result("prior", serde_json::json!({"v": 42}));
        let policy = RetryPolicy::default();
        invoke_with_policy("reader", &policy, &ctx, |ctx| {
            let prior = ctx.result_field("prior", "v");
            async move {
                assert_eq!(prior, Some(serde_json::json!(42)));
                Ok(())
            }
        })
        .await
        .expect("ok");
    }

    // compute_backoff respects the disable (backoff_ms==0) fast path.
    #[test]
    fn zero_backoff_is_immediate() {
        let policy = RetryPolicy::default();
        assert_eq!(compute_backoff(&policy, 1), Duration::ZERO);
    }
}
