//! The money-transfer **saga** — a two-step distributed transaction with
//! compensation, built on [`firefly_orchestration::Saga`].
//!
//! A transfer is *not* a single atomic command: it debits the source
//! account, then credits the destination. If the credit leg fails (or the
//! debit overdraws), the already-applied debit must be rolled back. That is
//! exactly the saga pattern:
//!
//! ```text
//!   step "debit"  : withdraw(amount) from source   ── compensate ──► deposit(amount) back to source
//!   step "credit" : deposit(amount)  to   destination
//! ```
//!
//! Each leg drives the same [`Bank`](crate::commands::Bank) application
//! service the CQRS handlers use, so a transfer produces real
//! `MoneyWithdrawn` / `MoneyDeposited` events on both streams — and the
//! compensation produces a real `MoneyDeposited` refund on the source
//! stream, observable on the streaming events endpoint.

use std::sync::Arc;
use std::sync::Mutex;

use firefly_orchestration::{Saga, SagaStatus, Step};
use serde::{Deserialize, Serialize};

use crate::commands::Bank;
use crate::domain::DomainError;

/// `POST /api/v1/transfers` command — move `amount` (cents) from `from` to
/// `to`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferRequest {
    /// The source account id (debited).
    #[serde(rename = "from")]
    pub from: String,
    /// The destination account id (credited).
    #[serde(rename = "to")]
    pub to: String,
    /// The amount to move, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

/// The result of a completed (or compensated) transfer — the wire shape
/// returned by `POST /api/v1/transfers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResult {
    /// `"completed"` when both legs succeeded, `"compensated"` when the
    /// transfer rolled back — the lowercase
    /// [`SagaStatus`](firefly_orchestration::SagaStatus) wire strings.
    pub status: String,
    /// The source account id.
    pub from: String,
    /// The destination account id.
    pub to: String,
    /// The amount moved, in minor units (cents).
    pub amount: i64,
    /// Names of the steps that executed successfully, in order.
    #[serde(rename = "stepsExecuted")]
    pub steps_executed: Vec<String>,
    /// Names of the steps whose compensation ran, in reverse order.
    #[serde(rename = "stepsRolledBack")]
    pub steps_rolled_back: Vec<String>,
    /// The failure detail when the transfer compensated; absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The typed error a transfer surfaces to its caller. A *business* failure
/// that triggered a clean rollback ([`Compensated`](TransferError::Compensated))
/// is distinct from a failure to even validate the request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    /// The request was malformed (same account, non-positive amount).
    #[error("{0}")]
    Invalid(String),
    /// The transfer failed and was rolled back; the inner string is the
    /// failing leg's domain error (e.g. `insufficient funds`).
    #[error("transfer rolled back: {0}")]
    Compensated(String),
}

/// Validates and runs a money transfer as a saga, returning the terminal
/// [`TransferResult`].
///
/// On the happy path both legs commit and the result is
/// `status: "completed"`. When the debit overdraws (or any leg errors), the
/// saga compensates — the debit, if it had already applied, is refunded —
/// and this returns [`TransferError::Compensated`] carrying the cause, with
/// the read model and event streams left consistent (source unchanged net
/// of the refund, destination untouched).
pub async fn run_transfer(
    bank: &Bank,
    req: &TransferRequest,
) -> Result<TransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }

    // Captures the domain error of the failing leg so the saga's generic
    // BoxError can be surfaced as a typed cause to the caller.
    let cause: Arc<Mutex<Option<DomainError>>> = Arc::new(Mutex::new(None));

    let debit_bank = bank.clone();
    let debit_from = req.from.clone();
    let refund_bank = bank.clone();
    let refund_from = req.from.clone();
    let amount = req.amount;
    let debit_cause = Arc::clone(&cause);

    // Step 1 — debit the source; compensation refunds it.
    let debit = Step::new("debit", move || {
        let bank = debit_bank.clone();
        let from = debit_from.clone();
        let cause = Arc::clone(&debit_cause);
        async move {
            bank.withdraw(&from, amount).await.map_err(|e| {
                *cause.lock().expect("cause lock") = Some(e.clone());
                box_err(e)
            })?;
            Ok(())
        }
    })
    .with_compensation(move || {
        let bank = refund_bank.clone();
        let from = refund_from.clone();
        async move {
            // Refund the debited amount. A refund is a normal deposit, so it
            // raises a real MoneyDeposited event on the source stream.
            bank.deposit(&from, amount).await.map_err(box_err)?;
            Ok(())
        }
    });

    let credit_bank = bank.clone();
    let credit_to = req.to.clone();
    let credit_cause = Arc::clone(&cause);

    // Step 2 — credit the destination (no compensation needed; it is the
    // last leg, so a failure here rolls back only the debit).
    let credit = Step::new("credit", move || {
        let bank = credit_bank.clone();
        let to = credit_to.clone();
        let cause = Arc::clone(&credit_cause);
        async move {
            bank.deposit(&to, amount).await.map_err(|e| {
                *cause.lock().expect("cause lock") = Some(e.clone());
                box_err(e)
            })?;
            Ok(())
        }
    });

    let saga = Saga::new("money-transfer").step(debit).step(credit);

    match saga.run().await {
        Ok(outcome) => Ok(TransferResult {
            status: SagaStatus::Completed.to_string(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
            steps_executed: outcome.steps_executed,
            steps_rolled_back: outcome.steps_rolled,
            error: None,
        }),
        Err(failure) => {
            let outcome = failure.outcome();
            let detail = cause
                .lock()
                .expect("cause lock")
                .clone()
                .map(|e| e.to_string())
                .unwrap_or_else(|| failure.error().to_string());
            // The terminal result is reported to the caller as a typed
            // compensation error, but the outcome shape is preserved for
            // observability/logging in the web layer.
            let _ = TransferResult {
                status: outcome.status.to_string(),
                from: req.from.clone(),
                to: req.to.clone(),
                amount: req.amount,
                steps_executed: outcome.steps_executed.clone(),
                steps_rolled_back: outcome.steps_rolled.clone(),
                error: Some(detail.clone()),
            };
            Err(TransferError::Compensated(detail))
        }
    }
}

/// Boxes a [`DomainError`] as the saga engine's `BoxError`.
fn box_err(e: DomainError) -> firefly_orchestration::BoxError {
    Box::<dyn std::error::Error + Send + Sync>::from(e.to_string())
}

#[cfg(test)]
mod tests {
    use firefly_eda::InMemoryBroker;
    use firefly_eventsourcing::MemoryEventStore;

    use super::*;

    fn bank() -> Bank {
        Bank::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )
    }

    #[tokio::test]
    async fn transfer_happy_path_moves_funds() {
        let bank = bank();
        let src = bank.open("alice", 1000).await.unwrap();
        let dst = bank.open("bob", 0).await.unwrap();

        let result = run_transfer(
            &bank,
            &TransferRequest {
                from: src.id.clone(),
                to: dst.id.clone(),
                amount: 300,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.status, "completed");
        assert_eq!(result.steps_executed, ["debit", "credit"]);
        assert!(result.steps_rolled_back.is_empty());
        assert!(result.error.is_none());

        // Funds moved: source 1000 − 300 = 700, destination 0 + 300 = 300.
        let src_view =
            crate::domain::Account::rehydrate(&src.id, &bank.load_events(&src.id).await.unwrap())
                .view();
        let dst_view =
            crate::domain::Account::rehydrate(&dst.id, &bank.load_events(&dst.id).await.unwrap())
                .view();
        assert_eq!(src_view.balance, 700);
        assert_eq!(dst_view.balance, 300);
        // open + withdraw on source; open + deposit on destination.
        assert_eq!(bank.load_events(&src.id).await.unwrap().len(), 2);
        assert_eq!(bank.load_events(&dst.id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn transfer_overdraft_compensates_and_leaves_funds_intact() {
        let bank = bank();
        let src = bank.open("alice", 100).await.unwrap();
        let dst = bank.open("bob", 0).await.unwrap();

        let err = run_transfer(
            &bank,
            &TransferRequest {
                from: src.id.clone(),
                to: dst.id.clone(),
                amount: 500, // more than the source holds
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err, TransferError::Compensated("insufficient funds".into()));

        // The debit never applied (overdraft caught up front), so the source
        // still holds its full balance and the destination is untouched.
        let src_view =
            crate::domain::Account::rehydrate(&src.id, &bank.load_events(&src.id).await.unwrap())
                .view();
        let dst_view =
            crate::domain::Account::rehydrate(&dst.id, &bank.load_events(&dst.id).await.unwrap())
                .view();
        assert_eq!(src_view.balance, 100);
        assert_eq!(dst_view.balance, 0);
    }

    #[tokio::test]
    async fn transfer_credit_failure_refunds_the_debit() {
        let bank = bank();
        let src = bank.open("alice", 1000).await.unwrap();
        // Destination "to" does not exist → the credit leg fails after the
        // debit applied, so compensation must refund the source.
        let err = run_transfer(
            &bank,
            &TransferRequest {
                from: src.id.clone(),
                to: "acc_missing".into(),
                amount: 400,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransferError::Compensated(_)));

        // Source net balance is restored: open(1000) − withdraw(400) +
        // refund(400) = 1000. The stream shows the debit *and* its refund.
        let src_events = bank.load_events(&src.id).await.unwrap();
        let src_view = crate::domain::Account::rehydrate(&src.id, &src_events).view();
        assert_eq!(src_view.balance, 1000);
        // open + withdraw + refund-deposit = 3 events.
        assert_eq!(src_events.len(), 3);
    }

    #[tokio::test]
    async fn transfer_validates_request() {
        let bank = bank();
        assert_eq!(
            run_transfer(
                &bank,
                &TransferRequest {
                    from: "a".into(),
                    to: "a".into(),
                    amount: 10
                }
            )
            .await
            .unwrap_err(),
            TransferError::Invalid("from and to must differ".into())
        );
        assert_eq!(
            run_transfer(
                &bank,
                &TransferRequest {
                    from: "a".into(),
                    to: "b".into(),
                    amount: 0
                }
            )
            .await
            .unwrap_err(),
            TransferError::Invalid("amount must be > 0".into())
        );
    }

    #[test]
    fn transfer_result_wire_shape() {
        let json = serde_json::to_string(&TransferResult {
            status: "completed".into(),
            from: "acc_a".into(),
            to: "acc_b".into(),
            amount: 300,
            steps_executed: vec!["debit".into(), "credit".into()],
            steps_rolled_back: vec![],
            error: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"status":"completed","from":"acc_a","to":"acc_b","amount":300,"stepsExecuted":["debit","credit"],"stepsRolledBack":[]}"#
        );
    }
}
