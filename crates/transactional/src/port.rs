//! The synchronous database port [`with_tx`](crate::with_tx) drives.
//!
//! Where Go leaned on `database/sql` (`*sql.DB`, `*sql.Tx`, and the `DBTX`
//! common-subset interface), Rust has no database handle in std, so this
//! module defines the minimal port instead: [`Value`] / [`Row`] for
//! parameters and results, [`Executor`] as the `DBTX` analog, and the
//! [`Database`] / [`Transaction`] pair that [`with_tx`](crate::with_tx)
//! orchestrates. Implementations adapt a concrete driver (the integration
//! tests ship a rusqlite adapter, playing the role `modernc.org/sqlite`
//! played in the Go tests).

use crate::error::TxError;

/// A single SQL parameter or result-column value — the typed analog of the
/// `args ...any` Go forwarded to the driver.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A 64-bit signed integer (`INTEGER` column; booleans bind as 0/1).
    Integer(i64),
    /// A 64-bit float (`REAL` column).
    Real(f64),
    /// UTF-8 text (`TEXT` column).
    Text(String),
    /// Raw bytes (`BLOB` column).
    Blob(Vec<u8>),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Integer(i64::from(v))
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Real(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Integer(i64::from(v))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Blob(v.to_vec())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}

/// One materialized result row — the eager analog of Go's `*sql.Row`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    /// Builds a row from its column values, left to right.
    pub fn new(values: Vec<Value>) -> Self {
        Row { values }
    }

    /// The value of column `index` (0-based), or `None` past the last column.
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// All column values, left to right.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// The number of columns.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// True when the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl From<Vec<Value>> for Row {
    fn from(values: Vec<Value>) -> Self {
        Row::new(values)
    }
}

/// The common subset of [`Database`] and [`Transaction`] used by
/// repositories — the Go `DBTX` interface.
///
/// Repositories accept `&dyn Executor` (or call [`exec`](crate::exec)) so the
/// same code runs against the bare connection and against an in-flight
/// transaction without branching.
pub trait Executor {
    /// Executes a statement and returns the number of affected rows
    /// (Go `ExecContext`). `params` bind to the driver's positional
    /// placeholders in order.
    fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, TxError>;

    /// Runs a query and returns every row, eagerly materialized
    /// (Go `QueryContext`).
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, TxError>;

    /// Runs a query and returns the first row, if any (Go `QueryRowContext`).
    /// The default implementation delegates to [`Executor::query`].
    fn query_row(&self, sql: &str, params: &[Value]) -> Result<Option<Row>, TxError> {
        Ok(self.query(sql, params)?.into_iter().next())
    }
}

/// An open database transaction — the `*sql.Tx` analog.
///
/// `commit` and `rollback` consume the transaction, so the type system rules
/// out the double-finish bugs Go could only catch at runtime.
pub trait Transaction: Executor {
    /// Commits the transaction.
    fn commit(self) -> Result<(), TxError>;

    /// Rolls the transaction back.
    fn rollback(self) -> Result<(), TxError>;
}

/// A database handle that can open transactions — the `*sql.DB` analog.
pub trait Database: Executor {
    /// The concrete transaction type handed out by [`Database::begin`],
    /// borrowing the connection for `'conn`.
    type Tx<'conn>: Transaction
    where
        Self: 'conn;

    /// Opens a new transaction (Go `db.BeginTx`). Implementations should
    /// surface driver failures as [`TxError::Database`];
    /// [`with_tx`](crate::with_tx) adds the `begin tx:` context.
    fn begin(&self) -> Result<Self::Tx<'_>, TxError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_from_integers() {
        assert_eq!(Value::from(7i64), Value::Integer(7));
        assert_eq!(Value::from(7i32), Value::Integer(7));
    }

    #[test]
    fn value_from_real() {
        assert_eq!(Value::from(1.5f64), Value::Real(1.5));
    }

    #[test]
    fn value_from_bool_binds_as_0_or_1() {
        assert_eq!(Value::from(true), Value::Integer(1));
        assert_eq!(Value::from(false), Value::Integer(0));
    }

    #[test]
    fn value_from_text() {
        assert_eq!(Value::from("hi"), Value::Text("hi".into()));
        assert_eq!(Value::from(String::from("hi")), Value::Text("hi".into()));
    }

    #[test]
    fn value_from_blob() {
        assert_eq!(Value::from(vec![1u8, 2]), Value::Blob(vec![1, 2]));
        assert_eq!(Value::from(&[1u8, 2][..]), Value::Blob(vec![1, 2]));
    }

    #[test]
    fn value_from_option() {
        assert_eq!(Value::from(Some(7i64)), Value::Integer(7));
        assert_eq!(Value::from(Option::<i64>::None), Value::Null);
    }

    #[test]
    fn row_accessors() {
        let row = Row::new(vec![Value::Integer(1), Value::Text("a".into())]);
        assert_eq!(row.len(), 2);
        assert!(!row.is_empty());
        assert_eq!(row.get(0), Some(&Value::Integer(1)));
        assert_eq!(row.get(1), Some(&Value::Text("a".into())));
        assert_eq!(row.get(2), None);
        assert_eq!(row.values(), &[Value::Integer(1), Value::Text("a".into())]);
        assert!(Row::default().is_empty());
    }

    #[test]
    fn query_row_default_takes_first_row() {
        struct Stub;
        impl Executor for Stub {
            fn execute(&self, _sql: &str, _params: &[Value]) -> Result<u64, TxError> {
                Ok(0)
            }
            fn query(&self, sql: &str, _params: &[Value]) -> Result<Vec<Row>, TxError> {
                if sql == "empty" {
                    Ok(Vec::new())
                } else {
                    Ok(vec![
                        Row::new(vec![Value::Integer(1)]),
                        Row::new(vec![Value::Integer(2)]),
                    ])
                }
            }
        }
        let first = Stub.query_row("two", &[]).unwrap();
        assert_eq!(first, Some(Row::new(vec![Value::Integer(1)])));
        assert_eq!(Stub.query_row("empty", &[]).unwrap(), None);
    }
}
