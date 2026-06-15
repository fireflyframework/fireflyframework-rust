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

//! A **two-phase (Try / Confirm / Cancel)** money transfer, declared with
//! [`#[firefly::tcc]`](firefly::tcc) (book chapter "Sagas, Workflows & TCC").
//!
//! Where the [saga](crate::transfer) applies each leg immediately and *undoes*
//! a committed leg on failure, TCC **reserves** first and only **commits** once
//! every participant's reservation succeeded — so a failed reservation is
//! cancelled, never compensated after the fact:
//!
//! ```text
//!   source: try = withdraw (hold)   confirm = (none; the debit is the capture)   cancel = deposit (release)
//!   dest:   try = verify exists     confirm = deposit (capture)                  cancel = (none; nothing held)
//! ```
//!
//! Try runs for both participants; if both succeed the coordinator confirms
//! both, else it cancels the ones already tried (in reverse). A transfer to a
//! non-existent destination therefore *holds then releases* the source — the
//! source ends untouched — while a successful transfer captures on both sides.

use std::sync::Arc;

use firefly::orchestration::TccError;
use serde::{Deserialize, Serialize};

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;
use crate::transfer::{TransferError, TransferRequest};

/// The wire result of a confirmed two-phase transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TccTransferResult {
    /// `"confirmed"` when both participants captured.
    pub status: String,
    /// The source wallet id.
    pub from: String,
    /// The destination wallet id.
    pub to: String,
    /// The amount moved, in minor units (cents).
    pub amount: i64,
}

/// The two-phase transfer coordinator: each participant drives the [`Ledger`].
struct TwoPhaseTransfer {
    ledger: Ledger,
}

#[firefly::tcc(name = "transfer-2pc")]
impl TwoPhaseTransfer {
    /// Source **try**: hold the funds by debiting now (a real `MoneyWithdrawn`).
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .withdraw(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }
    /// Source **confirm**: the debit on try already captured the funds.
    async fn capture_source(&self) -> Result<(), DomainError> {
        Ok(())
    }
    /// Source **cancel**: release the hold by refunding it (a real `MoneyDeposited`).
    async fn release_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }

    /// Destination **try**: pre-authorize by verifying the destination exists;
    /// nothing is committed yet, so there is no cancel.
    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        let events = self.ledger.load_events(&req.to).await?;
        if events.is_empty() {
            return Err(DomainError::NotFound(req.to.clone()));
        }
        Ok(())
    }
    /// Destination **confirm**: capture by crediting the destination.
    async fn capture_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.to, Money::cents(req.amount))
            .await?;
        Ok(())
    }
}

/// Validates and runs a two-phase transfer. On success both sides captured and
/// the result is `status: "confirmed"`; on any reservation failure the tried
/// participants are cancelled (the source hold released) and this returns
/// [`TransferError::Compensated`] with the cause.
pub async fn run_tcc_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TccTransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }
    let tcc = Arc::new(TwoPhaseTransfer {
        ledger: ledger.clone(),
    });
    match tcc.run(req.clone()).await {
        Ok(()) => Ok(TccTransferResult {
            status: "confirmed".into(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
        }),
        Err(err) => Err(TransferError::Compensated(tcc_cause(err))),
    }
}

/// Renders the failing phase's cause for the caller.
fn tcc_cause(err: TccError) -> String {
    match err {
        TccError::Try { source, .. } => source.to_string(),
        TccError::Confirm(errors) => errors
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; "),
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
    async fn confirms_and_moves_funds_on_success() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(1_000)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        let result = run_tcc_transfer(
            &ledger,
            &TransferRequest {
                from: src.id.clone(),
                to: dst.id.clone(),
                amount: 300,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.status, "confirmed");
        assert_eq!(balance(&ledger, &src.id).await, 700);
        assert_eq!(balance(&ledger, &dst.id).await, 300);
    }

    #[tokio::test]
    async fn cancels_and_releases_the_hold_when_destination_is_missing() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(1_000)).await.unwrap();
        let err = run_tcc_transfer(
            &ledger,
            &TransferRequest {
                from: src.id.clone(),
                to: "wlt_missing".into(),
                amount: 400,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransferError::Compensated(_)), "{err:?}");
        // Source try held the funds, then the dest try failed → source cancel
        // released them: the hold + its release net to the original balance.
        assert_eq!(balance(&ledger, &src.id).await, 1_000);
    }

    #[tokio::test]
    async fn aborts_without_holding_when_source_is_short() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(100)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        let err = run_tcc_transfer(
            &ledger,
            &TransferRequest {
                from: src.id.clone(),
                to: dst.id.clone(),
                amount: 500,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransferError::Compensated(_)), "{err:?}");
        assert_eq!(balance(&ledger, &src.id).await, 100);
        assert_eq!(balance(&ledger, &dst.id).await, 0);
    }
}
