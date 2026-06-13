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

//! Saga engine: sequential steps with reverse-order compensation.
//!
//! # Per-step retry and inter-step data passing (pyfly parity)
//!
//! A [`Step`] may declare a [`RetryPolicy`](crate::RetryPolicy) via
//! [`Step::with_retry`] (enforced through
//! [`invoke_with_policy`](crate::invoke_with_policy): max attempts,
//! exponential backoff, jitter, per-attempt timeout) and a context-aware
//! execute body via [`Step::with_context`] that can read prior step results
//! from the run's [`StepContext`](crate::StepContext) — pyfly's
//! `StepInvoker` + `Annotated[..., FromStep/Input]` argument injection.

use crate::observability::{NoOpOrchestrationEvents, OrchestrationEvents};
use crate::step_context::StepContext;
use crate::step_invoker::invoke_with_policy;
use crate::{boxed_action, ActionFn, BoxError, CancellationToken, ExecutionPattern, RetryPolicy};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

/// Controls how a saga handles compensation failures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CompensationPolicy {
    /// Logs and continues compensating remaining steps even if one
    /// compensation fails — the default.
    #[default]
    BestEffort,
    /// Aborts the rollback at the first compensation failure and surfaces a
    /// [`SagaError::Compensation`] wrapping the offender.
    StopOnError,
}

/// A context-aware action: receives the run's [`StepContext`] so it can read
/// prior step results and publish its own.
pub(crate) type CtxActionFn = Box<
    dyn Fn(StepContext) -> futures::future::BoxFuture<'static, Result<(), BoxError>> + Send + Sync,
>;

/// The execute / compensate body — either a legacy zero-arg action or a
/// context-aware one (inter-step data passing).
enum Body {
    Plain(ActionFn),
    WithContext(CtxActionFn),
}

impl Body {
    fn call(&self, ctx: &StepContext) -> futures::future::BoxFuture<'static, Result<(), BoxError>> {
        match self {
            Body::Plain(action) => action(),
            Body::WithContext(action) => action(ctx.clone()),
        }
    }
}

/// A single saga unit. The execute action moves the saga forward; the
/// optional compensation rolls back the side-effects of execute. Steps must
/// be idempotent — the engine retries under the configured
/// [`RetryPolicy`](crate::RetryPolicy).
pub struct Step {
    name: String,
    execute: Body,
    compensate: Option<Body>,
    retry: RetryPolicy,
}

impl Step {
    /// Creates a step from a name and an async execute action.
    pub fn new<F, Fut>(name: impl Into<String>, execute: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            execute: Body::Plain(boxed_action(execute)),
            compensate: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Creates a step whose execute body receives the run's
    /// [`StepContext`] — the engine spelling of pyfly's
    /// `Annotated[..., FromStep/Input/Variable]` argument injection. The
    /// step can read prior step results and publish its own
    /// ([`StepContext::set_result`](crate::StepContext::set_result)) for
    /// later steps. Inter-step data passing.
    pub fn with_context<F, Fut>(name: impl Into<String>, execute: F) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            execute: Body::WithContext(Box::new(move |ctx| Box::pin(execute(ctx)))),
            compensate: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Attaches an async compensation action that rolls back the
    /// side-effects of execute.
    pub fn with_compensation<F, Fut>(mut self, compensate: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.compensate = Some(Body::Plain(boxed_action(compensate)));
        self
    }

    /// Attaches a context-aware compensation action — pyfly's
    /// `@compensation_step` consuming `@FromStep` / `@CompensationError`.
    pub fn with_context_compensation<F, Fut>(mut self, compensate: F) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.compensate = Some(Body::WithContext(Box::new(move |ctx| {
            Box::pin(compensate(ctx))
        })));
        self
    }

    /// Sets the [`RetryPolicy`](crate::RetryPolicy) applied to this step's
    /// execute action — pyfly's per-step retry / backoff / jitter / timeout
    /// enforcement via `StepInvoker`. The default policy runs the step once.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The step name, as reported in [`Outcome`] and error messages.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The retry policy configured for this step.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("name", &self.name)
            .field("has_compensation", &self.compensate.is_some())
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

/// Terminal status of a saga run. The lowercase wire strings (`completed` /
/// `compensated` / `failed`) match the Go port's `Outcome.Status` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SagaStatus {
    /// Every step executed successfully.
    Completed,
    /// A step failed and already-executed steps were rolled back.
    Compensated,
    /// The run was cancelled before reaching a terminal step.
    Failed,
}

impl fmt::Display for SagaStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Completed => "completed",
            Self::Compensated => "compensated",
            Self::Failed => "failed",
        })
    }
}

/// Captures the saga's terminal state. Always populated, whether the run
/// succeeded or failed (see [`SagaFailure::outcome`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    /// The saga name.
    pub saga: String,
    /// Terminal status: completed, compensated, or failed.
    pub status: SagaStatus,
    /// Names of the steps that executed successfully, in order.
    pub steps_executed: Vec<String>,
    /// Names of the steps whose compensation ran successfully, in
    /// reverse execution order.
    pub steps_rolled: Vec<String>,
    /// Rendered message of the error that ended the run, if any.
    pub error: Option<String>,
    /// UTC instant the run started.
    pub started_at: DateTime<Utc>,
    /// UTC instant the run finished.
    pub finished_at: DateTime<Utc>,
}

/// Errors produced by [`Saga::run`].
#[derive(Debug, Error)]
pub enum SagaError {
    /// A step's execute action failed; rollback was attempted.
    #[error("step {step:?}: {source}")]
    Step {
        /// Name of the failing step.
        step: String,
        /// The error returned by the step's execute action.
        #[source]
        source: BoxError,
    },
    /// Compensation itself erred under [`CompensationPolicy::StopOnError`].
    /// `original` holds the failure that triggered rollback.
    #[error("saga: compensation failed at step {step:?}: {compensate} (original: {original})")]
    Compensation {
        /// Name of the step whose execute failure triggered rollback.
        step: String,
        /// The original step failure that triggered rollback.
        original: Box<SagaError>,
        /// The error returned by the failing compensation action.
        compensate: BoxError,
    },
    /// The run observed a cancelled [`CancellationToken`] before executing
    /// the next step.
    #[error("saga cancelled")]
    Cancelled,
}

impl SagaError {
    /// Reports whether this error is a compensation failure — the analogue
    /// of the Go port's `IsCompensationError`.
    pub fn is_compensation_error(&self) -> bool {
        matches!(self, Self::Compensation { .. })
    }
}

/// Unwraps a [`StepInvokeError`](crate::StepInvokeError) into the
/// `BoxError` cause attached to a [`SagaError::Step`].
///
/// For the default single-attempt policy a hard failure unwraps to its
/// original cause so the saga's `step "name": <cause>` message is byte-for-byte
/// identical to the pre-retry engine. When the step genuinely retried (more
/// than one attempt configured) or timed out, the richer invoker error is
/// preserved so the retry context is visible.
fn unwrap_invoke_cause(retry: &RetryPolicy, err: crate::StepInvokeError) -> BoxError {
    // A single-attempt, no-timeout policy can only fail with `Failed`, whose
    // source is the step's original error — unwrap it so the message shape is
    // unchanged from the pre-retry engine.
    if retry.max_attempts.max(1) == 1 && retry.timeout_ms == 0 {
        if let Some(source) = err.into_source() {
            return source;
        }
        // Unreachable in practice (no timeout configured); fall through.
        return "step failed".into();
    }
    Box::new(err)
}

/// A failed saga run: the error that ended it plus the fully-populated
/// [`Outcome`] — the Rust shape of Go's `(Outcome, error)` return pair.
#[derive(Debug)]
pub struct SagaFailure {
    outcome: Box<Outcome>,
    error: SagaError,
}

impl SagaFailure {
    /// The terminal outcome of the failed run (status, steps executed,
    /// steps rolled back, timestamps).
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    /// The error that ended the run.
    pub fn error(&self) -> &SagaError {
        &self.error
    }

    /// Reports whether the run failed because compensation itself erred —
    /// the analogue of the Go port's `IsCompensationError`.
    pub fn is_compensation_error(&self) -> bool {
        self.error.is_compensation_error()
    }

    /// Decomposes the failure into its outcome and error.
    pub fn into_parts(self) -> (Outcome, SagaError) {
        (*self.outcome, self.error)
    }
}

impl fmt::Display for SagaFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for SagaFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Runs [`Step`]s sequentially. On any execute failure, already-executed
/// steps are compensated in reverse order under the configured
/// [`CompensationPolicy`].
pub struct Saga {
    name: String,
    steps: Vec<Step>,
    policy: CompensationPolicy,
}

impl Saga {
    /// Creates an empty saga with the default
    /// [`CompensationPolicy::BestEffort`] policy.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            policy: CompensationPolicy::default(),
        }
    }

    /// Sets the compensation policy.
    pub fn policy(mut self, policy: CompensationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Appends a step. Steps execute in insertion order.
    pub fn step(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// The saga name, as reported in [`Outcome`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured steps, in execution order — the definition-listing
    /// accessor used by the validator, registry, and admin surfaces.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Step names in execution order.
    pub fn step_names(&self) -> Vec<&str> {
        self.steps.iter().map(|s| s.name.as_str()).collect()
    }

    /// Executes the saga. On success the returned [`Outcome`] has status
    /// [`SagaStatus::Completed`]; on failure the [`SagaFailure`] carries
    /// both the error and the terminal outcome.
    pub async fn run(&self) -> Result<Outcome, SagaFailure> {
        self.run_cancellable(&CancellationToken::new()).await
    }

    /// Executes the saga, checking `token` before each step. A cancelled
    /// token fails the run with [`SagaError::Cancelled`] and status
    /// [`SagaStatus::Failed`]; no compensation is attempted, mirroring the
    /// Go port's `ctx.Err()` handling.
    pub async fn run_cancellable(&self, token: &CancellationToken) -> Result<Outcome, SagaFailure> {
        self.run_inner(token, &StepContext::new(), &NoOpOrchestrationEvents)
            .await
    }

    /// Executes the saga threading `ctx` through every context-aware step
    /// ([`Step::with_context`]) so later steps can consume earlier results —
    /// inter-step data passing.
    pub async fn run_with_context(&self, ctx: &StepContext) -> Result<Outcome, SagaFailure> {
        self.run_inner(&CancellationToken::new(), ctx, &NoOpOrchestrationEvents)
            .await
    }

    /// Executes the saga with both an explicit cancellation token and a
    /// shared [`StepContext`].
    pub async fn run_with_context_cancellable(
        &self,
        token: &CancellationToken,
        ctx: &StepContext,
    ) -> Result<Outcome, SagaFailure> {
        self.run_inner(token, ctx, &NoOpOrchestrationEvents).await
    }

    /// Executes the saga, firing lifecycle hooks on `listener` —
    /// pyfly's `OrchestrationEvents` wiring. The listener observes
    /// `on_start`, per-step `on_step_started` / `on_step_success` /
    /// `on_step_failed`, `on_compensation_started` / `on_step_compensated`,
    /// and `on_completed`. Behaviour and wire output are otherwise identical
    /// to [`Saga::run`].
    pub async fn run_with_listener(
        &self,
        listener: Arc<dyn OrchestrationEvents>,
    ) -> Result<Outcome, SagaFailure> {
        self.run_inner(
            &CancellationToken::new(),
            &StepContext::new(),
            listener.as_ref(),
        )
        .await
    }

    /// Executes the saga with an explicit [`StepContext`] and lifecycle
    /// `listener` — the most general saga run.
    pub async fn run_with_context_and_listener(
        &self,
        token: &CancellationToken,
        ctx: &StepContext,
        listener: &dyn OrchestrationEvents,
    ) -> Result<Outcome, SagaFailure> {
        self.run_inner(token, ctx, listener).await
    }

    async fn run_inner(
        &self,
        token: &CancellationToken,
        ctx: &StepContext,
        listener: &dyn OrchestrationEvents,
    ) -> Result<Outcome, SagaFailure> {
        let started_at = Utc::now();
        let cid = ctx.correlation_id();
        listener
            .on_start(&self.name, ExecutionPattern::Saga, &cid)
            .await;
        let mut steps_executed: Vec<String> = Vec::new();
        let mut executed: Vec<usize> = Vec::new();

        for (i, step) in self.steps.iter().enumerate() {
            if token.is_cancelled() {
                let error = SagaError::Cancelled;
                let outcome = Outcome {
                    saga: self.name.clone(),
                    status: SagaStatus::Failed,
                    steps_executed,
                    steps_rolled: Vec::new(),
                    error: Some(error.to_string()),
                    started_at,
                    finished_at: Utc::now(),
                };
                listener
                    .on_completed(
                        &self.name,
                        ExecutionPattern::Saga,
                        &cid,
                        false,
                        duration_ms(started_at, outcome.finished_at),
                    )
                    .await;
                return Err(SagaFailure {
                    outcome: Box::new(outcome),
                    error,
                });
            }
            listener.on_step_started(&self.name, &cid, &step.name).await;
            // Apply the per-step RetryPolicy (max attempts, exponential
            // backoff, jitter, per-attempt timeout) — pyfly's StepInvoker.
            let step_start = Instant::now();
            let exec_result =
                invoke_with_policy(&step.name, &step.retry, ctx, |ctx| step.execute.call(ctx))
                    .await;
            let latency_ms = step_start.elapsed().as_secs_f64() * 1000.0;
            if let Err(invoke_err) = exec_result {
                let attempts = invoke_err.attempts();
                // Preserve the historical `step "name": <cause>` message
                // shape: a single-attempt failure unwraps to its original
                // cause, so the wrapping is identical to the pre-retry
                // engine. A genuine retry-exhaustion / timeout surfaces the
                // invoker error.
                let cause: BoxError = unwrap_invoke_cause(&step.retry, invoke_err);
                let step_error = SagaError::Step {
                    step: step.name.clone(),
                    source: cause,
                };
                let step_message = step_error.to_string();
                listener
                    .on_step_failed(
                        &self.name,
                        &cid,
                        &step.name,
                        &step_message,
                        attempts,
                        latency_ms,
                    )
                    .await;
                let (steps_rolled, compensation_failure) =
                    self.compensate(ctx, &executed, listener, &cid).await;
                let outcome = Outcome {
                    saga: self.name.clone(),
                    status: SagaStatus::Compensated,
                    steps_executed,
                    steps_rolled,
                    error: Some(step_message),
                    started_at,
                    finished_at: Utc::now(),
                };
                let error = match (compensation_failure, self.policy) {
                    (Some(compensate), CompensationPolicy::StopOnError) => {
                        SagaError::Compensation {
                            step: step.name.clone(),
                            original: Box::new(step_error),
                            compensate,
                        }
                    }
                    _ => step_error,
                };
                listener
                    .on_completed(
                        &self.name,
                        ExecutionPattern::Saga,
                        &cid,
                        false,
                        duration_ms(started_at, outcome.finished_at),
                    )
                    .await;
                return Err(SagaFailure {
                    outcome: Box::new(outcome),
                    error,
                });
            }
            listener
                .on_step_success(&self.name, &cid, &step.name, 1, latency_ms)
                .await;
            executed.push(i);
            steps_executed.push(step.name.clone());
        }

        let finished_at = Utc::now();
        listener
            .on_completed(
                &self.name,
                ExecutionPattern::Saga,
                &cid,
                true,
                duration_ms(started_at, finished_at),
            )
            .await;
        Ok(Outcome {
            saga: self.name.clone(),
            status: SagaStatus::Completed,
            steps_executed,
            steps_rolled: Vec::new(),
            error: None,
            started_at,
            finished_at,
        })
    }

    /// Rolls back the executed steps in reverse order, returning the names
    /// rolled and the first compensation error (if any). Under
    /// [`CompensationPolicy::StopOnError`] the rollback aborts at the first
    /// failure; under [`CompensationPolicy::BestEffort`] it continues.
    async fn compensate(
        &self,
        ctx: &StepContext,
        executed: &[usize],
        listener: &dyn OrchestrationEvents,
        cid: &str,
    ) -> (Vec<String>, Option<BoxError>) {
        if !executed.is_empty() {
            listener.on_compensation_started(&self.name, cid).await;
        }
        let mut rolled = Vec::new();
        let mut first_err: Option<BoxError> = None;
        for &i in executed.iter().rev() {
            let step = &self.steps[i];
            let Some(compensate) = &step.compensate else {
                continue;
            };
            match compensate.call(ctx).await {
                Ok(()) => {
                    listener
                        .on_step_compensated(&self.name, cid, &step.name, None)
                        .await;
                    rolled.push(step.name.clone());
                }
                Err(err) => {
                    listener
                        .on_step_compensated(&self.name, cid, &step.name, Some(&err.to_string()))
                        .await;
                    if self.policy == CompensationPolicy::StopOnError {
                        return (rolled, Some(err));
                    }
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }
        (rolled, first_err)
    }
}

/// Wall-clock milliseconds between two timestamps, clamped at zero — the
/// duration unit the [`OrchestrationEvents`] hooks report.
fn duration_ms(start: DateTime<Utc>, end: DateTime<Utc>) -> f64 {
    (end - start).num_milliseconds().max(0) as f64
}

impl fmt::Debug for Saga {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Saga")
            .field("name", &self.name)
            .field("steps", &self.steps)
            .field("policy", &self.policy)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    fn recording_step(name: &str, log: &Log) -> Step {
        let entry = name.to_string();
        let log = log.clone();
        Step::new(name, move || {
            let log = log.clone();
            let entry = entry.clone();
            async move {
                log.lock().unwrap().push(entry);
                Ok(())
            }
        })
    }

    fn step_with_rollback(name: &str, fail: bool, rollbacks: &Log) -> Step {
        let entry = name.to_string();
        let rollbacks = rollbacks.clone();
        Step::new(name, move || async move {
            if fail {
                Err("boom".into())
            } else {
                Ok(())
            }
        })
        .with_compensation(move || {
            let rollbacks = rollbacks.clone();
            let entry = entry.clone();
            async move {
                rollbacks.lock().unwrap().push(entry);
                Ok(())
            }
        })
    }

    // Port of Go TestSagaCompletes.
    #[tokio::test]
    async fn saga_completes() {
        let ran: Log = Arc::new(Mutex::new(Vec::new()));
        let saga = Saga::new("checkout")
            .step(recording_step("reserve", &ran))
            .step(recording_step("charge", &ran))
            .step(recording_step("ship", &ran));

        let out = saga.run().await.expect("saga should complete");
        assert_eq!(out.status, SagaStatus::Completed);
        assert_eq!(out.steps_executed, ["reserve", "charge", "ship"]);
        assert!(out.steps_rolled.is_empty());
        assert!(out.error.is_none());
        assert_eq!(*ran.lock().unwrap(), ["reserve", "charge", "ship"]);
        assert!(out.finished_at >= out.started_at);
    }

    // Port of Go TestSagaCompensatesOnFailure.
    #[tokio::test]
    async fn saga_compensates_on_failure_in_reverse_order() {
        let rollbacks: Log = Arc::new(Mutex::new(Vec::new()));
        let saga = Saga::new("checkout")
            .step(step_with_rollback("reserve", false, &rollbacks))
            .step(step_with_rollback("charge", false, &rollbacks))
            .step(step_with_rollback("ship", true, &rollbacks));

        let failure = saga.run().await.expect_err("expected error");
        assert_eq!(failure.outcome().status, SagaStatus::Compensated);
        // Reverse order: charge, reserve.
        assert_eq!(*rollbacks.lock().unwrap(), ["charge", "reserve"]);
        assert_eq!(failure.outcome().steps_rolled, ["charge", "reserve"]);
        assert_eq!(failure.outcome().steps_executed, ["reserve", "charge"]);
        // Error message matches the Go port's `step %q: %w` wrapping.
        assert_eq!(failure.to_string(), "step \"ship\": boom");
        assert_eq!(
            failure.outcome().error.as_deref(),
            Some("step \"ship\": boom")
        );
        assert!(!failure.is_compensation_error());
    }

    // Port of Go TestSagaCompensationStopOnError.
    #[tokio::test]
    async fn saga_compensation_stop_on_error_surfaces_compensation_error() {
        let comp_ok = Step::new("a", || async { Ok(()) }).with_compensation(|| async { Ok(()) });
        let comp_fail = Step::new("b", || async { Ok(()) })
            .with_compensation(|| async { Err("compensate-fail".into()) });
        let failing_step = Step::new("trigger", || async { Err("trigger".into()) });

        let saga = Saga::new("policy")
            .policy(CompensationPolicy::StopOnError)
            .step(comp_ok)
            .step(comp_fail)
            .step(failing_step);

        let failure = saga.run().await.expect_err("expected error");
        assert!(failure.is_compensation_error());
        assert!(failure.error().is_compensation_error());
        // Rollback aborted at the first compensation failure ("b"), so
        // nothing was successfully rolled back.
        assert!(failure.outcome().steps_rolled.is_empty());
        // Display matches the Go CompensationError format.
        assert_eq!(
            failure.to_string(),
            "saga: compensation failed at step \"trigger\": compensate-fail \
             (original: step \"trigger\": trigger)"
        );
        // The outcome records the original step error, as in Go.
        assert_eq!(
            failure.outcome().error.as_deref(),
            Some("step \"trigger\": trigger")
        );
    }

    // Rust-specific: best-effort policy keeps compensating after a failure.
    #[tokio::test]
    async fn saga_best_effort_continues_compensating_after_failure() {
        let comp_ok: Log = Arc::new(Mutex::new(Vec::new()));
        let log = comp_ok.clone();
        let a = Step::new("a", || async { Ok(()) }).with_compensation(move || {
            let log = log.clone();
            async move {
                log.lock().unwrap().push("a".to_string());
                Ok(())
            }
        });
        let b = Step::new("b", || async { Ok(()) })
            .with_compensation(|| async { Err("compensate-fail".into()) });
        let trigger = Step::new("trigger", || async { Err("trigger".into()) });

        let saga = Saga::new("policy").step(a).step(b).step(trigger);
        let failure = saga.run().await.expect_err("expected error");
        // Best effort: not a CompensationError; "b" failed but "a" still ran.
        assert!(!failure.is_compensation_error());
        assert_eq!(failure.outcome().steps_rolled, ["a"]);
        assert_eq!(*comp_ok.lock().unwrap(), ["a"]);
    }

    // Rust-specific: a failing first step compensates nothing.
    #[tokio::test]
    async fn saga_first_step_failure_compensates_nothing() {
        let rollbacks: Log = Arc::new(Mutex::new(Vec::new()));
        let saga = Saga::new("checkout").step(step_with_rollback("only", true, &rollbacks));
        let failure = saga.run().await.expect_err("expected error");
        assert_eq!(failure.outcome().status, SagaStatus::Compensated);
        assert!(failure.outcome().steps_rolled.is_empty());
        assert!(rollbacks.lock().unwrap().is_empty());
    }

    // Rust-specific: empty sagas complete.
    #[tokio::test]
    async fn saga_with_no_steps_completes() {
        let out = Saga::new("empty").run().await.expect("empty saga is ok");
        assert_eq!(out.status, SagaStatus::Completed);
        assert!(out.steps_executed.is_empty());
    }

    // Rust-specific port of the Go ctx.Err() branch: cancellation fails the
    // run without compensating.
    #[tokio::test]
    async fn saga_cancellation_marks_outcome_failed() {
        let token = CancellationToken::new();
        let rollbacks: Log = Arc::new(Mutex::new(Vec::new()));
        let cancel = token.clone();
        let first = Step::new("one", move || {
            let cancel = cancel.clone();
            async move {
                cancel.cancel();
                Ok(())
            }
        })
        .with_compensation({
            let rollbacks = rollbacks.clone();
            move || {
                let rollbacks = rollbacks.clone();
                async move {
                    rollbacks.lock().unwrap().push("one".to_string());
                    Ok(())
                }
            }
        });
        let second = Step::new("two", || async { Ok(()) });

        let saga = Saga::new("cancelled").step(first).step(second);
        let failure = saga
            .run_cancellable(&token)
            .await
            .expect_err("expected cancellation");
        assert_eq!(failure.outcome().status, SagaStatus::Failed);
        assert!(matches!(failure.error(), SagaError::Cancelled));
        assert_eq!(failure.outcome().steps_executed, ["one"]);
        // Cancellation does not compensate, mirroring Go.
        assert!(rollbacks.lock().unwrap().is_empty());
    }

    // Rust-specific: into_parts decomposition and Error::source chain.
    #[tokio::test]
    async fn saga_failure_decomposes_into_outcome_and_error() {
        let saga = Saga::new("s").step(Step::new("a", || async { Err("boom".into()) }));
        let failure = saga.run().await.expect_err("expected error");
        assert!(std::error::Error::source(&failure).is_some());
        let (outcome, error) = failure.into_parts();
        assert_eq!(outcome.status, SagaStatus::Compensated);
        assert!(matches!(error, SagaError::Step { .. }));
    }

    // Rust-specific: outcome serializes with the Go status strings.
    #[test]
    fn outcome_serde_round_trip() {
        let outcome = Outcome {
            saga: "checkout".to_string(),
            status: SagaStatus::Compensated,
            steps_executed: vec!["reserve".to_string()],
            steps_rolled: vec!["reserve".to_string()],
            error: Some("step \"charge\": boom".to_string()),
            started_at: Utc::now(),
            finished_at: Utc::now(),
        };
        let json = serde_json::to_value(&outcome).expect("serialize");
        assert_eq!(json["status"], "compensated");
        assert_eq!(json["saga"], "checkout");
        assert_eq!(json["steps_executed"][0], "reserve");
        let back: Outcome = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.status, outcome.status);
        assert_eq!(back.steps_executed, outcome.steps_executed);
        assert_eq!(back.error, outcome.error);
    }

    // Rust-specific: status strings match the Go port exactly.
    #[test]
    fn saga_status_display_matches_go_strings() {
        assert_eq!(SagaStatus::Completed.to_string(), "completed");
        assert_eq!(SagaStatus::Compensated.to_string(), "compensated");
        assert_eq!(SagaStatus::Failed.to_string(), "failed");
    }

    // ── Per-step retry enforcement (pyfly StepInvoker) ──────────────────

    // A flaky step succeeds within its retry budget; the saga completes.
    #[tokio::test]
    async fn saga_step_retries_until_success() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = Arc::new(AtomicU32::new(0));
        let a = attempts.clone();
        let saga = Saga::new("retrying").step(
            Step::new("flaky", move || {
                let a = a.clone();
                async move {
                    if a.fetch_add(1, Ordering::SeqCst) < 2 {
                        Err("transient".into())
                    } else {
                        Ok(())
                    }
                }
            })
            .with_retry(RetryPolicy {
                max_attempts: 5,
                backoff_ms: 1,
                ..Default::default()
            }),
        );
        let out = saga.run().await.expect("completes after retries");
        assert_eq!(out.status, SagaStatus::Completed);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    // A step exhausting its retries fails the saga; the error surfaces the
    // retry context and compensation runs.
    #[tokio::test]
    async fn saga_step_retry_exhausted_compensates() {
        let rollbacks: Log = Arc::new(Mutex::new(Vec::new()));
        let rb = rollbacks.clone();
        let saga = Saga::new("exhaust")
            .step(
                Step::new("ok", || async { Ok(()) }).with_compensation(move || {
                    let rb = rb.clone();
                    async move {
                        rb.lock().unwrap().push("ok".to_string());
                        Ok(())
                    }
                }),
            )
            .step(
                Step::new("always-fails", || async { Err("nope".into()) }).with_retry(
                    RetryPolicy {
                        max_attempts: 3,
                        backoff_ms: 1,
                        ..Default::default()
                    },
                ),
            );
        let failure = saga.run().await.expect_err("must fail");
        assert_eq!(failure.outcome().status, SagaStatus::Compensated);
        // Compensation of the earlier step ran.
        assert_eq!(*rollbacks.lock().unwrap(), ["ok"]);
        // The error message includes the retry-exhaustion context.
        assert!(failure.to_string().contains("3 attempt"));
    }

    // ── Inter-step data passing (pyfly FromStep / Input argument injection) ──

    #[tokio::test]
    async fn saga_threads_data_between_steps() {
        use serde_json::json;
        let ctx = StepContext::with_input(json!({"amount": 250}));
        let saga = Saga::new("payment")
            .step(Step::with_context("authorize", |ctx| async move {
                let amount = ctx.input_field("amount").unwrap();
                ctx.set_result("authorize", json!({"auth_id": "A-1", "amount": amount}));
                Ok(())
            }))
            .step(Step::with_context("capture", |ctx| async move {
                // Read the prior step's auth id.
                let auth = ctx.result_field("authorize", "auth_id").unwrap();
                ctx.set_result("capture", json!({"captured": auth}));
                Ok(())
            }));
        let out = saga.run_with_context(&ctx).await.expect("completes");
        assert_eq!(out.status, SagaStatus::Completed);
        assert_eq!(
            ctx.result_field("capture", "captured").unwrap(),
            json!("A-1")
        );
    }
}
