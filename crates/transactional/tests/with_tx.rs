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

//! Integration tests over a real SQLite database — the port of the Go
//! module's `tx_test.go` (which used `modernc.org/sqlite`), plus
//! Rust-specific coverage for the rollback-on-panic `Drop` guard and
//! the value-returning closure.

use firefly_transactional::{
    exec, with_tx, Database, Executor, Row, SqlValue, Transaction, TxContext, TxError,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// A `Database` port over a single shared `rusqlite::Connection` —
/// the test stand-in for Go's `*sql.DB`.
struct SqliteDatabase {
    conn: Arc<Mutex<Connection>>,
}

/// A hand-rolled `BEGIN` / `COMMIT` / `ROLLBACK` transaction over the
/// shared connection — the test stand-in for Go's `*sql.Tx`.
struct SqliteTransaction {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDatabase {
    fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        SqliteDatabase {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

fn db_err(err: rusqlite::Error) -> TxError {
    TxError::database(err.to_string())
}

fn bind(params: &[SqlValue]) -> impl Iterator<Item = rusqlite::types::Value> + '_ {
    params.iter().map(|p| match p {
        SqlValue::Null => rusqlite::types::Value::Null,
        SqlValue::Integer(i) => rusqlite::types::Value::Integer(*i),
        SqlValue::Real(f) => rusqlite::types::Value::Real(*f),
        SqlValue::Text(s) => rusqlite::types::Value::Text(s.clone()),
        SqlValue::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
    })
}

fn run_execute(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
    conn.execute(sql, rusqlite::params_from_iter(bind(params)))
        .map(|n| n as u64)
        .map_err(db_err)
}

fn run_query(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(bind(params)))
        .map_err(db_err)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(db_err)? {
        let mut values = Vec::with_capacity(columns.len());
        for idx in 0..columns.len() {
            let value: rusqlite::types::Value = row.get(idx).map_err(db_err)?;
            values.push(match value {
                rusqlite::types::Value::Null => SqlValue::Null,
                rusqlite::types::Value::Integer(i) => SqlValue::Integer(i),
                rusqlite::types::Value::Real(f) => SqlValue::Real(f),
                rusqlite::types::Value::Text(s) => SqlValue::Text(s),
                rusqlite::types::Value::Blob(b) => SqlValue::Blob(b),
            });
        }
        out.push(Row::new(columns.clone(), values));
    }
    Ok(out)
}

impl Executor for SqliteDatabase {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        run_execute(&conn, sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        run_query(&conn, sql, params)
    }
}

impl Database for SqliteDatabase {
    fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError> {
        self.conn
            .lock()
            .expect("sqlite mutex poisoned")
            .execute_batch("BEGIN")
            .map_err(db_err)?;
        Ok(Box::new(SqliteTransaction {
            conn: Arc::clone(&self.conn),
        }))
    }
}

impl Executor for SqliteTransaction {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        run_execute(&conn, sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
        let conn = self.conn.lock().expect("sqlite mutex poisoned");
        run_query(&conn, sql, params)
    }
}

impl Transaction for SqliteTransaction {
    fn commit(self: Box<Self>) -> Result<(), TxError> {
        self.conn
            .lock()
            .expect("sqlite mutex poisoned")
            .execute_batch("COMMIT")
            .map_err(db_err)
    }

    fn rollback(self: Box<Self>) -> Result<(), TxError> {
        self.conn
            .lock()
            .expect("sqlite mutex poisoned")
            .execute_batch("ROLLBACK")
            .map_err(db_err)
    }
}

/// `openDB` from the Go test: a fresh database with one table.
fn open_db() -> SqliteDatabase {
    let db = SqliteDatabase::in_memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER)", &[])
        .expect("create table");
    db
}

fn count(db: &SqliteDatabase) -> i64 {
    let row = db
        .query_row("SELECT COUNT(*) FROM t", &[])
        .expect("count query")
        .expect("count row");
    match row.get_index(0) {
        Some(SqlValue::Integer(n)) => *n,
        other => panic!("unexpected count value: {other:?}"),
    }
}

/// Go: `TestWithTxCommits`.
#[test]
fn with_tx_commits() {
    let db = open_db();
    with_tx(&TxContext::root(), &db, |ctx| {
        let conn = exec(ctx, &db);
        conn.execute("INSERT INTO t(v) VALUES(1)", &[])?;
        Ok(())
    })
    .expect("with_tx");
    assert_eq!(count(&db), 1, "count after commit");
}

/// Go: `TestWithTxRollsBack` — the application error is surfaced
/// unchanged (Go asserts `errors.Is(err, custom)`; here we downcast).
#[test]
fn with_tx_rolls_back() {
    #[derive(Debug)]
    struct AppError;
    impl std::fmt::Display for AppError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("application error")
        }
    }
    impl std::error::Error for AppError {}

    let db = open_db();
    let err = with_tx(&TxContext::root(), &db, |ctx| -> Result<(), TxError> {
        let _ = exec(ctx, &db).execute("INSERT INTO t(v) VALUES(2)", &[]);
        Err(TxError::application(AppError))
    })
    .expect_err("with_tx must fail");
    match &err {
        TxError::Application(inner) => {
            assert!(inner.downcast_ref::<AppError>().is_some(), "err: {err}");
            assert_eq!(inner.to_string(), "application error");
        }
        other => panic!("err: {other:?}"),
    }
    assert_eq!(count(&db), 0, "count after rollback");
}

/// Go: `TestNestedParticipates` — the nested call uses the same tx.
#[test]
fn nested_participates() {
    let db = open_db();
    with_tx(&TxContext::root(), &db, |ctx| {
        exec(ctx, &db).execute("INSERT INTO t(v) VALUES(1)", &[])?;
        // Nested call uses the same tx.
        with_tx(ctx, &db, |ctx| {
            exec(ctx, &db).execute("INSERT INTO t(v) VALUES(2)", &[])?;
            Ok(())
        })
    })
    .expect("with_tx");
    assert_eq!(count(&db), 2);
}

/// Go: `TestExecFallback` — no transaction in flight, `exec` returns
/// the plain connection.
#[test]
fn exec_fallback() {
    let db = open_db();
    let root = TxContext::root();
    exec(&root, &db)
        .execute("INSERT INTO t(v) VALUES(99)", &[])
        .expect("insert via fallback connection");
    assert_eq!(count(&db), 1);
}

/// Rust-specific: a nested closure error propagates to the outer call
/// and rolls back everything written inside the outer transaction.
#[test]
fn nested_error_rolls_back_outer() {
    let db = open_db();
    let err = with_tx(&TxContext::root(), &db, |ctx| -> Result<(), TxError> {
        exec(ctx, &db).execute("INSERT INTO t(v) VALUES(1)", &[])?;
        with_tx(ctx, &db, |ctx| {
            exec(ctx, &db).execute("INSERT INTO t(v) VALUES(2)", &[])?;
            Err(TxError::application("nested failure"))
        })
    })
    .expect_err("nested error must surface");
    assert_eq!(err.to_string(), "nested failure");
    assert_eq!(count(&db), 0, "outer tx must roll back both inserts");
}

/// Rust-specific: the `Drop` guard preserves the Go port's
/// rollback-on-panic guarantee — the panic resumes and nothing is
/// committed.
#[test]
fn rollback_on_panic() {
    let db = open_db();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = with_tx(&TxContext::root(), &db, |ctx| -> Result<(), TxError> {
            exec(ctx, &db).execute("INSERT INTO t(v) VALUES(3)", &[])?;
            panic!("boom");
        });
    }));
    assert!(outcome.is_err(), "the panic must resume");
    assert_eq!(count(&db), 0, "count after panic rollback");
}

/// Rust-specific: the closure can return a value through `with_tx`
/// (the Go `fn` returns only `error`).
#[test]
fn with_tx_returns_value() {
    let db = open_db();
    let id = with_tx(&TxContext::root(), &db, |ctx| {
        exec(ctx, &db).execute("INSERT INTO t(v) VALUES(7)", &[])?;
        let row = exec(ctx, &db)
            .query_row("SELECT id, v FROM t WHERE v = ?1", &[SqlValue::Integer(7)])?
            .expect("inserted row visible inside the tx");
        match row.get("id") {
            Some(SqlValue::Integer(id)) => Ok(*id),
            other => Err(TxError::database(format!("unexpected id: {other:?}"))),
        }
    })
    .expect("with_tx");
    assert_eq!(id, 1);
    assert_eq!(count(&db), 1);
}
