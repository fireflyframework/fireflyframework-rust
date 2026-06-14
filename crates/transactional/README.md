# `firefly-transactional`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-transactional` provides **context-bound transaction
management** over a database port, in the declarative style of mature
transaction frameworks. The canonical service shape:

```rust,ignore
let outcome = with_tx(&TxContext::root(), &db, |ctx| {
    // every repository call here uses the same transaction via ctx.
    exec(ctx, &db).execute("INSERT …", &[])?;
    order_repo.save(ctx, &order)
})?;
```

`with_tx` begins a transaction, hands the closure a `TxContext`
carrying it, commits on `Ok`, rolls back otherwise. Repositories that
want to participate read the transaction off the context with
`TxContext::tx` (or use the convenience `exec` helper) and fall back to
the supplied `Executor` when no transaction is active.

Rust has no ambient context, so the transaction travels in an
explicit, `Copy` `TxContext` handle rather than thread-local state.
Rather than binding to a concrete connection type, the crate defines
small synchronous **port traits** (`Database` / `Transaction` /
`Executor`) so any driver — rusqlite, postgres, a mock — can plug in.

## Why not `begin()` directly?

`Database::begin` returns a `Transaction` you must manually commit /
rollback in every code path. `with_tx` lifts the boilerplate:

* Commit on the closure returning `Ok`.
* Rollback on any `Err` from the closure (the original error is
  returned; a rollback failure is joined into
  `TxError::RollbackFailed`).
* Rollback on panic — a `Drop` guard rolls back while the panic
  unwinds, then the panic resumes.
* Nested `with_tx` calls reuse the outer transaction (a required-style
  propagation); commit/rollback is owned by the outermost caller.

## Mental model

```
with_tx(ctx, db, f) ─┐
                     │ db.begin()
                     │   │
                     │   ▼
                     │ ctx2 = TxContext { tx }
                     │   │
                     │   ▼
                     │ result = f(&ctx2)      ← repo calls exec(ctx2, db) → tx
                     │   │
       ┌─────────────┘
       │
       ▼
   result.is_ok() ? commit() : rollback()
   panic           ? Drop guard rollback, panic resumes
```

## Public surface

```rust,ignore
pub trait Executor: Send + Sync {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError>;
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError>;
    fn query_row(&self, sql: &str, params: &[SqlValue]) -> Result<Option<Row>, TxError>;
}

pub trait Database: Executor {
    fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError>;
}

pub trait Transaction: Executor {
    fn commit(self: Box<Self>) -> Result<(), TxError>;
    fn rollback(self: Box<Self>) -> Result<(), TxError>;
}

pub fn with_tx<T, D, F>(ctx: &TxContext<'_>, db: &D, f: F) -> Result<T, TxError>;
pub fn exec<'a>(ctx: &TxContext<'a>, db: &'a dyn Executor) -> Conn<'a>;

impl TxContext<'_> {
    pub const fn root() -> Self;
    pub fn tx(&self) -> Option<&dyn Transaction>;
    pub fn in_transaction(&self) -> bool;
}
```

`exec(ctx, db)` returns a `Conn` wrapping the transaction if one is
active on the context, otherwise `db` — exactly what every repository
wants. `Conn` implements `Executor` either way. `SqlValue` (Null /
Integer / Real / Text / Blob) and `Row` form the typed parameter and
column model used across the port traits.

## Quick start

```rust
use firefly_transactional::{exec, with_tx, Database, Executor, SqlValue, TxContext, TxError};

struct OrderRepo;

impl OrderRepo {
    /// Joins the ambient transaction when one is active, falls back to
    /// the plain connection otherwise.
    fn save(&self, ctx: &TxContext<'_>, db: &dyn Executor, total: i64) -> Result<(), TxError> {
        exec(ctx, db).execute(
            "INSERT INTO orders(total) VALUES (?1)",
            &[SqlValue::Integer(total)],
        )?;
        Ok(())
    }
}

/// Service composition: both writes share one transaction.
fn place_order<D: Database>(repo: &OrderRepo, db: &D) -> Result<(), TxError> {
    with_tx(&TxContext::root(), db, |ctx| {
        repo.save(ctx, db, 42)?;
        repo.save(ctx, db, 7) // same tx — both commit or both roll back
    })
}
```

Implement the three port traits once per driver. The integration tests
in `tests/with_tx.rs` contain a complete reference implementation over
`rusqlite`.

## Testing

```bash
cargo test -p firefly-transactional
```

Covers the four core cases — commit on success, rollback on error,
nested participation (two inserts inside a single outer tx), and the
`exec` fallback when no tx is active — against a real SQLite database,
plus mock-driven coverage of begin/commit wrapping, the
rollback-failure join, nested commit ownership, rollback-on-panic via
the `Drop` guard, application-error downcasting, value-returning
closures, and Send + Sync bounds.
