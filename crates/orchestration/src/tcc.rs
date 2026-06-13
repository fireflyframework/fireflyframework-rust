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

//! TCC engine: Try-Confirm-Cancel two-phase orchestration.
//!
//! # Per-step retry (pyfly parity)
//!
//! A [`TccParticipant`] may declare a [`RetryPolicy`](crate::RetryPolicy)
//! via [`TccParticipant::with_retry`]; the try and confirm phases are then
//! invoked through [`invoke_with_policy`](crate::invoke_with_policy) (max
//! attempts, exponential backoff, jitter, per-attempt timeout) — pyfly's
//! `ParticipantInvoker` / `StepInvoker`.

use crate::observability::{NoOpOrchestrationEvents, OrchestrationEvents};
use crate::step_context::StepContext;
use crate::step_invoker::invoke_with_policy;
use crate::{boxed_action, ActionFn, BoxError, ExecutionPattern, RetryPolicy, TccPhase};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

/// A context-aware TCC action: receives the run's [`StepContext`] so the try
/// phase can publish its result and confirm / cancel can read it — the engine
/// spelling of pyfly's `@FromTry` argument injection.
type CtxActionFn = Box<
    dyn Fn(StepContext) -> futures::future::BoxFuture<'static, Result<(), BoxError>> + Send + Sync,
>;

/// A participant phase body — either a legacy zero-arg action or a
/// context-aware one (`@FromTry` data passing).
enum Phase {
    Plain(ActionFn),
    WithContext(CtxActionFn),
}

impl Phase {
    fn call(&self, ctx: &StepContext) -> futures::future::BoxFuture<'static, Result<(), BoxError>> {
        match self {
            Phase::Plain(action) => action(),
            Phase::WithContext(action) => action(ctx.clone()),
        }
    }
}

/// A Try-Confirm-Cancel participant. Try reserves the resource. Confirm
/// finalises the reservation; Cancel rolls it back. Confirm and Cancel must
/// be idempotent.
pub struct TccParticipant {
    name: String,
    try_action: Phase,
    confirm_action: Phase,
    cancel_action: Option<Phase>,
    retry: RetryPolicy,
}

impl TccParticipant {
    /// Creates a participant from a name plus async try and confirm
    /// actions.
    pub fn new<TF, TFut, CF, CFut>(
        name: impl Into<String>,
        try_action: TF,
        confirm_action: CF,
    ) -> Self
    where
        TF: Fn() -> TFut + Send + Sync + 'static,
        TFut: Future<Output = Result<(), BoxError>> + Send + 'static,
        CF: Fn() -> CFut + Send + Sync + 'static,
        CFut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            try_action: Phase::Plain(boxed_action(try_action)),
            confirm_action: Phase::Plain(boxed_action(confirm_action)),
            cancel_action: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Creates a participant whose try and confirm bodies receive the run's
    /// [`StepContext`] — the engine spelling of pyfly's `@FromTry` argument
    /// injection. The try body can publish its outcome with
    /// [`StepContext::set_result`](crate::StepContext::set_result) and the
    /// confirm body can read it, so the value the try phase produced flows
    /// into confirm (and, via [`TccParticipant::with_context_cancel`], into
    /// cancel).
    ///
    /// ```
    /// use firefly_orchestration::{StepContext, Tcc, TccParticipant};
    /// use serde_json::json;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let tcc = Tcc::new("reserve").participant(TccParticipant::with_context(
    ///     "stock",
    ///     // try: reserve and record a reservation id.
    ///     |ctx| async move {
    ///         ctx.set_result("stock", json!({"reservation_id": "R-7"}));
    ///         Ok(())
    ///     },
    ///     // confirm: consume the try phase's result (@FromTry).
    ///     |ctx| async move {
    ///         assert_eq!(ctx.result_field("stock", "reservation_id").unwrap(), json!("R-7"));
    ///         Ok(())
    ///     },
    /// ));
    /// tcc.run().await.expect("confirms");
    /// # });
    /// ```
    pub fn with_context<TF, TFut, CF, CFut>(
        name: impl Into<String>,
        try_action: TF,
        confirm_action: CF,
    ) -> Self
    where
        TF: Fn(StepContext) -> TFut + Send + Sync + 'static,
        TFut: Future<Output = Result<(), BoxError>> + Send + 'static,
        CF: Fn(StepContext) -> CFut + Send + Sync + 'static,
        CFut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            try_action: Phase::WithContext(Box::new(move |ctx| Box::pin(try_action(ctx)))),
            confirm_action: Phase::WithContext(Box::new(move |ctx| Box::pin(confirm_action(ctx)))),
            cancel_action: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Attaches an async cancel action that rolls back a successful try.
    /// Participants without one are skipped during rollback.
    pub fn with_cancel<F, Fut>(mut self, cancel_action: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.cancel_action = Some(Phase::Plain(boxed_action(cancel_action)));
        self
    }

    /// Attaches a context-aware cancel action that can read the try phase's
    /// result from the [`StepContext`] — pyfly's `@FromTry` in a cancel
    /// method.
    pub fn with_context_cancel<F, Fut>(mut self, cancel_action: F) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.cancel_action = Some(Phase::WithContext(Box::new(move |ctx| {
            Box::pin(cancel_action(ctx))
        })));
        self
    }

    /// Sets the [`RetryPolicy`](crate::RetryPolicy) applied to this
    /// participant's try and confirm phases — pyfly's per-step retry /
    /// backoff / jitter / timeout enforcement. The default policy runs each
    /// phase once.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The participant name, as reported in error messages.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The retry policy configured for this participant.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }
}

impl fmt::Debug for TccParticipant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TccParticipant")
            .field("name", &self.name)
            .field("has_cancel", &self.cancel_action.is_some())
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

/// A single failed confirm phase, reported inside [`TccError::Confirm`].
#[derive(Debug, Error)]
#[error("confirm {participant:?}: {source}")]
pub struct ConfirmError {
    /// The participant whose confirm action failed.
    pub participant: String,
    /// The error returned by the confirm action.
    #[source]
    pub source: BoxError,
}

/// Renders joined confirm errors the way Go's `errors.Join` does: one
/// message per line.
fn joined(errors: &[ConfirmError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Errors produced by [`Tcc::run`].
#[derive(Debug, Error)]
pub enum TccError {
    /// A participant's try action failed; previously-tried participants
    /// were cancelled (best-effort, reverse order).
    #[error("tcc {tcc:?}: try {participant:?}: {source}")]
    Try {
        /// The TCC name.
        tcc: String,
        /// The participant whose try action failed.
        participant: String,
        /// The error returned by the try action.
        #[source]
        source: BoxError,
    },
    /// One or more confirm actions failed; messages are joined one per
    /// line, mirroring Go's `errors.Join`.
    #[error("{}", joined(.0))]
    Confirm(Vec<ConfirmError>),
}

/// Unwraps a [`StepInvokeError`](crate::StepInvokeError) into the
/// `BoxError` cause attached to a TCC error, preserving the historical
/// message shape for the default single-attempt / no-timeout policy.
fn unwrap_invoke_cause(retry: &RetryPolicy, err: crate::StepInvokeError) -> BoxError {
    if retry.max_attempts.max(1) == 1 && retry.timeout_ms == 0 {
        if let Some(source) = err.into_source() {
            return source;
        }
        return "participant failed".into();
    }
    Box::new(err)
}

/// Orchestrates a two-phase commit across a set of participants.
pub struct Tcc {
    name: String,
    participants: Vec<TccParticipant>,
}

impl Tcc {
    /// Creates an empty TCC orchestration.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            participants: Vec::new(),
        }
    }

    /// Appends a participant. Try and confirm phases run in insertion
    /// order; cancellation runs in reverse.
    pub fn participant(mut self, participant: TccParticipant) -> Self {
        self.participants.push(participant);
        self
    }

    /// The TCC name, as reported in error messages.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured participants, in try order — the definition-listing
    /// accessor used by the validator, registry, and admin surfaces.
    pub fn participants(&self) -> &[TccParticipant] {
        &self.participants
    }

    /// Participant names in try order.
    pub fn participant_names(&self) -> Vec<&str> {
        self.participants.iter().map(|p| p.name.as_str()).collect()
    }

    /// Executes try across every participant. On any try failure, cancel
    /// is invoked on the participants that succeeded their try
    /// (best-effort, reverse order). On success, confirm is invoked on
    /// every participant.
    ///
    /// Each try and confirm phase is invoked under the participant's
    /// [`RetryPolicy`](crate::RetryPolicy) ([`TccParticipant::with_retry`]).
    pub async fn run(&self) -> Result<(), TccError> {
        self.run_inner(&StepContext::new(), &NoOpOrchestrationEvents)
            .await
    }

    /// Executes the TCC threading `ctx` through every participant so confirm
    /// / cancel can read the value the try phase published — pyfly's
    /// `@FromTry` data passing ([`TccParticipant::with_context`]).
    pub async fn run_with_context(&self, ctx: &StepContext) -> Result<(), TccError> {
        self.run_inner(ctx, &NoOpOrchestrationEvents).await
    }

    /// Executes the TCC firing TCC-phase lifecycle hooks on `listener` —
    /// pyfly's `OrchestrationEvents` wiring (`on_start`, `on_phase_started` /
    /// `on_phase_completed` / `on_phase_failed`, per-participant
    /// `on_participant_*`, and `on_completed`). Behaviour is otherwise
    /// identical to [`Tcc::run`].
    pub async fn run_with_listener(
        &self,
        listener: Arc<dyn OrchestrationEvents>,
    ) -> Result<(), TccError> {
        self.run_inner(&StepContext::new(), listener.as_ref()).await
    }

    /// Executes the TCC with an explicit [`StepContext`] and lifecycle
    /// `listener` — the most general TCC run.
    pub async fn run_with_context_and_listener(
        &self,
        ctx: &StepContext,
        listener: &dyn OrchestrationEvents,
    ) -> Result<(), TccError> {
        self.run_inner(ctx, listener).await
    }

    async fn run_inner(
        &self,
        ctx: &StepContext,
        listener: &dyn OrchestrationEvents,
    ) -> Result<(), TccError> {
        let cid = ctx.correlation_id();
        let started_at = Instant::now();
        listener
            .on_start(&self.name, ExecutionPattern::Tcc, &cid)
            .await;

        // ── Try phase ───────────────────────────────────────────────────
        listener
            .on_phase_started(&self.name, &cid, TccPhase::Try)
            .await;
        let try_start = Instant::now();
        let mut tried: Vec<usize> = Vec::with_capacity(self.participants.len());
        for (i, participant) in self.participants.iter().enumerate() {
            listener
                .on_participant_started(&self.name, &cid, TccPhase::Try, &participant.name)
                .await;
            let try_result =
                invoke_with_policy(&participant.name, &participant.retry, ctx, |ctx| {
                    participant.try_action.call(ctx)
                })
                .await;
            if let Err(invoke_err) = try_result {
                let source = unwrap_invoke_cause(&participant.retry, invoke_err);
                let message = source.to_string();
                listener
                    .on_participant_failed(
                        &self.name,
                        &cid,
                        TccPhase::Try,
                        &participant.name,
                        &message,
                    )
                    .await;
                listener
                    .on_phase_failed(&self.name, &cid, TccPhase::Try, &message)
                    .await;
                self.cancel_tried(ctx, &tried, listener, &cid).await;
                listener
                    .on_completed(
                        &self.name,
                        ExecutionPattern::Tcc,
                        &cid,
                        false,
                        started_at.elapsed().as_secs_f64() * 1000.0,
                    )
                    .await;
                return Err(TccError::Try {
                    tcc: self.name.clone(),
                    participant: participant.name.clone(),
                    source,
                });
            }
            listener
                .on_participant_success(&self.name, &cid, TccPhase::Try, &participant.name)
                .await;
            tried.push(i);
        }
        listener
            .on_phase_completed(
                &self.name,
                &cid,
                TccPhase::Try,
                try_start.elapsed().as_secs_f64() * 1000.0,
            )
            .await;

        // ── Confirm phase ────────────────────────────────────────────────
        listener
            .on_phase_started(&self.name, &cid, TccPhase::Confirm)
            .await;
        let confirm_start = Instant::now();
        let mut failures: Vec<ConfirmError> = Vec::new();
        for &i in &tried {
            let participant = &self.participants[i];
            listener
                .on_participant_started(&self.name, &cid, TccPhase::Confirm, &participant.name)
                .await;
            let confirm_result =
                invoke_with_policy(&participant.name, &participant.retry, ctx, |ctx| {
                    participant.confirm_action.call(ctx)
                })
                .await;
            if let Err(invoke_err) = confirm_result {
                let source = unwrap_invoke_cause(&participant.retry, invoke_err);
                listener
                    .on_participant_failed(
                        &self.name,
                        &cid,
                        TccPhase::Confirm,
                        &participant.name,
                        &source.to_string(),
                    )
                    .await;
                failures.push(ConfirmError {
                    participant: participant.name.clone(),
                    source,
                });
            } else {
                listener
                    .on_participant_success(&self.name, &cid, TccPhase::Confirm, &participant.name)
                    .await;
            }
        }
        let confirm_ms = confirm_start.elapsed().as_secs_f64() * 1000.0;
        if failures.is_empty() {
            listener
                .on_phase_completed(&self.name, &cid, TccPhase::Confirm, confirm_ms)
                .await;
            listener
                .on_completed(
                    &self.name,
                    ExecutionPattern::Tcc,
                    &cid,
                    true,
                    started_at.elapsed().as_secs_f64() * 1000.0,
                )
                .await;
            Ok(())
        } else {
            let joined_msg = joined(&failures);
            listener
                .on_phase_failed(&self.name, &cid, TccPhase::Confirm, &joined_msg)
                .await;
            listener
                .on_completed(
                    &self.name,
                    ExecutionPattern::Tcc,
                    &cid,
                    false,
                    started_at.elapsed().as_secs_f64() * 1000.0,
                )
                .await;
            Err(TccError::Confirm(failures))
        }
    }

    /// Cancels the tried participants in reverse order, ignoring cancel
    /// errors and participants without a cancel action.
    async fn cancel_tried(
        &self,
        ctx: &StepContext,
        tried: &[usize],
        listener: &dyn OrchestrationEvents,
        cid: &str,
    ) {
        if !tried.is_empty() {
            listener
                .on_phase_started(&self.name, cid, TccPhase::Cancel)
                .await;
        }
        for &i in tried.iter().rev() {
            let participant = &self.participants[i];
            if let Some(cancel) = &participant.cancel_action {
                listener
                    .on_participant_started(&self.name, cid, TccPhase::Cancel, &participant.name)
                    .await;
                match cancel.call(ctx).await {
                    Ok(()) => {
                        listener
                            .on_participant_success(
                                &self.name,
                                cid,
                                TccPhase::Cancel,
                                &participant.name,
                            )
                            .await;
                    }
                    Err(err) => {
                        listener
                            .on_participant_failed(
                                &self.name,
                                cid,
                                TccPhase::Cancel,
                                &participant.name,
                                &err.to_string(),
                            )
                            .await;
                    }
                }
            }
        }
    }
}

impl fmt::Debug for Tcc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tcc")
            .field("name", &self.name)
            .field("participants", &self.participants)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<String>>>;

    fn participant(name: &str, try_fail: bool, log: &Log) -> TccParticipant {
        let entry = name.to_string();
        let try_log = log.clone();
        let try_entry = entry.clone();
        let confirm_log = log.clone();
        let confirm_entry = entry.clone();
        let cancel_log = log.clone();
        let cancel_entry = entry;
        TccParticipant::new(
            name,
            move || {
                let log = try_log.clone();
                let entry = try_entry.clone();
                async move {
                    log.lock().unwrap().push(format!("try-{entry}"));
                    if try_fail {
                        Err("boom".into())
                    } else {
                        Ok(())
                    }
                }
            },
            move || {
                let log = confirm_log.clone();
                let entry = confirm_entry.clone();
                async move {
                    log.lock().unwrap().push(format!("confirm-{entry}"));
                    Ok(())
                }
            },
        )
        .with_cancel(move || {
            let log = cancel_log.clone();
            let entry = cancel_entry.clone();
            async move {
                log.lock().unwrap().push(format!("cancel-{entry}"));
                Ok(())
            }
        })
    }

    // Port of Go TestTCCSucceedsAndCancels (happy half).
    #[tokio::test]
    async fn tcc_confirms_all_after_successful_tries() {
        let actions: Log = Arc::new(Mutex::new(Vec::new()));
        let tcc = Tcc::new("happy")
            .participant(participant("a", false, &actions))
            .participant(participant("b", false, &actions));
        tcc.run().await.expect("tcc should succeed");
        assert_eq!(
            *actions.lock().unwrap(),
            ["try-a", "try-b", "confirm-a", "confirm-b"]
        );
    }

    // Port of Go TestTCCSucceedsAndCancels (failure half).
    #[tokio::test]
    async fn tcc_cancels_tried_participants_on_try_failure() {
        let actions: Log = Arc::new(Mutex::new(Vec::new()));
        let tcc = Tcc::new("fail")
            .participant(participant("a", false, &actions))
            .participant(participant("b", true, &actions));
        let err = tcc.run().await.expect_err("expected error");
        // Should have tried both, then cancel-a.
        assert_eq!(*actions.lock().unwrap(), ["try-a", "try-b", "cancel-a"]);
        // Error message matches the Go port's `tcc %q: try %q: %w` wrapping.
        assert_eq!(err.to_string(), "tcc \"fail\": try \"b\": boom");
        assert!(matches!(err, TccError::Try { .. }));
    }

    // Rust-specific: cancellation runs in reverse try order.
    #[tokio::test]
    async fn tcc_cancels_in_reverse_order() {
        let actions: Log = Arc::new(Mutex::new(Vec::new()));
        let tcc = Tcc::new("transfer")
            .participant(participant("a", false, &actions))
            .participant(participant("b", false, &actions))
            .participant(participant("c", true, &actions));
        tcc.run().await.expect_err("expected error");
        assert_eq!(
            *actions.lock().unwrap(),
            ["try-a", "try-b", "try-c", "cancel-b", "cancel-a"]
        );
    }

    // Rust-specific: confirm failures are aggregated, one message per line.
    #[tokio::test]
    async fn tcc_joins_confirm_errors() {
        let mk = |name: &str| {
            TccParticipant::new(
                name,
                || async { Ok(()) },
                || async { Err("confirm-boom".into()) },
            )
        };
        let tcc = Tcc::new("t").participant(mk("a")).participant(mk("b"));
        let err = tcc.run().await.expect_err("expected error");
        match &err {
            TccError::Confirm(failures) => {
                assert_eq!(failures.len(), 2);
                assert_eq!(failures[0].participant, "a");
                assert_eq!(failures[1].participant, "b");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(
            err.to_string(),
            "confirm \"a\": confirm-boom\nconfirm \"b\": confirm-boom"
        );
    }

    // Rust-specific: participants without a cancel action are skipped
    // during rollback.
    #[tokio::test]
    async fn tcc_skips_participants_without_cancel() {
        let actions: Log = Arc::new(Mutex::new(Vec::new()));
        let no_cancel = {
            let log = actions.clone();
            TccParticipant::new(
                "a",
                move || {
                    let log = log.clone();
                    async move {
                        log.lock().unwrap().push("try-a".to_string());
                        Ok(())
                    }
                },
                || async { Ok(()) },
            )
        };
        let tcc = Tcc::new("t")
            .participant(no_cancel)
            .participant(participant("b", true, &actions));
        tcc.run().await.expect_err("expected error");
        // "a" has no cancel action: only the tries are recorded.
        assert_eq!(*actions.lock().unwrap(), ["try-a", "try-b"]);
    }

    // Rust-specific: an empty TCC succeeds.
    #[tokio::test]
    async fn tcc_with_no_participants_is_ok() {
        Tcc::new("empty").run().await.expect("empty is ok");
    }

    // ── Per-step retry enforcement (pyfly ParticipantInvoker) ───────────

    // A flaky try succeeds within its retry budget; the TCC confirms.
    #[tokio::test]
    async fn tcc_try_retries_until_success() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let tries = Arc::new(AtomicU32::new(0));
        let t = tries.clone();
        let tcc = Tcc::new("retrying").participant(
            TccParticipant::new(
                "p",
                move || {
                    let t = t.clone();
                    async move {
                        if t.fetch_add(1, Ordering::SeqCst) < 1 {
                            Err("transient".into())
                        } else {
                            Ok(())
                        }
                    }
                },
                || async { Ok(()) },
            )
            .with_retry(RetryPolicy {
                max_attempts: 3,
                backoff_ms: 1,
                ..Default::default()
            }),
        );
        tcc.run().await.expect("confirms after retry");
        assert_eq!(tries.load(Ordering::SeqCst), 2);
    }

    // A try exhausting its retries fails the TCC with a Try error carrying
    // the retry context.
    #[tokio::test]
    async fn tcc_try_retry_exhausted_fails() {
        let tcc = Tcc::new("exhaust").participant(
            TccParticipant::new("p", || async { Err("nope".into()) }, || async { Ok(()) })
                .with_retry(RetryPolicy {
                    max_attempts: 2,
                    backoff_ms: 1,
                    ..Default::default()
                }),
        );
        let err = tcc.run().await.expect_err("must fail");
        assert!(matches!(err, TccError::Try { .. }));
        assert!(err.to_string().contains("2 attempt"));
    }
}
