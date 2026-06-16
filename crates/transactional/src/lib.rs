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

//! # firefly-transactional
//!
//! Spring-`@Transactional`-equivalent **context-bound transaction
//! management** over a database port — the Rust counterpart of the Go
//! `transactional` module (which wraps `database/sql`).
//!
//! [`with_tx`] begins a transaction on the [`Database`] port, hands the
//! closure a [`TxContext`] carrying the live transaction, commits when
//! the closure returns `Ok`, and rolls back otherwise. Repositories
//! that want to participate read the transaction off the context with
//! [`TxContext::tx`] (or use the convenience [`exec`] helper) and fall
//! back to the supplied [`Executor`] when no transaction is active.
//!
//! ## Why not `begin()` directly?
//!
//! [`Database::begin`] returns a [`Transaction`] you must manually
//! commit or roll back in every code path. [`with_tx`] lifts the
//! boilerplate:
//!
//! * Commit on the closure returning `Ok`.
//! * Rollback on any `Err` from the closure (the original error is
//!   returned; a rollback failure is joined into
//!   [`TxError::RollbackFailed`], like Go's `errors.Join`).
//! * Rollback on panic — a `Drop` guard rolls back while the panic
//!   unwinds (the Go port does the same with `recover` + re-panic).
//! * Nested [`with_tx`] calls reuse the outer transaction (Spring's
//!   `Propagation.REQUIRED`).
//!
//! ## Go → Rust mapping
//!
//! Go threads the `*sql.Tx` through `context.Context`; Rust has no
//! ambient context, so the transaction travels in an explicit, `Copy`
//! [`TxContext`] handle instead.
//!
//! | Go (`transactional`)    | Rust (`firefly-transactional`)   |
//! |-------------------------|----------------------------------|
//! | `DBTX` interface        | [`Executor`] trait / [`Conn`]    |
//! | `*sql.DB`               | [`Database`] trait               |
//! | `*sql.Tx`               | [`Transaction`] trait            |
//! | `context.Context` value | [`TxContext`] handle             |
//! | `WithTx(ctx, db, fn)`   | [`with_tx(ctx, db, f)`](with_tx) |
//! | `TxFromContext(ctx)`    | [`TxContext::tx`]                |
//! | `Exec(ctx, db)`         | [`exec(ctx, db)`](exec)          |
//!
//! ## Quick start
//!
//! ```
//! use firefly_transactional::{exec, with_tx, Database, Executor, SqlValue, TxContext, TxError};
//!
//! /// Repository method: joins the ambient transaction when one is active.
//! fn save_order(ctx: &TxContext<'_>, db: &dyn Executor, total: i64) -> Result<(), TxError> {
//!     exec(ctx, db).execute(
//!         "INSERT INTO orders(total) VALUES (?1)",
//!         &[SqlValue::Integer(total)],
//!     )?;
//!     Ok(())
//! }
//!
//! /// Service composition: both writes share one transaction.
//! fn place_order<D: Database>(db: &D) -> Result<(), TxError> {
//!     with_tx(&TxContext::root(), db, |ctx| {
//!         save_order(ctx, db, 42)?;
//!         save_order(ctx, db, 7)
//!     })
//! }
//! # use firefly_transactional::{Row, Transaction};
//! # struct MemDb;
//! # struct MemTx;
//! # impl Executor for MemDb {
//! #     fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<u64, TxError> { Ok(1) }
//! #     fn query(&self, _sql: &str, _params: &[SqlValue]) -> Result<Vec<Row>, TxError> { Ok(Vec::new()) }
//! # }
//! # impl Database for MemDb {
//! #     fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError> { Ok(Box::new(MemTx)) }
//! # }
//! # impl Executor for MemTx {
//! #     fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<u64, TxError> { Ok(1) }
//! #     fn query(&self, _sql: &str, _params: &[SqlValue]) -> Result<Vec<Row>, TxError> { Ok(Vec::new()) }
//! # }
//! # impl Transaction for MemTx {
//! #     fn commit(self: Box<Self>) -> Result<(), TxError> { Ok(()) }
//! #     fn rollback(self: Box<Self>) -> Result<(), TxError> { Ok(()) }
//! # }
//! # place_order(&MemDb).unwrap();
//! ```

use std::error::Error as StdError;
use std::fmt;

mod manager;
pub use manager::{
    register_transaction_manager, transaction_manager, transactional, transactional_on,
    transactional_with, transactional_with_on, BoxedTxOp, Isolation, LocalTransactionManager,
    Propagation, TransactionManager, TxOptions, TxOutcome,
};

pub mod events;
pub use events::{
    publish_event, register_discovered_listeners, register_event_listener, EventDispatcher,
    EventListenerRegistration, TransactionPhase,
};

// Re-exported so `firefly-macros`-generated `#[event_listener]` thunks resolve
// `firefly_transactional::inventory::submit!` without the user crate depending on
// `inventory` directly (mirrors `firefly_container::inventory`).
#[doc(hidden)]
pub use inventory;

/// The released framework version. Calendar-versioned (`YY.M.PATCH`)
/// expressed as valid semver — the Go port's `26.05.01` corresponds to
/// `26.6.22` in the June 2026 release window.
pub const VERSION: &str = "26.6.22";

/// Errors produced by the transaction helper and the database port.
///
/// The `Display` strings mirror the Go module's error wrapping:
/// `begin tx: …`, `commit: …`, and the `errors.Join` of the
/// application error with a failed rollback.
#[derive(Debug, thiserror::Error)]
pub enum TxError {
    /// Opening the transaction failed (Go: `fmt.Errorf("begin tx: %w", err)`).
    #[error("begin tx: {0}")]
    Begin(#[source] Box<TxError>),

    /// Committing the transaction failed (Go: `fmt.Errorf("commit: %w", err)`).
    #[error("commit: {0}")]
    Commit(#[source] Box<TxError>),

    /// The closure failed **and** the subsequent rollback also failed —
    /// both errors are surfaced, mirroring Go's
    /// `errors.Join(err, fmt.Errorf("rollback: %w", rbErr))`.
    /// `source()` returns the original closure error.
    #[error("{source}; rollback: {rollback}")]
    RollbackFailed {
        /// The error returned by the closure (the cause of the rollback).
        source: Box<TxError>,
        /// The error reported by the failed rollback itself.
        rollback: Box<TxError>,
    },

    /// A database-level failure reported by an [`Executor`],
    /// [`Database`], or [`Transaction`] implementation.
    #[error("{0}")]
    Database(String),

    /// An application-level error surfaced from a [`with_tx`] closure.
    /// The boxed source supports `downcast_ref`, the Rust analog of
    /// Go's `errors.Is` identity check.
    #[error("{0}")]
    Application(Box<dyn StdError + Send + Sync + 'static>),
}

impl TxError {
    /// Wraps an application-level error so it can flow through
    /// [`with_tx`] and trigger a rollback. Accepts anything that
    /// converts into a boxed error — including `&str` / `String`.
    pub fn application(err: impl Into<Box<dyn StdError + Send + Sync + 'static>>) -> Self {
        TxError::Application(err.into())
    }

    /// Builds a [`TxError::Database`] from a driver error message.
    /// Port implementations use this for `execute` / `query` / `begin`
    /// / `commit` / `rollback` failures.
    pub fn database(message: impl Into<String>) -> Self {
        TxError::Database(message.into())
    }
}

/// A database value — the parameter and column model shared by every
/// [`Executor`] implementation. Plays the role of `database/sql`'s
/// `any` parameters and `Scan` targets in the Go module.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    /// SQL `NULL`.
    Null,
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit floating-point number.
    Real(f64),
    /// A text string.
    Text(String),
    /// A binary blob.
    Blob(Vec<u8>),
}

/// One result row: column names paired with their [`SqlValue`]s — the
/// materialized analog of Go's `*sql.Rows` cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<SqlValue>,
}

impl Row {
    /// Builds a row from parallel column-name and value vectors.
    pub fn new(columns: Vec<String>, values: Vec<SqlValue>) -> Self {
        Row { columns, values }
    }

    /// The column names, in select order.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The values, in select order.
    pub fn values(&self) -> &[SqlValue] {
        &self.values
    }

    /// Looks a value up by column name (first match wins).
    pub fn get(&self, column: &str) -> Option<&SqlValue> {
        let idx = self.columns.iter().position(|c| c == column)?;
        self.values.get(idx)
    }

    /// Looks a value up by positional index.
    pub fn get_index(&self, index: usize) -> Option<&SqlValue> {
        self.values.get(index)
    }

    /// The number of columns in the row.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// The common subset of a connection and a transaction used by
/// repositories — the port equivalent of the Go module's `DBTX`
/// interface (`ExecContext` / `QueryContext` / `QueryRowContext`).
pub trait Executor: Send + Sync {
    /// Runs a statement that returns no rows (INSERT / UPDATE / DELETE
    /// / DDL) and reports the number of rows affected.
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError>;

    /// Runs a query and materializes every result row.
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError>;

    /// Runs a query expected to return at most one row. `Ok(None)`
    /// replaces Go's `sql.ErrNoRows`. The default implementation
    /// takes the first row of [`Executor::query`].
    fn query_row(&self, sql: &str, params: &[SqlValue]) -> Result<Option<Row>, TxError> {
        Ok(self.query(sql, params)?.into_iter().next())
    }
}

/// An in-flight database transaction — the port equivalent of
/// `*sql.Tx`. Statements run through the inherited [`Executor`]
/// methods; `commit` / `rollback` consume the transaction so a
/// finished transaction cannot be reused.
pub trait Transaction: Executor {
    /// Makes every change performed through this transaction durable.
    fn commit(self: Box<Self>) -> Result<(), TxError>;

    /// Discards every change performed through this transaction.
    fn rollback(self: Box<Self>) -> Result<(), TxError>;
}

/// A connection that can open transactions — the port equivalent of
/// `*sql.DB`. Statements run outside a transaction (auto-commit)
/// through the inherited [`Executor`] methods.
pub trait Database: Executor {
    /// Begins a new transaction. Implementations report failures with
    /// [`TxError::database`]; [`with_tx`] adds the `begin tx:` wrap.
    fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError>;
}

/// The transaction-propagation handle — the Rust analog of the
/// `context.Context` value Go stores the `*sql.Tx` under. It is
/// `Copy`, cheap to pass by reference, and constructed in exactly two
/// places: [`TxContext::root`] outside any transaction, and internally
/// by [`with_tx`] with the live transaction attached.
#[derive(Clone, Copy, Default)]
pub struct TxContext<'a> {
    tx: Option<&'a dyn Transaction>,
}

impl<'a> TxContext<'a> {
    /// A context with no active transaction — the analog of
    /// `context.Background()`.
    pub const fn root() -> Self {
        TxContext { tx: None }
    }

    /// Returns the active transaction, if any — the analog of Go's
    /// `TxFromContext(ctx) (*sql.Tx, bool)`.
    pub fn tx(&self) -> Option<&'a dyn Transaction> {
        self.tx
    }

    /// Whether a transaction is currently in flight on this context.
    pub fn in_transaction(&self) -> bool {
        self.tx.is_some()
    }
}

impl fmt::Debug for TxContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxContext")
            .field("in_transaction", &self.in_transaction())
            .finish()
    }
}

/// The transaction-aware executor handle returned by [`exec`] — the
/// analog of the `DBTX` value Go's `Exec(ctx, db)` returns. It is the
/// active transaction when one is in flight, the plain connection
/// otherwise, and implements [`Executor`] either way.
#[derive(Clone, Copy)]
pub enum Conn<'a> {
    /// Statements run inside the active transaction.
    Tx(&'a dyn Transaction),
    /// No transaction in flight — statements run on the connection
    /// (auto-commit).
    Db(&'a dyn Executor),
}

impl fmt::Debug for Conn<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Conn::Tx(_) => f.write_str("Conn::Tx"),
            Conn::Db(_) => f.write_str("Conn::Db"),
        }
    }
}

impl Executor for Conn<'_> {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
        match *self {
            Conn::Tx(tx) => tx.execute(sql, params),
            Conn::Db(db) => db.execute(sql, params),
        }
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
        match *self {
            Conn::Tx(tx) => tx.query(sql, params),
            Conn::Db(db) => db.query(sql, params),
        }
    }

    fn query_row(&self, sql: &str, params: &[SqlValue]) -> Result<Option<Row>, TxError> {
        match *self {
            Conn::Tx(tx) => tx.query_row(sql, params),
            Conn::Db(db) => db.query_row(sql, params),
        }
    }
}

/// Returns the active executor for `ctx` — the transaction when one is
/// in flight, `db` otherwise. Repositories use it to remain
/// transaction-aware without explicit branching; the direct port of
/// Go's `Exec(ctx, db) DBTX`.
pub fn exec<'a>(ctx: &TxContext<'a>, db: &'a dyn Executor) -> Conn<'a> {
    match ctx.tx() {
        Some(tx) => Conn::Tx(tx),
        None => Conn::Db(db),
    }
}

/// Rolls the transaction back on `Drop` unless it was taken for an
/// explicit commit/rollback — this is what preserves the Go port's
/// rollback-on-panic guarantee (`defer` + `recover` + re-panic): when
/// the closure panics, unwinding drops the guard, the rollback runs
/// (its error intentionally ignored, as in Go's `_ = tx.Rollback()`),
/// and the panic resumes.
struct RollbackGuard<'conn> {
    tx: Option<Box<dyn Transaction + 'conn>>,
}

impl Drop for RollbackGuard<'_> {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.rollback();
        }
    }
}

/// Runs `f` inside a database transaction. Commits on `Ok`, rolls back
/// otherwise. Nested calls reuse the outer transaction — commit and
/// rollback are owned by the outermost caller (Spring's
/// `Propagation.REQUIRED`). If `f` panics, the transaction is rolled
/// back and the panic resumes unwinding.
///
/// Unlike the Go original (whose `fn` returns only `error`), the
/// closure may return a value: `with_tx` yields `Ok(T)` after a
/// successful commit.
pub fn with_tx<T, D, F>(ctx: &TxContext<'_>, db: &D, f: F) -> Result<T, TxError>
where
    D: Database + ?Sized,
    F: FnOnce(&TxContext<'_>) -> Result<T, TxError>,
{
    if ctx.in_transaction() {
        // Nested participation — reuse the outer tx. Commit/rollback is
        // owned by the outer caller.
        return f(ctx);
    }
    let tx = db.begin().map_err(|e| TxError::Begin(Box::new(e)))?;
    let mut guard = RollbackGuard { tx: Some(tx) };
    let inner = TxContext {
        tx: guard.tx.as_deref(),
    };
    let result = f(&inner);
    let tx = guard
        .tx
        .take()
        .expect("transaction is held by the guard until resolution");
    match result {
        Ok(value) => match tx.commit() {
            Ok(()) => Ok(value),
            Err(err) => Err(TxError::Commit(Box::new(err))),
        },
        Err(err) => match tx.rollback() {
            Ok(()) => Err(err),
            Err(rb) => Err(TxError::RollbackFailed {
                source: Box::new(err),
                rollback: Box::new(rb),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Shared instrumentation for the mock port: every interesting
    /// lifecycle event is counted so tests can assert exact behavior.
    #[derive(Default)]
    struct State {
        begins: AtomicUsize,
        commits: AtomicUsize,
        rollbacks: AtomicUsize,
        tx_execs: AtomicUsize,
        db_execs: AtomicUsize,
    }

    #[derive(Default)]
    struct MockDb {
        state: Arc<State>,
        fail_begin: bool,
        fail_commit: bool,
        fail_rollback: bool,
    }

    struct MockTx {
        state: Arc<State>,
        fail_commit: bool,
        fail_rollback: bool,
    }

    impl Executor for MockDb {
        fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<u64, TxError> {
            self.state.db_execs.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }

        fn query(&self, _sql: &str, _params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
            Ok(Vec::new())
        }
    }

    impl Database for MockDb {
        fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError> {
            if self.fail_begin {
                return Err(TxError::database("boom"));
            }
            self.state.begins.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(MockTx {
                state: Arc::clone(&self.state),
                fail_commit: self.fail_commit,
                fail_rollback: self.fail_rollback,
            }))
        }
    }

    impl Executor for MockTx {
        fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<u64, TxError> {
            self.state.tx_execs.fetch_add(1, Ordering::SeqCst);
            Ok(1)
        }

        fn query(&self, _sql: &str, _params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
            Ok(Vec::new())
        }
    }

    impl Transaction for MockTx {
        fn commit(self: Box<Self>) -> Result<(), TxError> {
            if self.fail_commit {
                return Err(TxError::database("boom"));
            }
            self.state.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn rollback(self: Box<Self>) -> Result<(), TxError> {
            if self.fail_rollback {
                return Err(TxError::database("boom"));
            }
            self.state.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn counts(db: &MockDb) -> (usize, usize, usize, usize, usize) {
        (
            db.state.begins.load(Ordering::SeqCst),
            db.state.commits.load(Ordering::SeqCst),
            db.state.rollbacks.load(Ordering::SeqCst),
            db.state.tx_execs.load(Ordering::SeqCst),
            db.state.db_execs.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn exec_prefers_active_tx() {
        let db = MockDb::default();
        with_tx(&TxContext::root(), &db, |ctx| {
            assert!(ctx.in_transaction());
            exec(ctx, &db).execute("INSERT", &[])?;
            Ok(())
        })
        .unwrap();
        let (begins, commits, rollbacks, tx_execs, db_execs) = counts(&db);
        assert_eq!(
            (begins, commits, rollbacks, tx_execs, db_execs),
            (1, 1, 0, 1, 0)
        );
    }

    #[test]
    fn exec_falls_back_to_db_without_tx() {
        let db = MockDb::default();
        let root = TxContext::root();
        assert!(!root.in_transaction());
        assert!(root.tx().is_none());
        exec(&root, &db).execute("INSERT", &[]).unwrap();
        let (_, _, _, tx_execs, db_execs) = counts(&db);
        assert_eq!((tx_execs, db_execs), (0, 1));
    }

    #[test]
    fn with_tx_returns_closure_value() {
        let db = MockDb::default();
        let n = with_tx(&TxContext::root(), &db, |ctx| {
            exec(ctx, &db).execute("INSERT", &[])
        })
        .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn closure_error_rolls_back_and_is_returned() {
        let db = MockDb::default();
        let err = with_tx(&TxContext::root(), &db, |_ctx| -> Result<(), TxError> {
            Err(TxError::application("application error"))
        })
        .unwrap_err();
        assert!(matches!(err, TxError::Application(_)));
        assert_eq!(err.to_string(), "application error");
        let (begins, commits, rollbacks, _, _) = counts(&db);
        assert_eq!((begins, commits, rollbacks), (1, 0, 1));
    }

    #[test]
    fn nested_call_participates_and_outer_owns_commit() {
        let db = MockDb::default();
        with_tx(&TxContext::root(), &db, |ctx| {
            exec(ctx, &db).execute("INSERT", &[])?;
            with_tx(ctx, &db, |inner| {
                exec(inner, &db).execute("INSERT", &[])?;
                Ok(())
            })
        })
        .unwrap();
        let (begins, commits, rollbacks, tx_execs, db_execs) = counts(&db);
        assert_eq!(
            (begins, commits, rollbacks, tx_execs, db_execs),
            (1, 1, 0, 2, 0)
        );
    }

    #[test]
    fn begin_failure_is_wrapped() {
        let db = MockDb {
            fail_begin: true,
            ..MockDb::default()
        };
        let err = with_tx(&TxContext::root(), &db, |_ctx| Ok(())).unwrap_err();
        assert!(matches!(err, TxError::Begin(_)));
        assert_eq!(err.to_string(), "begin tx: boom");
    }

    #[test]
    fn commit_failure_is_wrapped() {
        let db = MockDb {
            fail_commit: true,
            ..MockDb::default()
        };
        let err = with_tx(&TxContext::root(), &db, |_ctx| Ok(())).unwrap_err();
        assert!(matches!(err, TxError::Commit(_)));
        assert_eq!(err.to_string(), "commit: boom");
        let (_, commits, rollbacks, _, _) = counts(&db);
        assert_eq!((commits, rollbacks), (0, 0));
    }

    #[test]
    fn rollback_failure_joins_both_errors() {
        let db = MockDb {
            fail_rollback: true,
            ..MockDb::default()
        };
        let err = with_tx(&TxContext::root(), &db, |_ctx| -> Result<(), TxError> {
            Err(TxError::application("application error"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "application error; rollback: boom");
        match err {
            TxError::RollbackFailed { source, rollback } => {
                assert_eq!(source.to_string(), "application error");
                assert_eq!(rollback.to_string(), "boom");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn panic_in_closure_rolls_back_and_resumes() {
        let db = MockDb::default();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = with_tx(&TxContext::root(), &db, |ctx| -> Result<(), TxError> {
                exec(ctx, &db).execute("INSERT", &[])?;
                panic!("boom");
            });
        }));
        assert!(outcome.is_err(), "the panic must resume after rollback");
        let (begins, commits, rollbacks, _, _) = counts(&db);
        assert_eq!((begins, commits, rollbacks), (1, 0, 1));
    }

    #[test]
    fn application_error_supports_downcast() {
        #[derive(Debug)]
        struct AppError;
        impl fmt::Display for AppError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("application error")
            }
        }
        impl StdError for AppError {}

        let err = TxError::application(AppError);
        match &err {
            TxError::Application(inner) => {
                assert!(inner.downcast_ref::<AppError>().is_some());
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(err.to_string(), "application error");
    }

    #[test]
    fn error_display_matches_go_wrapping() {
        assert_eq!(TxError::database("boom").to_string(), "boom");
        assert_eq!(
            TxError::Begin(Box::new(TxError::database("boom"))).to_string(),
            "begin tx: boom"
        );
        assert_eq!(
            TxError::Commit(Box::new(TxError::database("boom"))).to_string(),
            "commit: boom"
        );
    }

    #[test]
    fn row_lookup_by_name_and_index() {
        let row = Row::new(
            vec!["id".into(), "v".into()],
            vec![SqlValue::Integer(1), SqlValue::Text("x".into())],
        );
        assert_eq!(row.get("id"), Some(&SqlValue::Integer(1)));
        assert_eq!(row.get("v"), Some(&SqlValue::Text("x".into())));
        assert_eq!(row.get("missing"), None);
        assert_eq!(row.get_index(0), Some(&SqlValue::Integer(1)));
        assert_eq!(row.get_index(9), None);
        assert_eq!(row.columns(), ["id".to_string(), "v".to_string()]);
        assert_eq!(row.len(), 2);
        assert!(!row.is_empty());
    }

    #[test]
    fn root_and_default_contexts_have_no_tx() {
        assert!(!TxContext::root().in_transaction());
        assert!(TxContext::root().tx().is_none());
        assert!(!TxContext::default().in_transaction());
        assert_eq!(
            format!("{:?}", TxContext::root()),
            "TxContext { in_transaction: false }"
        );
    }

    #[test]
    fn types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TxError>();
        assert_send_sync::<SqlValue>();
        assert_send_sync::<Row>();
        assert_send_sync::<TxContext<'static>>();
        assert_send_sync::<Conn<'static>>();
    }

    #[test]
    fn version_is_stamped() {
        assert_eq!(VERSION, "26.6.22");
    }
}
