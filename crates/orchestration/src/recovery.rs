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

//! Recovery service — finds executions stuck in a non-terminal status and
//! resumes, compensates, or fails them; also evicts old terminal history.
//!
//! The Rust spelling of pyfly's `RecoveryService`
//! (`pyfly.transactional.core.recovery`) and `SagaRecoveryService`
//! (`saga.persistence.recovery`): the periodic `asyncio` loop becomes an
//! explicit [`RecoveryService::recover_stale`] / [`RecoveryService::cleanup`]
//! pair the host's scheduler (e.g.
//! [`OrchestrationScheduler`](crate::OrchestrationScheduler)) drives.

use std::sync::Arc;

use chrono::{Duration, Utc};
use futures::future::BoxFuture;

use crate::model::{ExecutionState, ExecutionStatus};
use crate::persistence::{PersistenceError, PersistenceProvider};
use crate::BoxError;

/// What to do with one stuck execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Mark the execution [`ExecutionStatus::Failed`] — the default, and
    /// pyfly `SagaRecoveryService`'s only behavior.
    MarkFailed,
    /// Re-run it through the handler registered with
    /// [`RecoveryService::on_resume`].
    Resume,
    /// Roll it back through the handler registered with
    /// [`RecoveryService::on_compensate`].
    Compensate,
    /// Leave it untouched.
    Skip,
}

/// Chooses a [`RecoveryAction`] per stuck execution.
pub type RecoveryDecider = Arc<dyn Fn(&ExecutionState) -> RecoveryAction + Send + Sync>;

/// Async handler invoked to resume or compensate one execution.
pub type RecoveryHandler =
    Arc<dyn Fn(ExecutionState) -> BoxFuture<'static, Result<(), BoxError>> + Send + Sync>;

fn boxed_handler<F, Fut>(f: F) -> RecoveryHandler
where
    F: Fn(ExecutionState) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), BoxError>> + Send + 'static,
{
    Arc::new(move |state| Box::pin(f(state)))
}

/// Inspects persisted state to surface and repair stuck executions.
///
/// * Executions whose `updated_at` is older than the *stale threshold* and
///   whose status is non-terminal are considered stuck.
/// * Terminal executions older than the *retention period* are deleted by
///   [`Self::cleanup`].
pub struct RecoveryService {
    persistence: Arc<dyn PersistenceProvider>,
    stale_threshold: Duration,
    retention_period: Duration,
    decider: Option<RecoveryDecider>,
    resume: Option<RecoveryHandler>,
    compensate: Option<RecoveryHandler>,
}

impl std::fmt::Debug for RecoveryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryService")
            .field("stale_threshold", &self.stale_threshold)
            .field("retention_period", &self.retention_period)
            .finish_non_exhaustive()
    }
}

impl RecoveryService {
    /// Wraps a persistence provider with pyfly's defaults: one hour stale
    /// threshold, seven days retention.
    pub fn new(persistence: Arc<dyn PersistenceProvider>) -> Self {
        Self {
            persistence,
            stale_threshold: Duration::hours(1),
            retention_period: Duration::days(7),
            decider: None,
            resume: None,
            compensate: None,
        }
    }

    /// Sets how old (by `updated_at`) a non-terminal execution must be to
    /// count as stuck.
    #[must_use]
    pub fn stale_threshold(mut self, threshold: Duration) -> Self {
        self.stale_threshold = threshold;
        self
    }

    /// Sets how long terminal executions are retained before
    /// [`Self::cleanup`] deletes them.
    #[must_use]
    pub fn retention_period(mut self, retention: Duration) -> Self {
        self.retention_period = retention;
        self
    }

    /// Installs the policy choosing a [`RecoveryAction`] per stuck
    /// execution. Without one every stuck execution is
    /// [`RecoveryAction::MarkFailed`].
    #[must_use]
    pub fn decider(
        mut self,
        decider: impl Fn(&ExecutionState) -> RecoveryAction + Send + Sync + 'static,
    ) -> Self {
        self.decider = Some(Arc::new(decider));
        self
    }

    /// Installs the handler that re-runs an execution chosen for
    /// [`RecoveryAction::Resume`].
    #[must_use]
    pub fn on_resume<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(ExecutionState) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.resume = Some(boxed_handler(f));
        self
    }

    /// Installs the handler that rolls back an execution chosen for
    /// [`RecoveryAction::Compensate`].
    #[must_use]
    pub fn on_compensate<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(ExecutionState) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.compensate = Some(boxed_handler(f));
        self
    }

    /// Non-terminal executions whose `updated_at` is older than the stale
    /// threshold — pyfly's `find_stale`.
    pub async fn find_stale(&self) -> Result<Vec<ExecutionState>, PersistenceError> {
        let cutoff = Utc::now() - self.stale_threshold;
        self.persistence.list_stale(cutoff).await
    }

    /// Scans for stuck executions and repairs each according to the
    /// decider (default: mark FAILED). Returns how many executions were
    /// acted upon — pyfly `SagaRecoveryService.recover_stale`'s count.
    ///
    /// * [`RecoveryAction::MarkFailed`] — persists the execution as
    ///   [`ExecutionStatus::Failed`].
    /// * [`RecoveryAction::Resume`] — invokes the resume handler; success
    ///   re-marks the run [`ExecutionStatus::Completed`], failure marks it
    ///   [`ExecutionStatus::Failed`].
    /// * [`RecoveryAction::Compensate`] — invokes the compensate handler;
    ///   success marks the run [`ExecutionStatus::Compensated`], failure
    ///   [`ExecutionStatus::Failed`].
    /// * [`RecoveryAction::Skip`] — leaves the run untouched and does not
    ///   count it.
    ///
    /// # Concurrency
    ///
    /// Each stuck execution is *claimed* with an atomic compare-and-swap
    /// ([`PersistenceProvider::claim_stale`]) before its handler runs: the row
    /// is transitioned to an in-recovery marker (`RUNNING` for resume,
    /// `COMPENSATING` for compensate, `FAILED` for mark-failed) only if it is
    /// still stale. This makes overlapping recovery passes — e.g. a scheduled
    /// scan racing an operator-triggered scan — safe: the loser of the claim
    /// observes the row as no longer stale and skips it, so the side-effecting
    /// Resume/Compensate handler is never double-executed and the recovered
    /// count is not double-incremented.
    pub async fn recover_stale(&self) -> Result<usize, PersistenceError> {
        let cutoff = Utc::now() - self.stale_threshold;
        let stale = self.persistence.list_stale(cutoff).await?;
        let mut recovered = 0;
        for candidate in stale {
            let action = self
                .decider
                .as_ref()
                .map_or(RecoveryAction::MarkFailed, |d| d(&candidate));
            // The in-recovery marker each action claims the row with. Skip
            // never claims; the others atomically take ownership so a
            // concurrent scan that saw the same row in `list_stale` loses the
            // claim and moves on.
            let claim_status = match action {
                RecoveryAction::Skip => continue,
                RecoveryAction::MarkFailed => ExecutionStatus::Failed,
                RecoveryAction::Resume => ExecutionStatus::Running,
                RecoveryAction::Compensate => ExecutionStatus::Compensating,
            };
            let Some(mut state) = self
                .persistence
                .claim_stale(&candidate.correlation_id, cutoff, claim_status)
                .await?
            else {
                // Lost the claim (already recovered, refreshed, or another
                // pass owns it) — do not run the handler or count it.
                continue;
            };
            match action {
                // Already transitioned to FAILED by the atomic claim.
                RecoveryAction::MarkFailed => {}
                RecoveryAction::Resume => {
                    let outcome = match &self.resume {
                        Some(handler) => handler(state.clone()).await,
                        None => Err("no resume handler installed".into()),
                    };
                    let status = if outcome.is_ok() {
                        ExecutionStatus::Completed
                    } else {
                        ExecutionStatus::Failed
                    };
                    state.transition(status);
                    self.persistence.save(state).await?;
                }
                RecoveryAction::Compensate => {
                    let outcome = match &self.compensate {
                        Some(handler) => handler(state.clone()).await,
                        None => Err("no compensate handler installed".into()),
                    };
                    let status = if outcome.is_ok() {
                        ExecutionStatus::Compensated
                    } else {
                        ExecutionStatus::Failed
                    };
                    state.transition(status);
                    self.persistence.save(state).await?;
                }
                RecoveryAction::Skip => unreachable!("skip handled above"),
            }
            recovered += 1;
        }
        Ok(recovered)
    }

    /// Deletes terminal executions older than the retention period;
    /// returns how many were removed — pyfly's `cleanup`.
    pub async fn cleanup(&self) -> Result<usize, PersistenceError> {
        self.persistence.cleanup(self.retention_period).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ExecutionPattern;
    use crate::persistence::{ExecutionFilter, MemoryPersistence};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn stale_state(cid: &str, name: &str) -> ExecutionState {
        let mut state = ExecutionState::new(cid, name, ExecutionPattern::Saga);
        state.status = ExecutionStatus::Running;
        state.updated_at = Utc::now() - Duration::hours(1);
        state
    }

    fn service(persistence: &Arc<MemoryPersistence>) -> RecoveryService {
        RecoveryService::new(persistence.clone() as Arc<dyn PersistenceProvider>)
            .stale_threshold(Duration::seconds(60))
            .retention_period(Duration::hours(24))
    }

    // Port of pyfly TestRecoverStaleFindsAndMarks::test_marks_stale_saga_as_failed.
    #[tokio::test]
    async fn marks_stale_execution_as_failed() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("stale-1", "order-saga"))
            .await
            .unwrap();
        let recovery = service(&persistence);

        let count = recovery.recover_stale().await.expect("recover");
        assert_eq!(count, 1);
        let state = persistence.load("stale-1").await.unwrap().expect("present");
        assert_eq!(state.status, ExecutionStatus::Failed);
        assert!(state.completed_at.is_some());
    }

    // Port of pyfly test_marks_multiple_stale_sagas_as_failed +
    // test_returns_count_of_recovered_sagas.
    #[tokio::test]
    async fn marks_multiple_and_returns_count() {
        let persistence = Arc::new(MemoryPersistence::new());
        for cid in ["stale-1", "stale-2", "stale-3"] {
            persistence
                .save(stale_state(cid, "order-saga"))
                .await
                .unwrap();
        }
        let recovery = service(&persistence);
        assert_eq!(recovery.recover_stale().await.unwrap(), 3);
        for cid in ["stale-1", "stale-2", "stale-3"] {
            let state = persistence.load(cid).await.unwrap().expect("present");
            assert_eq!(state.status, ExecutionStatus::Failed);
        }
    }

    // Port of pyfly test_does_not_mark_recent_in_flight_sagas +
    // test_returns_zero_with_only_recent_sagas.
    #[tokio::test]
    async fn does_not_mark_recent_in_flight() {
        let persistence = Arc::new(MemoryPersistence::new());
        let mut recent = ExecutionState::new("recent-1", "order-saga", ExecutionPattern::Saga);
        recent.status = ExecutionStatus::Running;
        persistence.save(recent).await.unwrap();
        let recovery = service(&persistence).stale_threshold(Duration::seconds(600));

        assert_eq!(recovery.recover_stale().await.unwrap(), 0);
        let state = persistence
            .load("recent-1")
            .await
            .unwrap()
            .expect("present");
        assert_eq!(state.status, ExecutionStatus::Running);
    }

    // Port of pyfly test_does_not_mark_already_completed_sagas +
    // test_counts_only_in_flight_stale_sagas.
    #[tokio::test]
    async fn does_not_mark_completed_and_counts_only_in_flight() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("stale-1", "order-saga"))
            .await
            .unwrap();
        let mut done = stale_state("done-1", "order-saga");
        done.transition(ExecutionStatus::Completed);
        done.updated_at = Utc::now() - Duration::hours(2);
        persistence.save(done).await.unwrap();
        let recovery = service(&persistence);

        assert_eq!(recovery.recover_stale().await.unwrap(), 1);
        let state = persistence.load("done-1").await.unwrap().expect("present");
        assert_eq!(state.status, ExecutionStatus::Completed);
    }

    // Port of pyfly test_returns_zero_when_no_stale_sagas.
    #[tokio::test]
    async fn returns_zero_when_nothing_is_stale() {
        let persistence = Arc::new(MemoryPersistence::new());
        let recovery = service(&persistence);
        assert_eq!(recovery.recover_stale().await.unwrap(), 0);
        assert!(recovery.find_stale().await.unwrap().is_empty());
    }

    // Port of pyfly TestCleanup::{test_cleanup_delegates_to_persistence_port,
    // test_cleanup_returns_correct_count,
    // test_cleanup_does_not_remove_recent_completed_sagas,
    // test_cleanup_does_not_remove_in_flight_sagas}.
    #[tokio::test]
    async fn cleanup_removes_only_old_terminal_executions() {
        let persistence = Arc::new(MemoryPersistence::new());
        // Two old completed runs.
        for cid in ["old-done-1", "old-done-2"] {
            let mut done = ExecutionState::new(cid, "order-saga", ExecutionPattern::Saga);
            done.transition(ExecutionStatus::Completed);
            let old = Utc::now() - Duration::hours(48);
            done.updated_at = old;
            done.completed_at = Some(old);
            persistence.save(done).await.unwrap();
        }
        // A recently completed run and an old in-flight run.
        let mut recent = ExecutionState::new("recent-done", "order-saga", ExecutionPattern::Saga);
        recent.transition(ExecutionStatus::Completed);
        persistence.save(recent).await.unwrap();
        let mut in_flight =
            ExecutionState::new("in-flight-1", "order-saga", ExecutionPattern::Saga);
        in_flight.status = ExecutionStatus::Running;
        in_flight.updated_at = Utc::now() - Duration::hours(48);
        persistence.save(in_flight).await.unwrap();

        let recovery = service(&persistence);
        assert_eq!(recovery.cleanup().await.unwrap(), 2);
        assert!(persistence.load("old-done-1").await.unwrap().is_none());
        assert!(persistence.load("recent-done").await.unwrap().is_some());
        assert!(persistence.load("in-flight-1").await.unwrap().is_some());
        assert_eq!(recovery.cleanup().await.unwrap(), 0);
        assert_eq!(
            persistence
                .list(ExecutionFilter::all())
                .await
                .unwrap()
                .len(),
            2
        );
    }

    // Rust-specific spelling of "resume or compensate": the decider routes
    // stuck executions to the registered handlers.
    #[tokio::test]
    async fn decider_routes_to_resume_and_compensate_handlers() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("resume-me", "order-saga"))
            .await
            .unwrap();
        persistence
            .save(stale_state("rollback-me", "order-saga"))
            .await
            .unwrap();
        persistence
            .save(stale_state("skip-me", "order-saga"))
            .await
            .unwrap();

        let resumed = Arc::new(AtomicU32::new(0));
        let compensated = Arc::new(AtomicU32::new(0));
        let recovery = service(&persistence)
            .decider(|state| match state.correlation_id.as_str() {
                "resume-me" => RecoveryAction::Resume,
                "rollback-me" => RecoveryAction::Compensate,
                _ => RecoveryAction::Skip,
            })
            .on_resume({
                let resumed = resumed.clone();
                move |_state| {
                    let resumed = resumed.clone();
                    async move {
                        resumed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            })
            .on_compensate({
                let compensated = compensated.clone();
                move |_state| {
                    let compensated = compensated.clone();
                    async move {
                        compensated.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            });

        assert_eq!(recovery.recover_stale().await.unwrap(), 2);
        assert_eq!(resumed.load(Ordering::SeqCst), 1);
        assert_eq!(compensated.load(Ordering::SeqCst), 1);
        let resumed_state = persistence.load("resume-me").await.unwrap().unwrap();
        assert_eq!(resumed_state.status, ExecutionStatus::Completed);
        let rolled_state = persistence.load("rollback-me").await.unwrap().unwrap();
        assert_eq!(rolled_state.status, ExecutionStatus::Compensated);
        let skipped = persistence.load("skip-me").await.unwrap().unwrap();
        assert_eq!(skipped.status, ExecutionStatus::Running);
    }

    // Regression for Bug 2: two overlapping recovery passes that both observe
    // the same stale execution must not both run the side-effecting resume
    // handler. The atomic claim lets only one pass win, so the handler runs
    // exactly once and only one pass counts the recovery.
    #[tokio::test]
    async fn overlapping_scans_run_resume_handler_once() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("contested", "order-saga"))
            .await
            .unwrap();

        let calls = Arc::new(AtomicU32::new(0));
        // The handler yields and sleeps so the two passes overlap: the second
        // scan reaches its claim before the first finishes, exercising the
        // race the claim/CAS protects against.
        let make = || {
            let calls = calls.clone();
            RecoveryService::new(persistence.clone() as Arc<dyn PersistenceProvider>)
                .stale_threshold(Duration::seconds(60))
                .decider(|_| RecoveryAction::Resume)
                .on_resume(move |_state| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::task::yield_now().await;
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        Ok(())
                    }
                })
        };
        let a = make();
        let b = make();

        let (ra, rb) = tokio::join!(a.recover_stale(), b.recover_stale());
        let total = ra.unwrap() + rb.unwrap();
        assert_eq!(total, 1, "exactly one pass may recover the contested row");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "resume handler must run exactly once across overlapping scans"
        );
        let state = persistence.load("contested").await.unwrap().unwrap();
        assert_eq!(state.status, ExecutionStatus::Completed);
    }

    // Regression for Bug 2: a stale row already recovered (terminal) by a
    // prior pass is not picked up again by a subsequent scan, so the handler
    // is not re-run.
    #[tokio::test]
    async fn second_scan_does_not_rerun_recovered_execution() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("once", "order-saga"))
            .await
            .unwrap();
        let calls = Arc::new(AtomicU32::new(0));
        let recovery = service(&persistence)
            .decider(|_| RecoveryAction::Compensate)
            .on_compensate({
                let calls = calls.clone();
                move |_state| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            });
        assert_eq!(recovery.recover_stale().await.unwrap(), 1);
        // Second scan sees the row as terminal (Compensated) and skips it.
        assert_eq!(recovery.recover_stale().await.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let state = persistence.load("once").await.unwrap().unwrap();
        assert_eq!(state.status, ExecutionStatus::Compensated);
    }

    // Rust-specific: a failing resume handler marks the run failed.
    #[tokio::test]
    async fn failing_resume_marks_failed() {
        let persistence = Arc::new(MemoryPersistence::new());
        persistence
            .save(stale_state("r", "order-saga"))
            .await
            .unwrap();
        let recovery = service(&persistence)
            .decider(|_| RecoveryAction::Resume)
            .on_resume(|_state| async { Err("still broken".into()) });
        assert_eq!(recovery.recover_stale().await.unwrap(), 1);
        let state = persistence.load("r").await.unwrap().unwrap();
        assert_eq!(state.status, ExecutionStatus::Failed);
    }
}
