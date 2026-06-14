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

//! The **async, declarative** transaction layer — the Rust rendering of
//! Spring's `@Transactional` + `PlatformTransactionManager`.
//!
//! This module is **driver-agnostic**: it defines the policy types
//! ([`Propagation`], [`Isolation`], [`TxOptions`]), the object-safe
//! [`TransactionManager`] port that a concrete adapter (e.g.
//! `firefly-data-sqlx`) implements, a process-wide registry, and the generic
//! [`transactional`] orchestrator the `#[transactional]` macro expands to.
//!
//! The orchestrator hands the manager a *boxed operation future*; the manager
//! establishes its ambient context (a task-local transaction stack), applies
//! the requested [`Propagation`], runs the operation, and commits on `Ok` /
//! rolls back on `Err`. The user's `Result<R, E>` rides through type-erased so
//! one object-safe `execute` method serves every result type — and the same
//! [`TransactionManager`] abstraction backs relational, document, and saga-step
//! transactions (the audit's "build the boundary once" rule).

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;

use crate::TxError;

/// Transaction propagation — what happens when a transactional call runs while
/// another transaction may already be active. Mirrors Spring's
/// `Propagation` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Propagation {
    /// Join the current transaction, or start a new one if none exists.
    #[default]
    Required,
    /// Always start a new, independent transaction, suspending any current one.
    RequiresNew,
    /// Run within a nested transaction (a `SAVEPOINT`) if one is active, else
    /// behave like [`Required`](Propagation::Required).
    Nested,
    /// Join the current transaction if one exists, else run non-transactionally.
    Supports,
    /// Run non-transactionally, suspending any current transaction.
    NotSupported,
    /// Join the current transaction; error if none exists.
    Mandatory,
    /// Run non-transactionally; error if a transaction is active.
    Never,
}

/// Transaction isolation level. [`Default`](Isolation::Default) uses the
/// connection/driver default. Mirrors Spring's `Isolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Isolation {
    /// The datasource's default isolation level.
    #[default]
    Default,
    /// `READ UNCOMMITTED`.
    ReadUncommitted,
    /// `READ COMMITTED`.
    ReadCommitted,
    /// `REPEATABLE READ`.
    RepeatableRead,
    /// `SERIALIZABLE`.
    Serializable,
}

impl Isolation {
    /// The `SET TRANSACTION ISOLATION LEVEL` clause for this level, or `None`
    /// for [`Default`](Isolation::Default).
    pub fn sql_level(self) -> Option<&'static str> {
        match self {
            Isolation::Default => None,
            Isolation::ReadUncommitted => Some("READ UNCOMMITTED"),
            Isolation::ReadCommitted => Some("READ COMMITTED"),
            Isolation::RepeatableRead => Some("REPEATABLE READ"),
            Isolation::Serializable => Some("SERIALIZABLE"),
        }
    }
}

/// The declarative transaction attributes — Spring's `@Transactional(...)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxOptions {
    /// Propagation behaviour (default [`Propagation::Required`]).
    pub propagation: Propagation,
    /// Isolation level (default [`Isolation::Default`]).
    pub isolation: Isolation,
    /// Whether the transaction is read-only (a hint to the driver + the
    /// `firefly-data` read/write router).
    pub read_only: bool,
    /// Optional statement/transaction timeout.
    pub timeout: Option<Duration>,
}

impl Default for TxOptions {
    fn default() -> Self {
        TxOptions {
            propagation: Propagation::Required,
            isolation: Isolation::Default,
            read_only: false,
            timeout: None,
        }
    }
}

impl TxOptions {
    /// `Propagation.REQUIRED` (the default).
    pub fn required() -> Self {
        Self::default()
    }

    /// `Propagation.REQUIRES_NEW`.
    pub fn requires_new() -> Self {
        Self {
            propagation: Propagation::RequiresNew,
            ..Self::default()
        }
    }

    /// `Propagation.NESTED`.
    pub fn nested() -> Self {
        Self {
            propagation: Propagation::Nested,
            ..Self::default()
        }
    }

    /// Sets the propagation behaviour.
    pub fn with_propagation(mut self, propagation: Propagation) -> Self {
        self.propagation = propagation;
        self
    }

    /// Sets the isolation level.
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }

    /// Marks the transaction read-only.
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Sets a transaction timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

/// The type-erased outcome of a transactional operation, handed back to the
/// orchestrator by [`TransactionManager::execute`].
pub struct TxOutcome {
    /// The user's `Result<R, E>`, boxed; the orchestrator downcasts it back.
    pub value: Box<dyn Any + Send>,
    /// `true` when the operation returned `Err` (so the manager rolled back).
    pub rolled_back: bool,
}

impl TxOutcome {
    /// Wraps a result, flagging rollback iff it is `Err`.
    pub fn of<R, E>(result: Result<R, E>) -> Self
    where
        R: Send + 'static,
        E: Send + 'static,
    {
        let rolled_back = result.is_err();
        TxOutcome {
            value: Box::new(result),
            rolled_back,
        }
    }
}

/// A boxed transactional operation future, run by a [`TransactionManager`]
/// inside the transaction boundary. Bounded by `'a` so a method body that
/// borrows `&self` (a non-`'static` future) can still be boxed.
pub type BoxedTxOp<'a> = Pin<Box<dyn Future<Output = Result<TxOutcome, TxError>> + Send + 'a>>;

/// The transaction-manager port — Spring's `PlatformTransactionManager`.
///
/// A concrete adapter (the sqlx manager, a Mongo session manager, …) implements
/// this once; the generic [`transactional`] orchestrator and the
/// `#[transactional]` macro target it through the process registry. The single
/// object-safe [`execute`](TransactionManager::execute) method establishes the
/// ambient transaction context, applies `opts.propagation`, runs `op`, and
/// commits/rolls back — returning the (type-erased) [`TxOutcome`], or a
/// [`TxError`] only for an *infrastructure* failure (begin/commit/propagation
/// violation), never for the application error inside the outcome.
#[async_trait]
pub trait TransactionManager: Send + Sync {
    /// Runs `op` within a transaction governed by `opts`.
    async fn execute<'a>(&self, opts: TxOptions, op: BoxedTxOp<'a>) -> Result<TxOutcome, TxError>;

    /// Whether a transaction is currently active on this task (for
    /// introspection / `Propagation` checks). Defaults to `false`.
    fn is_active(&self) -> bool {
        false
    }
}

/// The process-wide transaction manager, registered once at startup (typically
/// by a data starter / auto-configuration).
static MANAGER: OnceLock<Arc<dyn TransactionManager>> = OnceLock::new();

/// Registers the process transaction manager. Returns `false` if one was
/// already registered (the first registration wins, mirroring a single primary
/// `PlatformTransactionManager`).
pub fn register_transaction_manager(manager: Arc<dyn TransactionManager>) -> bool {
    MANAGER.set(manager).is_ok()
}

/// The registered process transaction manager, if any.
pub fn transaction_manager() -> Option<Arc<dyn TransactionManager>> {
    MANAGER.get().cloned()
}

/// Runs `f` inside a transaction governed by `opts` — the runtime entry point
/// the `#[transactional]` macro expands to, and a fine programmatic API in its
/// own right (Spring's `TransactionTemplate`).
///
/// Behaviour:
/// - If no [`TransactionManager`] is registered, `f` runs **without** a
///   transaction (graceful degradation, e.g. in unit tests with no datasource).
/// - Otherwise the manager applies `opts.propagation`/`isolation`, runs `f`,
///   **commits** if `f` returns `Ok`, and **rolls back** if `f` returns `Err`
///   (Rust's `Result` already separates success from failure, so the default
///   rollback rule is "rollback on any `Err`"; for finer control use
///   [`transactional_with`]).
/// - An infrastructure failure (begin/commit/propagation violation) surfaces as
///   `E::from(TxError)`.
pub async fn transactional<'a, R, E, F, Fut>(opts: TxOptions, f: F) -> Result<R, E>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = Result<R, E>> + Send + 'a,
    R: Send + 'static,
    E: Send + 'static + From<TxError>,
{
    transactional_with(opts, |_e| true, f).await
}

/// Like [`transactional`] but with an explicit **rollback rule**:
/// `should_rollback(&E) -> bool` decides, per error, whether to roll back
/// (`true`) or commit anyway (`false`) — Spring's `rollbackFor` /
/// `noRollbackFor`. The default in [`transactional`] always rolls back on
/// `Err`.
pub async fn transactional_with<'a, R, E, F, Fut, P>(
    opts: TxOptions,
    should_rollback: P,
    f: F,
) -> Result<R, E>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = Result<R, E>> + Send + 'a,
    P: FnOnce(&E) -> bool + Send + 'a,
    R: Send + 'static,
    E: Send + 'static + From<TxError>,
{
    match transaction_manager() {
        Some(manager) => transactional_with_on(&manager, opts, should_rollback, f).await,
        None => f().await,
    }
}

/// Like [`transactional`] but against an **explicit** manager instead of the
/// process registry — for programmatic transactions, multi-datasource setups,
/// and tests. (Spring's `new TransactionTemplate(specificTxManager)`.)
pub async fn transactional_on<'a, R, E, F, Fut>(
    manager: &Arc<dyn TransactionManager>,
    opts: TxOptions,
    f: F,
) -> Result<R, E>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = Result<R, E>> + Send + 'a,
    R: Send + 'static,
    E: Send + 'static + From<TxError>,
{
    transactional_with_on(manager, opts, |_e| true, f).await
}

/// The core orchestrator: run `f` through `manager` under `opts`, committing on
/// `Ok` and rolling back on `Err` per `should_rollback`.
pub async fn transactional_with_on<'a, R, E, F, Fut, P>(
    manager: &Arc<dyn TransactionManager>,
    opts: TxOptions,
    should_rollback: P,
    f: F,
) -> Result<R, E>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = Result<R, E>> + Send + 'a,
    P: FnOnce(&E) -> bool + Send + 'a,
    R: Send + 'static,
    E: Send + 'static + From<TxError>,
{
    let op: BoxedTxOp<'a> = Box::pin(async move {
        let result: Result<R, E> = f().await;
        let rolled_back = match &result {
            Ok(_) => false,
            Err(e) => should_rollback(e),
        };
        Ok(TxOutcome {
            value: Box::new(result),
            rolled_back,
        })
    });

    match manager.execute(opts, op).await {
        Ok(outcome) => *outcome
            .value
            .downcast::<Result<R, E>>()
            .expect("transaction outcome carries the operation's Result<R, E>"),
        Err(tx_err) => Err(E::from(tx_err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_options_builders() {
        assert_eq!(TxOptions::default().propagation, Propagation::Required);
        assert_eq!(
            TxOptions::requires_new().propagation,
            Propagation::RequiresNew
        );
        assert_eq!(TxOptions::nested().propagation, Propagation::Nested);
        assert!(TxOptions::default().read_only().read_only);
        assert_eq!(
            TxOptions::default()
                .with_isolation(Isolation::Serializable)
                .isolation,
            Isolation::Serializable
        );
    }

    #[test]
    fn isolation_sql() {
        assert_eq!(Isolation::Default.sql_level(), None);
        assert_eq!(Isolation::Serializable.sql_level(), Some("SERIALIZABLE"));
        assert_eq!(Isolation::ReadCommitted.sql_level(), Some("READ COMMITTED"));
    }

    #[tokio::test]
    async fn transactional_runs_without_manager() {
        // No manager registered in this test process → degrades to a plain call.
        let out: Result<i32, TxError> =
            transactional(TxOptions::default(), || async { Ok(7) }).await;
        assert_eq!(out.unwrap(), 7);
    }

    #[tokio::test]
    async fn transactional_propagates_error_without_manager() {
        let out: Result<i32, TxError> = transactional(TxOptions::default(), || async {
            Err(TxError::application("boom"))
        })
        .await;
        assert!(out.is_err());
    }
}
