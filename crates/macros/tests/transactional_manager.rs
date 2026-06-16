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

//! `#[transactional(manager = "<expr>")]` — Spring's `@Transactional("txManager")`.
//!
//! Proves the option drives an **explicit** `TransactionManager` (here a spy that
//! records its invocation) via `transactional_on`, instead of the process-global
//! registry — so a service that owns its own manager stays isolated. No global
//! manager is registered in this test; the call still runs transactionally.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;

use async_trait::async_trait;
use firefly::transactional::{BoxedTxOp, TransactionManager, TxError, TxOptions, TxOutcome};

#[derive(Debug)]
struct DemoError(#[allow(dead_code)] String);
impl From<TxError> for DemoError {
    fn from(e: TxError) -> Self {
        DemoError(e.to_string())
    }
}

/// A `TransactionManager` that records how many times it governed a call, then
/// runs the operation and honours its outcome (like `LocalTransactionManager`).
struct SpyManager {
    governed: Arc<AtomicUsize>,
}

#[async_trait]
impl TransactionManager for SpyManager {
    async fn execute<'a>(&self, _opts: TxOptions, op: BoxedTxOp<'a>) -> Result<TxOutcome, TxError> {
        self.governed.fetch_add(1, SeqCst);
        op.await
    }
}

/// A service that owns its transaction manager (the multi-datasource / per-test
/// isolation pattern) and exposes it through an accessor the macro names.
struct LedgerService {
    manager: Arc<dyn TransactionManager>,
    work: Arc<AtomicUsize>,
}

impl LedgerService {
    /// The accessor `#[transactional(manager = "...")]` evaluates per call.
    fn tx_manager(&self) -> Arc<dyn TransactionManager> {
        Arc::clone(&self.manager)
    }

    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn record(&self, amount: u32) -> Result<u32, DemoError> {
        self.work.fetch_add(1, SeqCst);
        Ok(amount * 2)
    }

    #[firefly::transactional(manager = "self.tx_manager()")]
    async fn always_fails(&self) -> Result<u32, DemoError> {
        self.work.fetch_add(1, SeqCst);
        Err(DemoError("boom".into()))
    }
}

#[tokio::test]
async fn manager_option_routes_through_the_explicit_manager() {
    let governed = Arc::new(AtomicUsize::new(0));
    let work = Arc::new(AtomicUsize::new(0));
    let svc = LedgerService {
        manager: Arc::new(SpyManager {
            governed: Arc::clone(&governed),
        }),
        work: Arc::clone(&work),
    };

    let out = svc
        .record(21)
        .await
        .expect("commits through the spy manager");
    assert_eq!(out, 42, "the body's value flows back");
    assert_eq!(work.load(SeqCst), 1, "the body ran once");
    assert_eq!(
        governed.load(SeqCst),
        1,
        "the explicit (spy) manager governed the call, not the global registry"
    );
}

#[tokio::test]
async fn manager_option_surfaces_the_body_error() {
    let governed = Arc::new(AtomicUsize::new(0));
    let work = Arc::new(AtomicUsize::new(0));
    let svc = LedgerService {
        manager: Arc::new(SpyManager {
            governed: Arc::clone(&governed),
        }),
        work: Arc::clone(&work),
    };

    let err = svc.always_fails().await.expect_err("Err rolls back");
    assert!(matches!(err, DemoError(_)));
    assert_eq!(
        governed.load(SeqCst),
        1,
        "the spy manager still governed the call"
    );
}
