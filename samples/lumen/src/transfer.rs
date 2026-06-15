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

//! The money-transfer **saga** — a distributed transaction with compensation,
//! written **declaratively** with [`#[firefly::saga]`](firefly::saga) (book
//! chapter "Sagas, Workflows & TCC").
//!
//! A transfer is *not* a single atomic command: it debits the source wallet,
//! then credits the destination. If the credit leg fails (or the debit
//! overdraws), the already-applied debit must be rolled back. That is exactly
//! the saga pattern, declared as annotated methods on [`TransferSaga`]:
//!
//! ```text
//!   #[saga_step(id = "debit", compensate = "refund_debit")]  withdraw(amount) from source
//!         └─ on rollback ─► refund_debit: deposit(amount) back to source
//!   #[saga_step(id = "credit", depends_on = ["debit"])]      deposit(amount)  to   destination
//! ```
//!
//! `#[saga]` lowers these methods onto the `firefly-orchestration` `Saga`
//! engine: `depends_on` orders the steps, `compensate` names the rollback
//! method, and each parameter is injected from the saga context (here the
//! request, via `#[input]`). Each leg drives the same [`Ledger`] the CQRS
//! handlers use, so a transfer produces real `MoneyWithdrawn` / `MoneyDeposited`
//! events on both streams — and the compensation produces a real refund
//! `MoneyDeposited` on the source stream, observable on the streaming endpoint.

use std::sync::Arc;

use firefly::orchestration::SagaError;
use serde::{Deserialize, Serialize};

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;

/// `POST /api/v1/transfers` command — move `amount` (cents) from `from` to
/// `to`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferRequest {
    /// The source wallet id (debited).
    pub from: String,
    /// The destination wallet id (credited).
    pub to: String,
    /// The amount to move, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

/// The result of a completed (or compensated) transfer — the wire shape
/// returned by `POST /api/v1/transfers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResult {
    /// `"completed"` when both legs succeeded — the lowercase
    /// [`SagaStatus`](firefly::orchestration::SagaStatus) wire string.
    pub status: String,
    /// The source wallet id.
    pub from: String,
    /// The destination wallet id.
    pub to: String,
    /// The amount moved, in minor units (cents).
    pub amount: i64,
    /// Names of the steps that executed successfully, in order.
    #[serde(rename = "stepsExecuted")]
    pub steps_executed: Vec<String>,
    /// Names of the steps whose compensation ran, in reverse order.
    #[serde(rename = "stepsRolledBack")]
    pub steps_rolled_back: Vec<String>,
}

/// The typed error a transfer surfaces to its caller. A *business* failure
/// that triggered a clean rollback ([`Compensated`](TransferError::Compensated))
/// is distinct from a request that failed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// The request was malformed (same wallet, non-positive amount).
    Invalid(String),
    /// The transfer failed and was rolled back; the inner string is the
    /// failing leg's domain error (e.g. `insufficient funds`).
    Compensated(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Invalid(detail) => f.write_str(detail),
            TransferError::Compensated(detail) => write!(f, "transfer rolled back: {detail}"),
        }
    }
}

impl std::error::Error for TransferError {}

/// The money-transfer saga, declared with [`#[firefly::saga]`](firefly::saga):
/// each leg is an annotated method driving the [`Ledger`]. The macro generates
/// `TransferSaga::run` (used by [`run_transfer`]) and `TransferSaga::saga`.
struct TransferSaga {
    ledger: Ledger,
}

#[firefly::saga(name = "money-transfer")]
impl TransferSaga {
    /// Debit the source wallet (a real `MoneyWithdrawn` event). Rolled back by
    /// [`refund_debit`](Self::refund_debit) when a later leg fails.
    #[saga_step(id = "debit", compensate = "refund_debit")]
    async fn debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .withdraw(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }

    /// Compensation for `debit`: a refund is a normal deposit, so it raises a
    /// real `MoneyDeposited` event on the source stream.
    async fn refund_debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }

    /// Credit the destination (a real `MoneyDeposited` event). The last leg, so
    /// a failure here rolls back only the debit.
    #[saga_step(id = "credit", depends_on = ["debit"])]
    async fn credit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.to, Money::cents(req.amount))
            .await?;
        Ok(())
    }
}

/// Validates and runs a money transfer as a declarative saga, returning the
/// terminal [`TransferResult`].
///
/// On the happy path both legs commit and the result is `status: "completed"`.
/// When the debit overdraws (or any leg errors), the saga compensates — the
/// debit, if it had already applied, is refunded — and this returns
/// [`TransferError::Compensated`] carrying the cause, with the event streams
/// left consistent (source unchanged net of the refund, destination
/// untouched).
pub async fn run_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }

    let saga = Arc::new(TransferSaga {
        ledger: ledger.clone(),
    });
    match saga.run(req.clone()).await {
        Ok(outcome) => Ok(TransferResult {
            status: outcome.status.to_string(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
            steps_executed: outcome.steps_executed,
            steps_rolled_back: outcome.steps_rolled,
        }),
        Err(failure) => {
            // Surface the failing leg's typed domain error (e.g. "insufficient
            // funds"), unwrapped from the saga's generic step error.
            let detail = match failure.error() {
                SagaError::Step { source, .. } => source.to_string(),
                other => other.to_string(),
            };
            Err(TransferError::Compensated(detail))
        }
    }
}

#[cfg(test)]
mod tests {
    use firefly::eda::InMemoryBroker;
    use firefly::eventsourcing::MemoryEventStore;

    use super::*;
    use crate::domain::Wallet;

    fn ledger() -> Ledger {
        Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )
    }

    async fn balance(ledger: &Ledger, id: &str) -> i64 {
        let events = ledger.load_events(id).await.unwrap();
        Wallet::rehydrate(id, &events).view().balance
    }

    #[tokio::test]
    async fn transfer_happy_path_moves_funds() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(1_000)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();

        let result = run_transfer(
            &ledger,
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
        assert_eq!(balance(&ledger, &src.id).await, 700);
        assert_eq!(balance(&ledger, &dst.id).await, 300);
    }

    #[tokio::test]
    async fn transfer_overdraft_compensates_and_leaves_funds_intact() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(100)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();

        let err = run_transfer(
            &ledger,
            &TransferRequest {
                from: src.id.clone(),
                to: dst.id.clone(),
                amount: 500, // more than the source holds
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err, TransferError::Compensated("insufficient funds".into()));
        assert_eq!(balance(&ledger, &src.id).await, 100);
        assert_eq!(balance(&ledger, &dst.id).await, 0);
    }

    #[tokio::test]
    async fn transfer_credit_failure_refunds_the_debit() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(1_000)).await.unwrap();
        // Destination does not exist → the credit leg fails after the debit
        // applied, so compensation must refund the source.
        let err = run_transfer(
            &ledger,
            &TransferRequest {
                from: src.id.clone(),
                to: "wlt_missing".into(),
                amount: 400,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransferError::Compensated(_)));

        // Source net balance restored: open(1000) − withdraw(400) +
        // refund(400) = 1000. The stream shows the debit *and* its refund.
        let src_events = ledger.load_events(&src.id).await.unwrap();
        assert_eq!(
            Wallet::rehydrate(&src.id, &src_events).view().balance,
            1_000
        );
        assert_eq!(src_events.len(), 3); // open + withdraw + refund-deposit
    }

    #[tokio::test]
    async fn transfer_validates_request() {
        let ledger = ledger();
        assert_eq!(
            run_transfer(
                &ledger,
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
                &ledger,
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
            from: "wlt_a".into(),
            to: "wlt_b".into(),
            amount: 300,
            steps_executed: vec!["debit".into(), "credit".into()],
            steps_rolled_back: vec![],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"status":"completed","from":"wlt_a","to":"wlt_b","amount":300,"stepsExecuted":["debit","credit"],"stepsRolledBack":[]}"#
        );
    }
}
