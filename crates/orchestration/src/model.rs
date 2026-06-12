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

//! Core orchestration model — enums and value types shared across patterns.
//!
//! The Rust spelling of pyfly's `pyfly.transactional.core.model` /
//! `core.persistence` value layer, itself mirroring
//! `org.fireflyframework.orchestration.core.model` from the Java engine.
//! Saga, workflow, and TCC executions all speak this vocabulary, and the
//! wire strings (`"RUNNING"`, `"TIMED_OUT"`, `"SAGA"`, …) match the Java,
//! Python, Go, and .NET ports byte for byte.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle status of an orchestration execution.
///
/// Covers all three patterns (saga, workflow, TCC). Pattern-specific
/// transitions (TCC's `TRYING` / `CONFIRMING` / `CANCELING`) coexist with
/// universal ones (`RUNNING`, `COMPLETED`, `FAILED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExecutionStatus {
    /// Accepted but not started yet (also the `ASYNC` trigger answer).
    Pending,
    /// Steps are actively executing.
    Running,
    /// Blocked on a signal or timer.
    Waiting,
    /// Paused by an operator.
    Suspended,
    /// Finished successfully — terminal.
    Completed,
    /// Finished with an unrecovered error — terminal.
    Failed,
    /// Cancelled by the caller — terminal.
    Cancelled,
    /// Exceeded its execution deadline — terminal.
    TimedOut,
    /// TCC: try phase in progress.
    Trying,
    /// TCC: confirm phase in progress.
    Confirming,
    /// TCC: every participant confirmed — terminal.
    Confirmed,
    /// TCC: cancel phase in progress.
    Canceling,
    /// TCC: every tried participant cancelled — terminal.
    Canceled,
    /// Saga: compensation in progress.
    Compensating,
    /// Saga: compensation finished — terminal.
    Compensated,
}

impl ExecutionStatus {
    /// All statuses, in declaration order — handy for exhaustive checks.
    pub const ALL: [ExecutionStatus; 15] = [
        Self::Pending,
        Self::Running,
        Self::Waiting,
        Self::Suspended,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
        Self::TimedOut,
        Self::Trying,
        Self::Confirming,
        Self::Confirmed,
        Self::Canceling,
        Self::Canceled,
        Self::Compensating,
        Self::Compensated,
    ];

    /// `true` when the execution can no longer change state.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::TimedOut
                | Self::Confirmed
                | Self::Canceled
                | Self::Compensated
        )
    }

    /// The canonical wire string, e.g. `"TIMED_OUT"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Running => "RUNNING",
            Self::Waiting => "WAITING",
            Self::Suspended => "SUSPENDED",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::TimedOut => "TIMED_OUT",
            Self::Trying => "TRYING",
            Self::Confirming => "CONFIRMING",
            Self::Confirmed => "CONFIRMED",
            Self::Canceling => "CANCELING",
            Self::Canceled => "CANCELED",
            Self::Compensating => "COMPENSATING",
            Self::Compensated => "COMPENSATED",
        }
    }

    /// Parses the canonical wire string (`"RUNNING"`, `"TIMED_OUT"`, …).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == s)
    }
}

impl fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which orchestration pattern produced an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExecutionPattern {
    /// Sequential steps with reverse-order compensation.
    Saga,
    /// DAG of nodes executed in topological waves.
    Workflow,
    /// Try-Confirm-Cancel two-phase orchestration.
    Tcc,
}

impl ExecutionPattern {
    /// The canonical wire string, e.g. `"SAGA"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Saga => "SAGA",
            Self::Workflow => "WORKFLOW",
            Self::Tcc => "TCC",
        }
    }
}

impl fmt::Display for ExecutionPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the caller wants to interact with an execution start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TriggerMode {
    /// Run to completion on the caller's task and return the outcome.
    #[default]
    Sync,
    /// Spawn the run in the background and answer immediately with
    /// [`ExecutionStatus::Pending`].
    Async,
}

impl fmt::Display for TriggerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sync => "SYNC",
            Self::Async => "ASYNC",
        })
    }
}

/// Lifecycle status of a single step within an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StepStatus {
    /// Not started yet.
    Pending,
    /// Currently executing.
    Running,
    /// Executed successfully.
    Done,
    /// Execution failed.
    Failed,
    /// Skipped (unsatisfied dependency or short-circuit).
    Skipped,
    /// Compensation in progress.
    Compensating,
    /// Compensation completed.
    Compensated,
    /// Compensation itself failed.
    CompensationFailed,
}

impl fmt::Display for StepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "PENDING",
            Self::Running => "RUNNING",
            Self::Done => "DONE",
            Self::Failed => "FAILED",
            Self::Skipped => "SKIPPED",
            Self::Compensating => "COMPENSATING",
            Self::Compensated => "COMPENSATED",
            Self::CompensationFailed => "COMPENSATION_FAILED",
        })
    }
}

/// One of the three TCC execution phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TccPhase {
    /// Reserve the resource.
    Try,
    /// Finalise the reservation.
    Confirm,
    /// Roll the reservation back.
    Cancel,
}

impl fmt::Display for TccPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Try => "TRY",
            Self::Confirm => "CONFIRM",
            Self::Cancel => "CANCEL",
        })
    }
}

/// Immutable retry configuration for a single step or participant —
/// pyfly's frozen `RetryPolicy` dataclass.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Total attempts including the first call. `1` disables retry.
    pub max_attempts: u32,
    /// Base backoff between attempts, in milliseconds.
    pub backoff_ms: u64,
    /// Per-attempt timeout in milliseconds (`0` disables).
    pub timeout_ms: u64,
    /// Whether to apply random jitter to the backoff.
    pub jitter: bool,
    /// Fraction of `backoff_ms` used as jitter range (`0.0–1.0`).
    pub jitter_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff_ms: 0,
            timeout_ms: 0,
            jitter: false,
            jitter_factor: 0.0,
        }
    }
}

/// Serializable snapshot of an execution — pyfly's `ExecutionState`
/// dataclass, the unit persisted by every
/// [`PersistenceProvider`](crate::PersistenceProvider) adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Unique id correlating this run across services and signals.
    pub correlation_id: String,
    /// The saga / workflow / TCC definition name.
    pub name: String,
    /// Which pattern produced the execution.
    pub pattern: ExecutionPattern,
    /// Current lifecycle status.
    pub status: ExecutionStatus,
    /// UTC instant the run started.
    pub started_at: DateTime<Utc>,
    /// UTC instant the state last changed.
    pub updated_at: DateTime<Utc>,
    /// UTC instant the run reached a terminal status, if it has.
    pub completed_at: Option<DateTime<Utc>>,
    /// Opaque engine snapshot (step records, variables, input) —
    /// pyfly's `ExecutionContext.to_dict()` payload.
    pub payload: serde_json::Value,
}

impl ExecutionState {
    /// Creates a fresh state in [`ExecutionStatus::Pending`] with both
    /// timestamps set to now and an empty JSON-object payload.
    pub fn new(
        correlation_id: impl Into<String>,
        name: impl Into<String>,
        pattern: ExecutionPattern,
    ) -> Self {
        let now = Utc::now();
        Self {
            correlation_id: correlation_id.into(),
            name: name.into(),
            pattern,
            status: ExecutionStatus::Pending,
            started_at: now,
            updated_at: now,
            completed_at: None,
            payload: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Builder-style status override.
    #[must_use]
    pub fn with_status(mut self, status: ExecutionStatus) -> Self {
        self.status = status;
        self
    }

    /// Builder-style payload override.
    #[must_use]
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }

    /// `true` when [`Self::status`] is terminal.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// Bumps `updated_at` to now.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Transitions to `status` and bumps `updated_at`; terminal statuses
    /// also stamp `completed_at`.
    pub fn transition(&mut self, status: ExecutionStatus) {
        self.status = status;
        self.updated_at = Utc::now();
        if status.is_terminal() {
            self.completed_at = Some(self.updated_at);
        }
    }

    /// Serializes to the JSON wire form shared with pyfly's
    /// `StateSerializer.serialize` (Redis / file / network adapters).
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Deserializes the wire form produced by [`Self::to_json`] — pyfly's
    /// `StateSerializer.deserialize`.
    pub fn from_json(raw: &str) -> serde_json::Result<Self> {
        serde_json::from_str(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of pyfly TestExecutionStatus::test_terminal_states.
    #[test]
    fn terminal_states_match_pyfly() {
        let terminal = [
            ExecutionStatus::Completed,
            ExecutionStatus::Failed,
            ExecutionStatus::Cancelled,
            ExecutionStatus::TimedOut,
            ExecutionStatus::Confirmed,
            ExecutionStatus::Canceled,
            ExecutionStatus::Compensated,
        ];
        for status in ExecutionStatus::ALL {
            assert_eq!(status.is_terminal(), terminal.contains(&status), "{status}");
        }
    }

    // Port of pyfly TestExecutionStatus::test_running_is_not_terminal.
    #[test]
    fn running_and_pending_are_not_terminal() {
        assert!(!ExecutionStatus::Running.is_terminal());
        assert!(!ExecutionStatus::Pending.is_terminal());
    }

    // Port of pyfly TestEnums::test_pattern_values / test_trigger_mode /
    // test_step_status_values / test_tcc_phase.
    #[test]
    fn enum_wire_strings_match_pyfly() {
        assert_eq!(ExecutionPattern::Saga.to_string(), "SAGA");
        assert_eq!(ExecutionPattern::Workflow.to_string(), "WORKFLOW");
        assert_eq!(ExecutionPattern::Tcc.to_string(), "TCC");
        assert_eq!(TriggerMode::Sync.to_string(), "SYNC");
        assert_eq!(TriggerMode::Async.to_string(), "ASYNC");
        assert_eq!(
            StepStatus::CompensationFailed.to_string(),
            "COMPENSATION_FAILED"
        );
        assert_eq!(TccPhase::Try.to_string(), "TRY");
        assert_eq!(TccPhase::Confirm.to_string(), "CONFIRM");
        assert_eq!(TccPhase::Cancel.to_string(), "CANCEL");
        assert_eq!(ExecutionStatus::TimedOut.to_string(), "TIMED_OUT");
    }

    #[test]
    fn status_parse_round_trips() {
        for status in ExecutionStatus::ALL {
            assert_eq!(ExecutionStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(ExecutionStatus::parse("NOPE"), None);
    }

    // Port of pyfly TestRetryPolicy.
    #[test]
    fn retry_policy_defaults_and_custom() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_attempts, 1);
        assert_eq!(p.backoff_ms, 0);
        let p = RetryPolicy {
            max_attempts: 3,
            backoff_ms: 100,
            timeout_ms: 5000,
            jitter: true,
            jitter_factor: 0.2,
        };
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.timeout_ms, 5000);
        assert!(p.jitter);
    }

    // Port of pyfly TestSerialization::test_round_trip.
    #[test]
    fn execution_state_json_round_trip() {
        let mut state = ExecutionState::new("cid-1", "t", ExecutionPattern::Saga);
        state.transition(ExecutionStatus::Running);
        let raw = state.to_json().expect("serialize");
        let restored = ExecutionState::from_json(&raw).expect("deserialize");
        assert_eq!(restored.correlation_id, "cid-1");
        assert_eq!(restored.status, ExecutionStatus::Running);
        assert_eq!(restored.pattern, ExecutionPattern::Saga);
        // Wire field names match pyfly's StateSerializer.
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["pattern"], "SAGA");
        assert_eq!(value["status"], "RUNNING");
        assert!(value["completed_at"].is_null());
        assert!(value.get("payload").is_some());
    }

    #[test]
    fn transition_to_terminal_stamps_completed_at() {
        let mut state = ExecutionState::new("cid", "t", ExecutionPattern::Workflow);
        assert!(state.completed_at.is_none());
        state.transition(ExecutionStatus::Completed);
        assert!(state.completed_at.is_some());
        assert!(state.is_terminal());
    }
}
