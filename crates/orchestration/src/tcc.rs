//! TCC engine: Try-Confirm-Cancel two-phase orchestration.
//!
//! # Per-step retry (pyfly parity)
//!
//! A [`TccParticipant`] may declare a [`RetryPolicy`](crate::RetryPolicy)
//! via [`TccParticipant::with_retry`]; the try and confirm phases are then
//! invoked through [`invoke_with_policy`](crate::invoke_with_policy) (max
//! attempts, exponential backoff, jitter, per-attempt timeout) — pyfly's
//! `ParticipantInvoker` / `StepInvoker`.

use crate::step_context::StepContext;
use crate::step_invoker::invoke_with_policy;
use crate::{boxed_action, ActionFn, BoxError, RetryPolicy};
use std::fmt;
use std::future::Future;
use thiserror::Error;

/// A Try-Confirm-Cancel participant. Try reserves the resource. Confirm
/// finalises the reservation; Cancel rolls it back. Confirm and Cancel must
/// be idempotent.
pub struct TccParticipant {
    name: String,
    try_action: ActionFn,
    confirm_action: ActionFn,
    cancel_action: Option<ActionFn>,
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
            try_action: boxed_action(try_action),
            confirm_action: boxed_action(confirm_action),
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
        self.cancel_action = Some(boxed_action(cancel_action));
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
        let ctx = StepContext::new();
        let mut tried: Vec<usize> = Vec::with_capacity(self.participants.len());
        for (i, participant) in self.participants.iter().enumerate() {
            let try_result =
                invoke_with_policy(&participant.name, &participant.retry, &ctx, |_ctx| {
                    (participant.try_action)()
                })
                .await;
            if let Err(invoke_err) = try_result {
                self.cancel_tried(&tried).await;
                return Err(TccError::Try {
                    tcc: self.name.clone(),
                    participant: participant.name.clone(),
                    source: unwrap_invoke_cause(&participant.retry, invoke_err),
                });
            }
            tried.push(i);
        }

        let mut failures: Vec<ConfirmError> = Vec::new();
        for &i in &tried {
            let participant = &self.participants[i];
            let confirm_result =
                invoke_with_policy(&participant.name, &participant.retry, &ctx, |_ctx| {
                    (participant.confirm_action)()
                })
                .await;
            if let Err(invoke_err) = confirm_result {
                failures.push(ConfirmError {
                    participant: participant.name.clone(),
                    source: unwrap_invoke_cause(&participant.retry, invoke_err),
                });
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TccError::Confirm(failures))
        }
    }

    /// Cancels the tried participants in reverse order, ignoring cancel
    /// errors and participants without a cancel action.
    async fn cancel_tried(&self, tried: &[usize]) {
        for &i in tried.iter().rev() {
            if let Some(cancel) = &self.participants[i].cancel_action {
                let _ = cancel().await;
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
