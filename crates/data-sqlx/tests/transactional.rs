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

//! End-to-end proof that `@Transactional` (the `firefly_transactional` runtime)
//! drives the **sqlx repository**: a write inside a rolled-back transaction is
//! undone, a committed one persists. This is the integration the
//! `#[transactional]` macro relies on — the repository enlists in the ambient
//! transaction via the task-local stack, so the whole unit commits or rolls back
//! atomically over a real SQLite database.

#![cfg(feature = "sqlite")]

mod common;

use std::sync::Arc;

use common::{reactive_repo, User};
use firefly_data::ReactiveCrudRepository;
use firefly_data_sqlx::{Db, SqlxTransactionManager};
use firefly_transactional::{transactional_on, TransactionManager, TxError, TxOptions};

/// A test error that satisfies the `transactional` contract (`From<TxError>`).
#[derive(Debug)]
struct TestError(#[allow(dead_code)] String);
impl From<TxError> for TestError {
    fn from(e: TxError) -> Self {
        TestError(e.to_string())
    }
}

/// Opens a **shared-cache** in-memory SQLite `Db` (a multi-connection pool sees
/// one database, so a write on the transaction's connection is visible to a
/// later read on another connection once committed) and builds a transaction
/// manager bound to *this* database.
///
/// The tests drive the manager with [`transactional_on`] (an **explicit**
/// manager) rather than the process-global registry: the registry is first-wins
/// and process-wide, so several tests each owning their own isolated database
/// must not share one — exactly the multi-datasource / per-test case
/// `transactional_on` exists for.
async fn wired_db(name: &str) -> (Db, Arc<dyn TransactionManager>) {
    let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let pool = sqlx::SqlitePool::connect(&url).await.expect("open sqlite");
    let db = Db::Sqlite(pool);
    common::create_table(&db).await;
    let manager: Arc<dyn TransactionManager> = Arc::new(SqlxTransactionManager::new(db.clone()));
    (db, manager)
}

#[tokio::test]
async fn rollback_undoes_a_repository_write() {
    let (db, manager) = wired_db("tx_rollback").await;
    let repo = reactive_repo(db);

    // A transaction that writes a row, then fails — the framework rolls back.
    let outcome: Result<(), TestError> =
        transactional_on(&manager, TxOptions::default(), || async {
            repo.save(User::new("u1", "ada", 10, true))
                .block()
                .await
                .map_err(|e| TestError(e.to_string()))?;
            // The row is visible *inside* the transaction…
            assert!(
                repo.find_by_id("u1".to_string())
                    .block()
                    .await
                    .map_err(|e| TestError(e.to_string()))?
                    .is_some(),
                "the write is visible within its own transaction"
            );
            Err(TestError("forced rollback".into()))
        })
        .await;

    assert!(outcome.is_err(), "the op returned Err");
    // …but rolled back, so a fresh read never sees it.
    let after = repo
        .find_by_id("u1".to_string())
        .block()
        .await
        .expect("read after rollback");
    assert!(after.is_none(), "rollback must undo the insert");
}

#[tokio::test]
async fn non_transactional_write_is_visible_with_a_manager_registered() {
    // The decisive case for the `lumen-ledger` design note: with a process-global
    // transaction manager *registered*, an ordinary (non-`@Transactional`) write
    // must still commit immediately and be visible to a later read on another
    // pool connection. (If this failed, registering a manager would break every
    // plain repository read.) This is the only test that touches the first-wins
    // process registry, so it never collides with the `transactional_on` tests.
    let (db, manager) = wired_db("tx_plain").await;
    firefly_transactional::register_transaction_manager(manager);
    let repo = reactive_repo(db);

    repo.save(User::new("u3", "cy", 30, true))
        .block()
        .await
        .expect("plain save");
    let read = repo
        .find_by_id("u3".to_string())
        .block()
        .await
        .expect("plain read");
    assert!(
        read.is_some(),
        "a non-transactional write must be visible to a later read"
    );
}

#[tokio::test]
async fn commit_persists_repository_writes() {
    let (db, manager) = wired_db("tx_commit").await;
    let repo = reactive_repo(db);

    let outcome: Result<(), TestError> =
        transactional_on(&manager, TxOptions::default(), || async {
            repo.save(User::new("u2", "bob", 20, true))
                .block()
                .await
                .map_err(|e| TestError(e.to_string()))?;
            Ok(())
        })
        .await;

    assert!(outcome.is_ok(), "the op committed");
    let after = repo
        .find_by_id("u2".to_string())
        .block()
        .await
        .expect("read after commit");
    assert!(after.is_some(), "commit persists the insert");
}
