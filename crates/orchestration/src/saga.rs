//! Saga engine: sequential steps with reverse-order compensation.

use crate::{boxed_action, ActionFn, BoxError, CancellationToken};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
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

/// A single saga unit. The execute action moves the saga forward; the
/// optional compensation rolls back the side-effects of execute. Steps must
/// be idempotent — the engine may retry both phases.
pub struct Step {
    name: String,
    execute: ActionFn,
    compensate: Option<ActionFn>,
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
            execute: boxed_action(execute),
            compensate: None,
        }
    }

    /// Attaches an async compensation action that rolls back the
    /// side-effects of execute.
    pub fn with_compensation<F, Fut>(mut self, compensate: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.compensate = Some(boxed_action(compensate));
        self
    }

    /// The step name, as reported in [`Outcome`] and error messages.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("name", &self.name)
            .field("has_compensation", &self.compensate.is_some())
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
        let started_at = Utc::now();
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
                return Err(SagaFailure {
                    outcome: Box::new(outcome),
                    error,
                });
            }
            if let Err(cause) = (step.execute)().await {
                let step_error = SagaError::Step {
                    step: step.name.clone(),
                    source: cause,
                };
                let step_message = step_error.to_string();
                let (steps_rolled, compensation_failure) = self.compensate(&executed).await;
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
                return Err(SagaFailure {
                    outcome: Box::new(outcome),
                    error,
                });
            }
            executed.push(i);
            steps_executed.push(step.name.clone());
        }

        Ok(Outcome {
            saga: self.name.clone(),
            status: SagaStatus::Completed,
            steps_executed,
            steps_rolled: Vec::new(),
            error: None,
            started_at,
            finished_at: Utc::now(),
        })
    }

    /// Rolls back the executed steps in reverse order, returning the names
    /// rolled and the first compensation error (if any). Under
    /// [`CompensationPolicy::StopOnError`] the rollback aborts at the first
    /// failure; under [`CompensationPolicy::BestEffort`] it continues.
    async fn compensate(&self, executed: &[usize]) -> (Vec<String>, Option<BoxError>) {
        let mut rolled = Vec::new();
        let mut first_err: Option<BoxError> = None;
        for &i in executed.iter().rev() {
            let step = &self.steps[i];
            let Some(compensate) = &step.compensate else {
                continue;
            };
            match compensate().await {
                Ok(()) => rolled.push(step.name.clone()),
                Err(err) => {
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
}
