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

//! The sqlx [`TransactionManager`] — the adapter that makes Spring-style
//! `@Transactional` real over Postgres / MySQL / SQLite.
//!
//! # How ambient enlistment works
//!
//! [`firefly_transactional::transactional`] (and the `#[transactional]` macro)
//! hand this manager the operation future. The manager opens a sqlx
//! transaction, stores it in a **task-local stack** ([`TX_STACK`]), and runs
//! the operation *inside that scope*. While the scope is active, every
//! repository read/write routes through [`pg_execute`] / [`sqlite_execute`] /
//! … which look at [`TX_STACK`] and run the statement on the **active
//! transaction** instead of a fresh pool connection — so an ordinary
//! `repo.save(a).await?; repo.save(b).await?;` is atomic with no change to the
//! repository code. `Pool::begin()` yields an *owned* `Transaction<'static>`,
//! which is what makes stashing it in a task-local possible in Rust.
//!
//! Propagation, isolation, read-only, timeout, and `SAVEPOINT`-based nesting
//! are all implemented here; the policy types live in `firefly-transactional`.

use std::sync::Arc;

use async_trait::async_trait;
use firefly_transactional::{
    BoxedTxOp, Propagation, TransactionManager, TxError, TxOptions, TxOutcome,
};
use tokio::sync::Mutex;

use crate::db::Db;

tokio::task_local! {
    /// The active transaction stack for the current task. Present only while a
    /// [`SqlxTransactionManager`] scope is running; repositories consult it to
    /// enlist in the ambient transaction.
    static TX_STACK: Arc<Mutex<TxStack>>;
}

/// One owned sqlx transaction, tagged by backend.
enum ActiveTx {
    #[cfg(feature = "postgres")]
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
    #[cfg(feature = "mysql")]
    MySql(sqlx::Transaction<'static, sqlx::MySql>),
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::Transaction<'static, sqlx::Sqlite>),
}

impl ActiveTx {
    /// Runs a side-effecting statement (SAVEPOINT / SET TRANSACTION / …) on
    /// this transaction.
    async fn execute_raw(&mut self, sql: &str) -> Result<(), TxError> {
        match self {
            #[cfg(feature = "postgres")]
            ActiveTx::Postgres(tx) => {
                sqlx::query(sql)
                    .execute(&mut **tx)
                    .await
                    .map_err(to_tx_err)?;
            }
            #[cfg(feature = "mysql")]
            ActiveTx::MySql(tx) => {
                sqlx::query(sql)
                    .execute(&mut **tx)
                    .await
                    .map_err(to_tx_err)?;
            }
            #[cfg(feature = "sqlite")]
            ActiveTx::Sqlite(tx) => {
                sqlx::query(sql)
                    .execute(&mut **tx)
                    .await
                    .map_err(to_tx_err)?;
            }
        }
        Ok(())
    }

    /// Commits this transaction.
    async fn commit(self) -> Result<(), TxError> {
        match self {
            #[cfg(feature = "postgres")]
            ActiveTx::Postgres(tx) => tx.commit().await.map_err(to_tx_err),
            #[cfg(feature = "mysql")]
            ActiveTx::MySql(tx) => tx.commit().await.map_err(to_tx_err),
            #[cfg(feature = "sqlite")]
            ActiveTx::Sqlite(tx) => tx.commit().await.map_err(to_tx_err),
        }
    }

    /// Rolls this transaction back.
    async fn rollback(self) -> Result<(), TxError> {
        match self {
            #[cfg(feature = "postgres")]
            ActiveTx::Postgres(tx) => tx.rollback().await.map_err(to_tx_err),
            #[cfg(feature = "mysql")]
            ActiveTx::MySql(tx) => tx.rollback().await.map_err(to_tx_err),
            #[cfg(feature = "sqlite")]
            ActiveTx::Sqlite(tx) => tx.rollback().await.map_err(to_tx_err),
        }
    }
}

/// A frame on the transaction stack. `Savepoint` / `Joined` are transparent
/// markers (the savepoint name + rollback decision are tracked locally by the
/// nested-run code); they exist only so [`TxStack::active_real_index`] can see
/// past them to the enclosing real transaction.
enum Frame {
    /// A real transaction this frame owns (root `REQUIRED`, or `REQUIRES_NEW`).
    Real { tx: ActiveTx, rollback_only: bool },
    /// A `SAVEPOINT` on the nearest enclosing real transaction (`NESTED`).
    Savepoint,
    /// A join onto the enclosing transaction (`REQUIRED`/`SUPPORTS`/`MANDATORY`
    /// while one is already active); transparent for enlistment.
    Joined,
    /// Suspends transaction participation (`NOT_SUPPORTED`); while on top,
    /// repositories use the pool, not the transaction.
    Suspended,
}

/// The per-task stack of active transaction frames.
struct TxStack {
    frames: Vec<Frame>,
    savepoint_seq: u32,
}

impl TxStack {
    /// The index of the active real-transaction frame, or `None` if the top is
    /// a suspension or there is no transaction.
    fn active_real_index(&self) -> Option<usize> {
        for (i, frame) in self.frames.iter().enumerate().rev() {
            match frame {
                Frame::Suspended => return None,
                Frame::Real { .. } => return Some(i),
                Frame::Savepoint | Frame::Joined => continue,
            }
        }
        None
    }

    /// A mutable handle to the transaction repositories should currently use.
    fn current_tx_mut(&mut self) -> Option<&mut ActiveTx> {
        let idx = self.active_real_index()?;
        match &mut self.frames[idx] {
            Frame::Real { tx, .. } => Some(tx),
            _ => None,
        }
    }

    /// Marks the active real transaction (and any join above it) rollback-only,
    /// so a nested failure forces the whole transaction to roll back (Spring's
    /// rollback-only marking).
    fn mark_rollback_only(&mut self) {
        if let Some(idx) = self.active_real_index() {
            if let Frame::Real { rollback_only, .. } = &mut self.frames[idx] {
                *rollback_only = true;
            }
        }
    }
}

/// Reads `TX_STACK` for the current task, cloning the `Arc` handle.
fn current_stack() -> Option<Arc<Mutex<TxStack>>> {
    TX_STACK.try_with(Arc::clone).ok()
}

fn to_tx_err(e: sqlx::Error) -> TxError {
    TxError::database(e.to_string())
}

/// Opens a new sqlx transaction on `db`, applying `opts` (isolation /
/// read-only / timeout) as the first statements where the backend supports it.
async fn begin_active_tx(db: &Db, opts: &TxOptions) -> Result<ActiveTx, TxError> {
    match db {
        #[cfg(feature = "postgres")]
        Db::Postgres(pool) => {
            let tx = pool.begin().await.map_err(to_tx_err)?;
            let mut active = ActiveTx::Postgres(tx);
            apply_pg_like_opts(&mut active, opts, true).await?;
            Ok(active)
        }
        #[cfg(feature = "mysql")]
        Db::MySql(pool) => {
            let tx = pool.begin().await.map_err(to_tx_err)?;
            let mut active = ActiveTx::MySql(tx);
            apply_pg_like_opts(&mut active, opts, false).await?;
            Ok(active)
        }
        #[cfg(feature = "sqlite")]
        Db::Sqlite(pool) => {
            // SQLite has no per-transaction isolation / read-only knobs; the
            // transaction itself provides the atomicity guarantee.
            let tx = pool.begin().await.map_err(to_tx_err)?;
            Ok(ActiveTx::Sqlite(tx))
        }
    }
}

/// Applies isolation / read-only / timeout to a freshly-opened PG/MySQL
/// transaction (issued as the transaction's first statements). `is_pg` selects
/// Postgres-only `SET LOCAL statement_timeout`.
async fn apply_pg_like_opts(
    active: &mut ActiveTx,
    opts: &TxOptions,
    is_pg: bool,
) -> Result<(), TxError> {
    if let Some(level) = opts.isolation.sql_level() {
        active
            .execute_raw(&format!("SET TRANSACTION ISOLATION LEVEL {level}"))
            .await?;
    }
    if opts.read_only {
        active.execute_raw("SET TRANSACTION READ ONLY").await?;
    }
    if is_pg {
        if let Some(timeout) = opts.timeout {
            let ms = timeout.as_millis().max(1);
            active
                .execute_raw(&format!("SET LOCAL statement_timeout = {ms}"))
                .await?;
        }
    }
    Ok(())
}

/// The sqlx-backed [`TransactionManager`]. Register one at startup with
/// [`firefly_transactional::register_transaction_manager`] (a data starter /
/// auto-configuration does this) and every `#[transactional]` async fn becomes
/// transactional over this datasource.
#[derive(Clone)]
pub struct SqlxTransactionManager {
    db: Db,
}

impl SqlxTransactionManager {
    /// Builds a transaction manager over the given [`Db`].
    pub fn new(db: Db) -> Self {
        SqlxTransactionManager { db }
    }
}

#[async_trait]
impl TransactionManager for SqlxTransactionManager {
    async fn execute<'a>(&self, opts: TxOptions, op: BoxedTxOp<'a>) -> Result<TxOutcome, TxError> {
        match current_stack() {
            None => self.run_as_root(opts, op).await,
            Some(stack) => run_as_nested(stack, self.db.clone(), opts, op).await,
        }
    }

    fn is_active(&self) -> bool {
        current_stack()
            .map(|s| {
                s.try_lock()
                    .map(|g| g.active_real_index().is_some())
                    .unwrap_or(true)
            })
            .unwrap_or(false)
    }
}

impl SqlxTransactionManager {
    /// Runs the operation as the outermost transactional scope.
    async fn run_as_root<'a>(
        &self,
        opts: TxOptions,
        op: BoxedTxOp<'a>,
    ) -> Result<TxOutcome, TxError> {
        match opts.propagation {
            Propagation::Mandatory => Err(TxError::application(
                "Propagation.MANDATORY requires an existing transaction, but none is active",
            )),
            // No transaction needed: run the operation directly.
            Propagation::Never | Propagation::NotSupported | Propagation::Supports => op.await,
            // Open a transaction and scope the operation within it.
            Propagation::Required | Propagation::RequiresNew | Propagation::Nested => {
                let tx = begin_active_tx(&self.db, &opts).await?;
                let stack = Arc::new(Mutex::new(TxStack {
                    frames: vec![Frame::Real {
                        tx,
                        rollback_only: false,
                    }],
                    savepoint_seq: 0,
                }));
                let op_result = TX_STACK.scope(Arc::clone(&stack), op).await;

                // The task-local clone is dropped now; reclaim the root frame.
                let (active_tx, rollback_only) = {
                    let mut g = stack.lock().await;
                    match g.frames.pop() {
                        Some(Frame::Real { tx, rollback_only }) => (tx, rollback_only),
                        _ => return Err(TxError::database("transaction stack corrupted")),
                    }
                };

                match op_result {
                    Ok(outcome) => {
                        if outcome.rolled_back || rollback_only {
                            active_tx.rollback().await?;
                        } else {
                            active_tx.commit().await?;
                        }
                        Ok(outcome)
                    }
                    Err(infra) => {
                        let _ = active_tx.rollback().await;
                        Err(infra)
                    }
                }
            }
        }
    }
}

/// Runs the operation nested within an already-active transaction stack.
async fn run_as_nested<'a>(
    stack: Arc<Mutex<TxStack>>,
    db: Db,
    opts: TxOptions,
    op: BoxedTxOp<'a>,
) -> Result<TxOutcome, TxError> {
    match opts.propagation {
        Propagation::Never => Err(TxError::application(
            "Propagation.NEVER, but a transaction is active",
        )),

        // Join the current transaction; propagate a nested failure as
        // rollback-only on the enclosing transaction.
        Propagation::Required | Propagation::Supports | Propagation::Mandatory => {
            stack.lock().await.frames.push(Frame::Joined);
            let op_result = op.await;
            let mut g = stack.lock().await;
            g.frames.pop(); // remove the Joined frame
            match &op_result {
                Ok(outcome) if outcome.rolled_back => g.mark_rollback_only(),
                Ok(_) => {}
                Err(_) => g.mark_rollback_only(),
            }
            op_result
        }

        // Independent transaction on a separate connection.
        Propagation::RequiresNew => {
            let tx = begin_active_tx(&db, &opts).await?;
            stack.lock().await.frames.push(Frame::Real {
                tx,
                rollback_only: false,
            });
            let op_result = op.await;
            let (active_tx, rollback_only) = {
                let mut g = stack.lock().await;
                match g.frames.pop() {
                    Some(Frame::Real { tx, rollback_only }) => (tx, rollback_only),
                    _ => return Err(TxError::database("transaction stack corrupted")),
                }
            };
            match op_result {
                Ok(outcome) => {
                    if outcome.rolled_back || rollback_only {
                        active_tx.rollback().await?;
                    } else {
                        active_tx.commit().await?;
                    }
                    Ok(outcome)
                }
                Err(infra) => {
                    let _ = active_tx.rollback().await;
                    Err(infra)
                }
            }
        }

        // SAVEPOINT on the enclosing transaction.
        Propagation::Nested => {
            let name = {
                let mut g = stack.lock().await;
                g.savepoint_seq += 1;
                let name = format!("firefly_sp_{}", g.savepoint_seq);
                if let Some(tx) = g.current_tx_mut() {
                    tx.execute_raw(&format!("SAVEPOINT {name}")).await?;
                } else {
                    return Err(TxError::application(
                        "Propagation.NESTED requires an enclosing transaction",
                    ));
                }
                g.frames.push(Frame::Savepoint);
                name
            };
            let op_result = op.await;
            let mut g = stack.lock().await;
            g.frames.pop(); // remove the Savepoint frame
            let roll = match &op_result {
                Ok(outcome) => outcome.rolled_back,
                Err(_) => true,
            };
            if let Some(tx) = g.current_tx_mut() {
                if roll {
                    tx.execute_raw(&format!("ROLLBACK TO SAVEPOINT {name}"))
                        .await?;
                } else {
                    tx.execute_raw(&format!("RELEASE SAVEPOINT {name}")).await?;
                }
            }
            op_result
        }

        // Suspend transaction participation for the operation.
        Propagation::NotSupported => {
            stack.lock().await.frames.push(Frame::Suspended);
            let op_result = op.await;
            stack.lock().await.frames.pop();
            op_result
        }
    }
}

// ── Enlisting executors: pool-or-transaction, per backend ───────────────────
//
// Each helper runs a prepared `sqlx::query(...)` against the ambient
// transaction when one is active for its backend, else against the pool. The
// repository calls these instead of `query.execute(pool)` directly.

#[cfg(feature = "postgres")]
type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

#[cfg(feature = "postgres")]
pub(crate) async fn pg_execute<'q>(
    pool: &sqlx::PgPool,
    query: PgQuery<'q>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Postgres(tx)) = g.current_tx_mut() {
            return query.execute(&mut **tx).await;
        }
    }
    query.execute(pool).await
}

#[cfg(feature = "postgres")]
pub(crate) async fn pg_fetch_optional<'q>(
    pool: &sqlx::PgPool,
    query: PgQuery<'q>,
) -> Result<Option<sqlx::postgres::PgRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Postgres(tx)) = g.current_tx_mut() {
            return query.fetch_optional(&mut **tx).await;
        }
    }
    query.fetch_optional(pool).await
}

#[cfg(feature = "postgres")]
pub(crate) async fn pg_fetch_one<'q>(
    pool: &sqlx::PgPool,
    query: PgQuery<'q>,
) -> Result<sqlx::postgres::PgRow, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Postgres(tx)) = g.current_tx_mut() {
            return query.fetch_one(&mut **tx).await;
        }
    }
    query.fetch_one(pool).await
}

#[cfg(feature = "postgres")]
pub(crate) async fn pg_fetch_all<'q>(
    pool: &sqlx::PgPool,
    query: PgQuery<'q>,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Postgres(tx)) = g.current_tx_mut() {
            return query.fetch_all(&mut **tx).await;
        }
    }
    query.fetch_all(pool).await
}

#[cfg(feature = "mysql")]
type MyQuery<'q> = sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>;

#[cfg(feature = "mysql")]
pub(crate) async fn mysql_execute<'q>(
    pool: &sqlx::MySqlPool,
    query: MyQuery<'q>,
) -> Result<sqlx::mysql::MySqlQueryResult, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::MySql(tx)) = g.current_tx_mut() {
            return query.execute(&mut **tx).await;
        }
    }
    query.execute(pool).await
}

#[cfg(feature = "mysql")]
pub(crate) async fn mysql_fetch_optional<'q>(
    pool: &sqlx::MySqlPool,
    query: MyQuery<'q>,
) -> Result<Option<sqlx::mysql::MySqlRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::MySql(tx)) = g.current_tx_mut() {
            return query.fetch_optional(&mut **tx).await;
        }
    }
    query.fetch_optional(pool).await
}

#[cfg(feature = "mysql")]
pub(crate) async fn mysql_fetch_one<'q>(
    pool: &sqlx::MySqlPool,
    query: MyQuery<'q>,
) -> Result<sqlx::mysql::MySqlRow, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::MySql(tx)) = g.current_tx_mut() {
            return query.fetch_one(&mut **tx).await;
        }
    }
    query.fetch_one(pool).await
}

#[cfg(feature = "mysql")]
pub(crate) async fn mysql_fetch_all<'q>(
    pool: &sqlx::MySqlPool,
    query: MyQuery<'q>,
) -> Result<Vec<sqlx::mysql::MySqlRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::MySql(tx)) = g.current_tx_mut() {
            return query.fetch_all(&mut **tx).await;
        }
    }
    query.fetch_all(pool).await
}

#[cfg(feature = "sqlite")]
type SqliteQuery<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;

#[cfg(feature = "sqlite")]
pub(crate) async fn sqlite_execute<'q>(
    pool: &sqlx::SqlitePool,
    query: SqliteQuery<'q>,
) -> Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Sqlite(tx)) = g.current_tx_mut() {
            return query.execute(&mut **tx).await;
        }
    }
    query.execute(pool).await
}

#[cfg(feature = "sqlite")]
pub(crate) async fn sqlite_fetch_optional<'q>(
    pool: &sqlx::SqlitePool,
    query: SqliteQuery<'q>,
) -> Result<Option<sqlx::sqlite::SqliteRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Sqlite(tx)) = g.current_tx_mut() {
            return query.fetch_optional(&mut **tx).await;
        }
    }
    query.fetch_optional(pool).await
}

#[cfg(feature = "sqlite")]
pub(crate) async fn sqlite_fetch_one<'q>(
    pool: &sqlx::SqlitePool,
    query: SqliteQuery<'q>,
) -> Result<sqlx::sqlite::SqliteRow, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Sqlite(tx)) = g.current_tx_mut() {
            return query.fetch_one(&mut **tx).await;
        }
    }
    query.fetch_one(pool).await
}

#[cfg(feature = "sqlite")]
pub(crate) async fn sqlite_fetch_all<'q>(
    pool: &sqlx::SqlitePool,
    query: SqliteQuery<'q>,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, sqlx::Error> {
    if let Some(stack) = current_stack() {
        let mut g = stack.lock().await;
        if let Some(ActiveTx::Sqlite(tx)) = g.current_tx_mut() {
            return query.fetch_all(&mut **tx).await;
        }
    }
    query.fetch_all(pool).await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use firefly_transactional::{transactional_on, transactional_with_on, Propagation, TxOptions};
    use sqlx::Row;

    /// A **shared-cache** in-memory SQLite database, unique per test. Shared
    /// cache lets several pool connections (needed for `REQUIRES_NEW`, which
    /// runs on a separate connection) observe the same database, while a kept
    /// idle connection keeps the in-memory DB alive for the test's duration.
    async fn setup() -> Db {
        use std::str::FromStr;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::Duration;
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let url = format!("sqlite:file:firefly_tx_test_{n}?mode=memory&cache=shared");
        // A busy_timeout so any write-lock contention errors quickly instead of
        // hanging (SQLite serializes writers — see the REQUIRES_NEW test).
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
            .expect("sqlite connect options")
            .busy_timeout(Duration::from_secs(5));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(5)
            .connect_with(opts)
            .await
            .expect("open shared-cache in-memory sqlite");
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create table");
        Db::Sqlite(pool)
    }

    fn manager(db: &Db) -> Arc<dyn TransactionManager> {
        Arc::new(SqlxTransactionManager::new(db.clone()))
    }

    async fn insert(db: &Db, id: i64, name: &str) -> Result<(), TxError> {
        let Db::Sqlite(pool) = db else { unreachable!() };
        let q = sqlx::query("INSERT INTO t (id, name) VALUES (?, ?)")
            .bind(id)
            .bind(name);
        sqlite_execute(pool, q).await.map_err(to_tx_err)?;
        Ok(())
    }

    async fn count(db: &Db) -> i64 {
        let Db::Sqlite(pool) = db else { unreachable!() };
        let q = sqlx::query("SELECT COUNT(*) FROM t");
        let row = sqlite_fetch_one(pool, q).await.expect("count");
        row.get::<i64, _>(0)
    }

    #[tokio::test]
    async fn commits_all_or_nothing_on_ok() {
        let db = setup().await;
        let mgr = manager(&db);
        let out: Result<(), TxError> = transactional_on(&mgr, TxOptions::default(), || async {
            insert(&db, 1, "a").await?;
            insert(&db, 2, "b").await?;
            Ok(())
        })
        .await;
        assert!(out.is_ok());
        assert_eq!(count(&db).await, 2, "both inserts committed atomically");
    }

    #[tokio::test]
    async fn rolls_back_all_on_err() {
        let db = setup().await;
        let mgr = manager(&db);
        let out: Result<(), TxError> = transactional_on(&mgr, TxOptions::default(), || async {
            insert(&db, 1, "a").await?;
            // Read-your-writes inside the transaction.
            assert_eq!(
                count(&db).await,
                1,
                "uncommitted write visible within the tx"
            );
            Err(TxError::application("boom"))
        })
        .await;
        assert!(out.is_err());
        assert_eq!(count(&db).await, 0, "the failed transaction rolled back");
    }

    #[tokio::test]
    async fn requires_new_commits_independently_of_a_failing_outer() {
        let db = setup().await;
        let mgr = manager(&db);
        // An inner REQUIRES_NEW commits independently of a later outer failure.
        // SQLite serializes writers, so the inner must commit (and release the
        // write lock) BEFORE the outer writes — hence the inner runs first.
        let out: Result<(), TxError> = transactional_on(&mgr, TxOptions::default(), || async {
            let inner: Result<(), TxError> =
                transactional_on(&mgr, TxOptions::requires_new(), || async {
                    insert(&db, 2, "inner").await?;
                    Ok(())
                })
                .await;
            assert!(inner.is_ok());
            // Now the outer takes the write lock, then fails and rolls back.
            insert(&db, 1, "outer").await?;
            Err(TxError::application("outer fails"))
        })
        .await;
        assert!(out.is_err());
        // The outer row rolled back; the independent inner row survives.
        assert_eq!(
            count(&db).await,
            1,
            "REQUIRES_NEW committed; outer rolled back"
        );
    }

    #[tokio::test]
    async fn nested_savepoint_rolls_back_without_killing_outer() {
        let db = setup().await;
        let mgr = manager(&db);
        let out: Result<(), TxError> = transactional_on(&mgr, TxOptions::default(), || async {
            insert(&db, 1, "kept").await?;
            // A NESTED block fails and rolls back to its savepoint; we swallow
            // the error so the outer transaction still commits its own row.
            let nested: Result<(), TxError> =
                transactional_on(&mgr, TxOptions::nested(), || async {
                    insert(&db, 2, "savepoint").await?;
                    Err(TxError::application("nested fails"))
                })
                .await;
            assert!(nested.is_err());
            Ok(())
        })
        .await;
        assert!(out.is_ok());
        assert_eq!(
            count(&db).await,
            1,
            "savepoint rolled back, outer committed"
        );
    }

    #[tokio::test]
    async fn mandatory_without_active_tx_errors() {
        let db = setup().await;
        let mgr = manager(&db);
        let out: Result<(), TxError> = transactional_on(
            &mgr,
            TxOptions::default().with_propagation(Propagation::Mandatory),
            || async { Ok(()) },
        )
        .await;
        assert!(out.is_err(), "MANDATORY requires an existing transaction");
    }

    #[tokio::test]
    async fn rollback_rule_can_commit_on_err() {
        let db = setup().await;
        let mgr = manager(&db);
        // A no-rollback rule: commit even though the operation returns Err.
        let out: Result<(), TxError> = transactional_with_on(
            &mgr,
            TxOptions::default(),
            |_e| false, // never roll back
            || async {
                insert(&db, 1, "kept-despite-err").await?;
                Err(TxError::application("non-rollback error"))
            },
        )
        .await;
        assert!(out.is_err());
        assert_eq!(count(&db).await, 1, "no-rollback rule committed the row");
    }
}
