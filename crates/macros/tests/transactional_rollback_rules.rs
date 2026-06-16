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

//! `#[transactional(no_rollback_for = …, rollback_only_for = …)]` — the
//! transaction rollback rules (`no_rollback_for` is Spring's `noRollbackFor`;
//! `rollback_only_for` is a Rust-native restrictive rule, not Spring's additive
//! `rollbackFor`).
//!
//! Spring names exception *types*; the Rust analog is an error *pattern*. These
//! tests drive each method through a spy `TransactionManager` that records, per
//! call, the **rollback decision** the generated `should_rollback` predicate
//! made — the `TxOutcome::rolled_back` flag, i.e. whether the returned `Err`
//! commits or rolls back — proving:
//! - default: every `Err` rolls back;
//! - `no_rollback_for`: a matching `Err` commits instead;
//! - `rollback_only_for`: only a matching `Err` rolls back;
//! - both: `no_rollback_for` wins on overlap;
//! - the rules apply on both the explicit-manager and process-global paths.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

use async_trait::async_trait;
use firefly::transactional::{BoxedTxOp, TransactionManager, TxError, TxOptions, TxOutcome};

/// The service's error type — the `rollback_only_for` / `no_rollback_for`
/// patterns name its variants, the way `@Transactional(noRollbackFor = …)` names
/// exception classes.
#[derive(Debug)]
enum SvcError {
    /// A domain "not found" — a caller may want it to *commit* a side effect.
    NotFound,
    /// A validation failure carrying a message.
    Validation(#[allow(dead_code)] String),
    /// An infrastructure failure that must always roll back.
    Backend(#[allow(dead_code)] String),
    /// Transaction-infrastructure failure (begin/commit), via `From<TxError>`.
    Tx(#[allow(dead_code)] String),
}

impl From<TxError> for SvcError {
    fn from(e: TxError) -> Self {
        SvcError::Tx(e.to_string())
    }
}

/// Records each governed call's *decision* — commit vs rollback — by reading the
/// `TxOutcome::rolled_back` flag the generated predicate produced.
#[derive(Default)]
struct DecisionSpy {
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
}

struct SpyManager {
    spy: Arc<DecisionSpy>,
}

#[async_trait]
impl TransactionManager for SpyManager {
    async fn execute<'a>(&self, _opts: TxOptions, op: BoxedTxOp<'a>) -> Result<TxOutcome, TxError> {
        let outcome = op.await?;
        if outcome.rolled_back {
            self.spy.rollbacks.fetch_add(1, SeqCst);
        } else {
            self.spy.commits.fetch_add(1, SeqCst);
        }
        Ok(outcome)
    }
}

/// A service owning its manager (so the tests stay off the process-global
/// registry and run in parallel) — the rollback rules ride on the same
/// `manager = "…"` path, exercising `transactional_with_on`.
struct Ledger {
    manager: Arc<dyn TransactionManager>,
}

impl Ledger {
    fn tx_manager(&self) -> Arc<dyn TransactionManager> {
        Arc::clone(&self.manager)
    }

    /// Default rule: every `Err` rolls back.
    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn default_rule(&self, err: Option<SvcError>) -> Result<u32, SvcError> {
        match err {
            Some(e) => Err(e),
            None => Ok(1),
        }
    }

    /// `no_rollback_for`: a `NotFound` commits; everything else still rolls back.
    #[firefly::transactional(manager = "self.tx_manager()", no_rollback_for = "SvcError::NotFound")]
    async fn lenient_on_not_found(&self, err: Option<SvcError>) -> Result<u32, SvcError> {
        match err {
            Some(e) => Err(e),
            None => Ok(1),
        }
    }

    /// `rollback_only_for`: only a `Backend` rolls back; other errors commit.
    #[firefly::transactional(
        manager = "self.tx_manager()",
        rollback_only_for = "SvcError::Backend(_)"
    )]
    async fn rollback_only_backend(&self, err: Option<SvcError>) -> Result<u32, SvcError> {
        match err {
            Some(e) => Err(e),
            None => Ok(1),
        }
    }

    /// Both rules with overlap on `Validation`: `no_rollback_for` wins, so a
    /// `Validation` commits even though it is in the `rollback_only_for` set.
    #[firefly::transactional(
        manager = "self.tx_manager()",
        rollback_only_for = "SvcError::Backend(_) | SvcError::Validation(_)",
        no_rollback_for = "SvcError::Validation(_)"
    )]
    async fn both_rules(&self, err: Option<SvcError>) -> Result<u32, SvcError> {
        match err {
            Some(e) => Err(e),
            None => Ok(1),
        }
    }
}

/// No `manager` attribute — routes through the **process-global** registry
/// (`transactional_with`), so the rollback rule is exercised on that path too,
/// not only the explicit-manager `transactional_with_on` path the methods above
/// use.
#[firefly::transactional(rollback_only_for = "SvcError::Backend(_)")]
async fn global_path(err: Option<SvcError>) -> Result<u32, SvcError> {
    match err {
        Some(e) => Err(e),
        None => Ok(1),
    }
}

fn ledger() -> (Ledger, Arc<DecisionSpy>) {
    let spy = Arc::new(DecisionSpy::default());
    let ledger = Ledger {
        manager: Arc::new(SpyManager {
            spy: Arc::clone(&spy),
        }),
    };
    (ledger, spy)
}

#[tokio::test]
async fn default_rule_rolls_back_on_any_error() {
    let (svc, spy) = ledger();
    svc.default_rule(None).await.expect("Ok commits");
    assert_eq!(spy.commits.load(SeqCst), 1);

    let _ = svc
        .default_rule(Some(SvcError::NotFound))
        .await
        .unwrap_err();
    let _ = svc
        .default_rule(Some(SvcError::Backend("io".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.rollbacks.load(SeqCst), 2, "every Err rolls back");
    assert_eq!(spy.commits.load(SeqCst), 1, "only the Ok committed");
}

#[tokio::test]
async fn no_rollback_for_commits_the_matching_error() {
    let (svc, spy) = ledger();

    // NotFound matches no_rollback_for -> commits despite being an Err.
    let err = svc
        .lenient_on_not_found(Some(SvcError::NotFound))
        .await
        .expect_err("the Err is still returned to the caller");
    assert!(matches!(err, SvcError::NotFound));
    assert_eq!(spy.commits.load(SeqCst), 1, "NotFound committed");
    assert_eq!(spy.rollbacks.load(SeqCst), 0);

    // A different error still rolls back.
    let _ = svc
        .lenient_on_not_found(Some(SvcError::Backend("io".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.rollbacks.load(SeqCst), 1, "Backend still rolls back");
    assert_eq!(spy.commits.load(SeqCst), 1);
}

#[tokio::test]
async fn rollback_only_for_restricts_rollback_to_the_matching_error() {
    let (svc, spy) = ledger();

    // Backend matches rollback_only_for -> rolls back.
    let _ = svc
        .rollback_only_backend(Some(SvcError::Backend("io".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.rollbacks.load(SeqCst), 1);

    // Validation is NOT in rollback_only_for -> commits despite being an Err.
    let _ = svc
        .rollback_only_backend(Some(SvcError::Validation("bad".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.commits.load(SeqCst), 1, "non-Backend error commits");
    assert_eq!(spy.rollbacks.load(SeqCst), 1);
}

#[tokio::test]
async fn no_rollback_for_wins_over_rollback_for_on_overlap() {
    let (svc, spy) = ledger();

    // Validation is in BOTH sets; no_rollback_for wins -> commits.
    let _ = svc
        .both_rules(Some(SvcError::Validation("bad".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.commits.load(SeqCst), 1, "overlap resolves to commit");
    assert_eq!(spy.rollbacks.load(SeqCst), 0);

    // Backend is only in rollback_only_for -> rolls back.
    let _ = svc
        .both_rules(Some(SvcError::Backend("io".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.rollbacks.load(SeqCst), 1);

    // NotFound is in neither set -> not in rollback_only_for -> commits.
    let _ = svc.both_rules(Some(SvcError::NotFound)).await.unwrap_err();
    assert_eq!(
        spy.commits.load(SeqCst),
        2,
        "error outside both sets commits"
    );
    assert_eq!(spy.rollbacks.load(SeqCst), 1);
}

#[tokio::test]
async fn rollback_rules_apply_on_the_process_global_path() {
    // Register a process-global spy manager (first-wins). This is the only test
    // that touches the global registry; the explicit-manager methods above route
    // through `transactional_on` and never consult it, so there is no contention.
    let spy = Arc::new(DecisionSpy::default());
    let registered = firefly::transactional::register_transaction_manager(Arc::new(SpyManager {
        spy: Arc::clone(&spy),
    }));
    assert!(
        registered,
        "no global manager was registered before this test"
    );

    // Backend matches rollback_only_for -> rolls back, on the global path.
    let _ = global_path(Some(SvcError::Backend("io".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.rollbacks.load(SeqCst), 1);

    // Validation is not matched -> commits, proving the predicate rides the
    // process-global `transactional_with` entry point as well.
    let _ = global_path(Some(SvcError::Validation("bad".into())))
        .await
        .unwrap_err();
    assert_eq!(spy.commits.load(SeqCst), 1);
}
