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

//! Transfer **compliance** — a declarative [`#[firefly::workflow]`](firefly::workflow)
//! that gates a transfer behind two *independent* checks run in parallel before
//! the money moves (book chapter "Sagas, Workflows & TCC").
//!
//! Unlike the [transfer saga](crate::transfer) (a linear debit→credit chain),
//! compliance is a **DAG**: `balance-check` and `limit-check` have no
//! dependency on each other, so the engine runs them in the same topological
//! layer; `approve` declares `depends_on = ["balance-check", "limit-check"]`
//! and therefore runs only after both complete, consuming their results via
//! `#[from_step(...)]`.
//!
//! ```text
//!   balance-check ─┐
//!                  ├─► approve   (depends_on both)
//!   limit-check  ──┘
//! ```
//!
//! `balance-check` reads the *real* source aggregate from the [`Ledger`]; only
//! the per-transfer ceiling is a new policy input.

use std::sync::Arc;

use firefly::orchestration::WorkflowError;

use crate::domain::Wallet;
use crate::ledger::Ledger;
use crate::transfer::TransferRequest;

/// The per-transfer ceiling, in minor units (cents) — a stand-in for a
/// configurable regulatory limit, enforced by the `limit-check` node.
pub const MAX_TRANSFER_CENTS: i64 = 1_000_000; // 10,000.00

/// Why a transfer failed compliance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplianceError {
    /// The source wallet does not exist, so its balance cannot be checked.
    NotFound(String),
    /// A check failed — the transfer is not allowed (the string says why).
    Rejected(String),
}

impl std::fmt::Display for ComplianceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplianceError::NotFound(id) => write!(f, "source wallet {id} not found"),
            ComplianceError::Rejected(why) => write!(f, "transfer rejected: {why}"),
        }
    }
}

impl std::error::Error for ComplianceError {}

/// The compliance workflow: each node drives the [`Ledger`] or a policy input.
struct ComplianceCheck {
    ledger: Ledger,
    max_cents: i64,
}

#[firefly::workflow(name = "transfer-compliance")]
impl ComplianceCheck {
    /// Does the source wallet hold enough to cover the transfer? Reads the real
    /// source aggregate. Errors if the source does not exist.
    #[workflow_step(id = "balance-check")]
    async fn balance_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        let events = self
            .ledger
            .load_events(&req.from)
            .await
            .map_err(|e| ComplianceError::NotFound(e.to_string()))?;
        if events.is_empty() {
            return Err(ComplianceError::NotFound(req.from.clone()));
        }
        let balance = Wallet::rehydrate(&req.from, &events).view().balance;
        Ok(balance >= req.amount)
    }

    /// Is the amount within the per-transfer ceiling? Independent of the
    /// balance check, so it runs in the same parallel layer.
    #[workflow_step(id = "limit-check")]
    async fn limit_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        Ok(req.amount <= self.max_cents)
    }

    /// The decision node: runs only after both checks (`depends_on`) and
    /// consumes their boolean verdicts via `#[from_step]`.
    #[workflow_step(id = "approve", depends_on = ["balance-check", "limit-check"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] funds_ok: bool,
        #[from_step("limit-check")] within_limit: bool,
    ) -> Result<(), ComplianceError> {
        if !funds_ok {
            return Err(ComplianceError::Rejected("insufficient funds".into()));
        }
        if !within_limit {
            return Err(ComplianceError::Rejected(format!(
                "amount exceeds the {} cent per-transfer ceiling",
                self.max_cents
            )));
        }
        Ok(())
    }
}

/// Runs the compliance workflow for `req`. `Ok(())` means the transfer is
/// approved (both checks passed); `Err` carries the typed reason it was
/// rejected — the parallel-DAG counterpart to the linear [transfer
/// saga](crate::transfer::run_transfer).
pub async fn run_compliance(ledger: &Ledger, req: &TransferRequest) -> Result<(), ComplianceError> {
    let check = Arc::new(ComplianceCheck {
        ledger: ledger.clone(),
        max_cents: MAX_TRANSFER_CENTS,
    });
    match check.run(req.clone()).await {
        Ok(()) => Ok(()),
        Err(failure) => Err(compliance_cause(failure)),
    }
}

/// Recovers a typed [`ComplianceError`] from the failing node's error. The
/// workflow engine surfaces a node failure as a boxed error whose concrete type
/// is erased, so we recover the variant from the (preserved) message — the
/// `balance-check` node's "not found" vs. the `approve` node's rejection.
fn compliance_cause(failure: WorkflowError) -> ComplianceError {
    let detail = match &failure {
        WorkflowError::Node { source, .. } => {
            if let Some(err) = source.downcast_ref::<ComplianceError>() {
                return err.clone();
            }
            source.to_string()
        }
        other => other.to_string(),
    };
    if detail.contains("not found") {
        ComplianceError::NotFound(detail)
    } else {
        ComplianceError::Rejected(detail)
    }
}

#[cfg(test)]
mod tests {
    use firefly::eda::InMemoryBroker;
    use firefly::eventsourcing::MemoryEventStore;

    use super::*;
    use crate::money::Money;

    fn ledger() -> Ledger {
        Ledger::new(
            Arc::new(MemoryEventStore::new()),
            Arc::new(InMemoryBroker::new()),
        )
    }

    #[tokio::test]
    async fn approves_a_funded_in_limit_transfer() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(1_000)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        run_compliance(
            &ledger,
            &TransferRequest {
                from: src.id,
                to: dst.id,
                amount: 300,
            },
        )
        .await
        .expect("approved");
    }

    #[tokio::test]
    async fn rejects_an_overdrawn_transfer() {
        let ledger = ledger();
        let src = ledger.open("alice", Money::cents(100)).await.unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        let err = run_compliance(
            &ledger,
            &TransferRequest {
                from: src.id,
                to: dst.id,
                amount: 500,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ComplianceError::Rejected(_)), "{err:?}");
    }

    #[tokio::test]
    async fn rejects_an_over_ceiling_transfer() {
        let ledger = ledger();
        let src = ledger
            .open("whale", Money::cents(MAX_TRANSFER_CENTS * 5))
            .await
            .unwrap();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        let err = run_compliance(
            &ledger,
            &TransferRequest {
                from: src.id,
                to: dst.id,
                amount: MAX_TRANSFER_CENTS + 1,
            },
        )
        .await
        .unwrap_err();
        // Funded but over the ceiling — rejected by the limit check.
        assert!(err.to_string().contains("ceiling"), "{err}");
    }

    #[tokio::test]
    async fn errors_when_the_source_is_unknown() {
        let ledger = ledger();
        let dst = ledger.open("bob", Money::ZERO).await.unwrap();
        let err = run_compliance(
            &ledger,
            &TransferRequest {
                from: "wlt_nope".into(),
                to: dst.id,
                amount: 100,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ComplianceError::NotFound(_)), "{err:?}");
    }
}
