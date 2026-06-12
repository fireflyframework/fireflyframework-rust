//! The transaction error family.
//!
//! The Go module returns plain `error` values wrapped with
//! `fmt.Errorf("begin tx: %w", …)` / `"commit: %w"` / `"rollback: %w"` and
//! joins a failed rollback with the application error via `errors.Join`.
//! [`TxError`] reproduces every one of those shapes as a typed variant so the
//! display strings — and the source chains — match the Go port.

use std::error::Error;

/// Boxed error payload carried inside [`TxError`] variants — the Rust analog
/// of Go's untyped `error` interface value.
pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Error returned by [`with_tx`](crate::with_tx) and the database port traits.
///
/// Driver implementations ([`Database`](crate::Database) /
/// [`Transaction`](crate::Transaction) / [`Executor`](crate::Executor)) should
/// surface raw driver failures as [`TxError::Database`];
/// [`with_tx`](crate::with_tx) adds the `begin tx:` / `commit:` / `rollback:`
/// context exactly where the Go `WithTx` wrapped with `fmt.Errorf`.
#[derive(Debug, thiserror::Error)]
pub enum TxError {
    /// Opening the transaction failed
    /// (Go: `fmt.Errorf("begin tx: %w", err)`).
    #[error("begin tx: {0}")]
    Begin(#[source] BoxError),

    /// Committing the transaction failed
    /// (Go: `fmt.Errorf("commit: %w", err)`).
    #[error("commit: {0}")]
    Commit(#[source] BoxError),

    /// Rolling the transaction back failed
    /// (Go: `fmt.Errorf("rollback: %w", err)`).
    #[error("rollback: {0}")]
    Rollback(#[source] BoxError),

    /// A driver-level failure from [`Executor`](crate::Executor) methods or
    /// from a port implementation — Go returned these unwrapped, so this
    /// variant is transparent.
    #[error(transparent)]
    Database(BoxError),

    /// An application error returned by the [`with_tx`](crate::with_tx)
    /// closure. It passes through [`with_tx`](crate::with_tx) unchanged
    /// (after the rollback), exactly as Go returned `err` verbatim, so
    /// callers can recover the original error with
    /// [`TxError::application_ref`].
    #[error(transparent)]
    Application(BoxError),

    /// The closure failed *and* the rollback failed. Both errors are kept,
    /// mirroring Go's `errors.Join(err, fmt.Errorf("rollback: %w", rbErr))`;
    /// the display joins them with `"; "` instead of Go's newline.
    #[error("{source}; {rollback}")]
    RollbackFailed {
        /// The original error returned by the closure.
        source: Box<TxError>,
        /// The rollback failure (a [`TxError::Rollback`]).
        rollback: Box<TxError>,
    },
}

impl TxError {
    /// Wraps a raw driver failure — what port implementations return.
    pub fn database(err: impl Into<BoxError>) -> Self {
        TxError::Database(err.into())
    }

    /// Wraps an application error so the [`with_tx`](crate::with_tx) closure
    /// can return it through the transaction boundary.
    pub fn application(err: impl Into<BoxError>) -> Self {
        TxError::Application(err.into())
    }

    /// True when this error originated from the application closure (either
    /// directly or joined with a rollback failure).
    pub fn is_application(&self) -> bool {
        match self {
            TxError::Application(_) => true,
            TxError::RollbackFailed { source, .. } => source.is_application(),
            _ => false,
        }
    }

    /// Recovers the typed application error, traversing a
    /// [`TxError::RollbackFailed`] join — the Rust analog of Go's
    /// `errors.Is` / `errors.As` walking an `errors.Join`.
    pub fn application_ref<E: Error + 'static>(&self) -> Option<&E> {
        match self {
            TxError::Application(inner) => inner.downcast_ref::<E>(),
            TxError::RollbackFailed { source, .. } => source.application_ref::<E>(),
            _ => None,
        }
    }
}
