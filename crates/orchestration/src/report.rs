//! Execution reports — final per-run summaries surfaced to API callers.
//!
//! The Rust spelling of pyfly's `ExecutionReport` and
//! `ExecutionReportBuilder` (`pyfly.transactional.core.report`). The Python
//! builder walks an `ExecutionContext`'s step records; the Rust port builds
//! the same report from the engines' public results: a saga
//! [`Outcome`](crate::Outcome) or a persisted
//! [`ExecutionState`](crate::ExecutionState).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{ExecutionPattern, ExecutionStatus, StepStatus};
use crate::saga::{Outcome, SagaStatus};

/// Per-step summary inside an [`ExecutionReport`] — pyfly's `StepReport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepReport {
    /// The step / node / participant id.
    pub step_id: String,
    /// Terminal step status.
    pub status: StepStatus,
    /// How many times the step was attempted.
    pub attempts: u32,
    /// The step's JSON result, if it produced one.
    pub result: serde_json::Value,
    /// Rendered error message when the step failed.
    pub error: Option<String>,
}

impl StepReport {
    /// A `DONE` step report with one attempt and no result.
    pub fn done(step_id: impl Into<String>) -> Self {
        Self {
            step_id: step_id.into(),
            status: StepStatus::Done,
            attempts: 1,
            result: serde_json::Value::Null,
            error: None,
        }
    }
}

/// Per-step compensation summary — pyfly's `CompensationReport`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompensationReport {
    /// The compensated step id.
    pub step_id: String,
    /// Status after compensation (`COMPENSATED` / `COMPENSATION_FAILED`).
    pub status: StepStatus,
    /// The compensation's JSON result, if any.
    pub result: serde_json::Value,
    /// Rendered error message when compensation failed.
    pub error: Option<String>,
}

/// Final per-run summary — pyfly's `ExecutionReport`. Serializes with the
/// same field names the Python REST controllers emit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// The definition name.
    pub name: String,
    /// Which engine produced the run.
    pub pattern: ExecutionPattern,
    /// Correlation id of the run.
    pub correlation_id: String,
    /// Terminal lifecycle status.
    pub status: ExecutionStatus,
    /// UTC instant the run started.
    pub started_at: DateTime<Utc>,
    /// UTC instant the run finished, if it has.
    pub completed_at: Option<DateTime<Utc>>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: f64,
    /// Rendered error message when the run failed.
    pub error: Option<String>,
    /// Per-step summaries, in execution order.
    pub steps: Vec<StepReport>,
    /// Per-step compensation summaries, in compensation order.
    pub compensations: Vec<CompensationReport>,
    /// Engine variables captured at completion.
    pub variables: serde_json::Map<String, serde_json::Value>,
}

impl ExecutionReport {
    /// `true` when [`Self::status`] is a successful terminal status —
    /// pyfly's `successful`.
    pub fn successful(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Confirmed
        )
    }

    /// Builds a report from a saga [`Outcome`](crate::Outcome) and its
    /// correlation id — the engine analogue of pyfly's
    /// `ExecutionReportBuilder.build(ctx)`.
    ///
    /// Each executed step becomes a `DONE` [`StepReport`]; each rolled-back
    /// step becomes a `COMPENSATED` [`CompensationReport`]. The execution
    /// status is mapped from the saga's [`SagaStatus`](crate::SagaStatus).
    pub fn from_saga_outcome(correlation_id: impl Into<String>, outcome: &Outcome) -> Self {
        let status = match outcome.status {
            SagaStatus::Completed => ExecutionStatus::Completed,
            SagaStatus::Compensated => ExecutionStatus::Compensated,
            SagaStatus::Failed => ExecutionStatus::Failed,
        };
        let steps = outcome
            .steps_executed
            .iter()
            .map(StepReport::done)
            .collect();
        let compensations = outcome
            .steps_rolled
            .iter()
            .map(|step_id| CompensationReport {
                step_id: step_id.clone(),
                status: StepStatus::Compensated,
                result: serde_json::Value::Null,
                error: None,
            })
            .collect();
        let duration_ms = (outcome.finished_at - outcome.started_at)
            .num_milliseconds()
            .max(0) as f64;
        Self {
            name: outcome.saga.clone(),
            pattern: ExecutionPattern::Saga,
            correlation_id: correlation_id.into(),
            status,
            started_at: outcome.started_at,
            completed_at: Some(outcome.finished_at),
            duration_ms,
            error: outcome.error.clone(),
            steps,
            compensations,
            variables: serde_json::Map::new(),
        }
    }

    /// Builds a header-only report from a persisted
    /// [`ExecutionState`](crate::ExecutionState) — the shape the REST
    /// listing surfaces without step detail.
    pub fn from_state(state: &crate::model::ExecutionState) -> Self {
        let duration_ms = state
            .completed_at
            .map(|done| (done - state.started_at).num_milliseconds().max(0) as f64)
            .unwrap_or(0.0);
        let variables = state.payload.as_object().cloned().unwrap_or_default();
        Self {
            name: state.name.clone(),
            pattern: state.pattern,
            correlation_id: state.correlation_id.clone(),
            status: state.status,
            started_at: state.started_at,
            completed_at: state.completed_at,
            duration_ms,
            error: None,
            steps: Vec::new(),
            compensations: Vec::new(),
            variables,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionState, Saga, Step};

    // The report mirrors a completed saga's executed steps.
    #[tokio::test]
    async fn from_completed_saga_outcome() {
        let saga = Saga::new("checkout")
            .step(Step::new("reserve", || async { Ok(()) }))
            .step(Step::new("charge", || async { Ok(()) }));
        let outcome = saga.run().await.expect("completes");
        let report = ExecutionReport::from_saga_outcome("cid-1", &outcome);
        assert_eq!(report.name, "checkout");
        assert_eq!(report.pattern, ExecutionPattern::Saga);
        assert_eq!(report.correlation_id, "cid-1");
        assert_eq!(report.status, ExecutionStatus::Completed);
        assert!(report.successful());
        assert_eq!(
            report
                .steps
                .iter()
                .map(|s| s.step_id.as_str())
                .collect::<Vec<_>>(),
            ["reserve", "charge"]
        );
        assert!(report.compensations.is_empty());
        assert!(report.error.is_none());
    }

    // A compensated saga reports the rolled-back steps.
    #[tokio::test]
    async fn from_compensated_saga_outcome() {
        let saga = Saga::new("checkout")
            .step(Step::new("reserve", || async { Ok(()) }).with_compensation(|| async { Ok(()) }))
            .step(Step::new("charge", || async { Err("declined".into()) }));
        let failure = saga.run().await.expect_err("fails");
        let report = ExecutionReport::from_saga_outcome("cid-2", failure.outcome());
        assert_eq!(report.status, ExecutionStatus::Compensated);
        assert!(!report.successful());
        assert_eq!(report.steps, vec![StepReport::done("reserve")]);
        assert_eq!(report.compensations.len(), 1);
        assert_eq!(report.compensations[0].step_id, "reserve");
        assert_eq!(report.compensations[0].status, StepStatus::Compensated);
    }

    // Header-only report from a persisted state.
    #[test]
    fn from_state_carries_header_fields() {
        let mut state = ExecutionState::new("cid-3", "order", ExecutionPattern::Workflow);
        state.payload = serde_json::json!({"amount": 100});
        state.transition(ExecutionStatus::Completed);
        let report = ExecutionReport::from_state(&state);
        assert_eq!(report.name, "order");
        assert_eq!(report.pattern, ExecutionPattern::Workflow);
        assert_eq!(report.status, ExecutionStatus::Completed);
        assert!(report.completed_at.is_some());
        assert_eq!(report.variables["amount"], serde_json::json!(100));
    }

    // Wire field names match pyfly's controller output.
    #[tokio::test]
    async fn serializes_to_pyfly_wire_shape() {
        let saga = Saga::new("s").step(Step::new("a", || async { Ok(()) }));
        let outcome = saga.run().await.unwrap();
        let report = ExecutionReport::from_saga_outcome("c", &outcome);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["name"], "s");
        assert_eq!(json["pattern"], "SAGA");
        assert_eq!(json["status"], "COMPLETED");
        assert_eq!(json["steps"][0]["step_id"], "a");
        assert_eq!(json["steps"][0]["status"], "DONE");
        assert!(json["duration_ms"].is_number());
    }
}
