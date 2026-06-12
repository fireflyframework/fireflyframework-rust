//! The synchronous database port the migration runner drives.

/// A single SQL parameter or result-column value.
///
/// The migration runner only ever traffics in 64-bit integers and UTF-8
/// text, so the value enum is deliberately tiny.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlValue {
    /// A 64-bit signed integer (`INTEGER` column).
    Int(i64),
    /// UTF-8 text (`TEXT` column; `TIMESTAMP` is bound as RFC 3339 text).
    Text(String),
}

/// Error raised by a [`Database`] implementation.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DatabaseError(
    /// Driver-reported failure message.
    pub String,
);

/// Minimal synchronous database access the migration runner needs — the
/// Rust analog of the `*sql.DB` handle the Go module accepted.
///
/// Implementations adapt a concrete driver connection (rusqlite,
/// postgres, …). All SQL issued through this port is parameter-free and
/// ANSI-compatible, except the single history-row insert, which binds
/// five values to `?` positional placeholders.
pub trait Database {
    /// Execute a statement. `params` bind to `?` placeholders in order;
    /// the slice is empty for DDL and for migration bodies (which may
    /// contain multiple statements).
    fn execute(&mut self, sql: &str, params: &[SqlValue]) -> Result<(), DatabaseError>;

    /// Run a query and return every row, each column converted to a
    /// [`SqlValue`].
    fn query(&mut self, sql: &str) -> Result<Vec<Vec<SqlValue>>, DatabaseError>;

    /// Open a transaction on this connection.
    fn begin(&mut self) -> Result<(), DatabaseError>;

    /// Commit the open transaction.
    fn commit(&mut self) -> Result<(), DatabaseError>;

    /// Roll back the open transaction.
    fn rollback(&mut self) -> Result<(), DatabaseError>;
}
