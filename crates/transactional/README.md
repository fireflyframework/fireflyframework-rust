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

## In-process application events

The crate also ships a **thread-safe, async** in-process
publish/subscribe — the Rust port of Spring's `@EventListener` and
`@TransactionalEventListener`. It lives in the `events` module and is
keyed on the concrete event [`TypeId`]; the event type only has to be
`Any + Send + Sync + 'static`.

Publish a domain event with `publish_event`:

```rust,ignore
firefly::publish_event(OrderPlaced { id }).await;
```

A listener is a free `async fn` that takes the event by shared
reference. There are two flavours.

**Immediate** (`@EventListener`): runs synchronously — awaited — at
`publish_event` time, in registration order.

```rust,ignore
#[firefly::application_event_listener]
async fn on_order_placed(event: &OrderPlaced) {
    audit_log(event).await;
}
```

**Transaction-bound** (`@TransactionalEventListener`): when the event is
published inside an active transaction, it is buffered and dispatched at
the surrounding transaction's phase. The phase is one of:

* `after_commit` — once the transaction has committed (the default, and
  the canonical phase: publish integration events, send notifications,
  evict caches).
* `before_commit` — just before the commit, still inside the
  transaction.
* `after_rollback` — once the transaction has rolled back.
* `after_completion` — after either outcome.

```rust,ignore
#[firefly::transactional_event_listener]                       // after_commit
async fn publish_integration_event(event: &WalletOpened) {
    bus.send(event).await;   // only after the opening transaction commits
}

#[firefly::transactional_event_listener(phase = "after_rollback")]
async fn release_reservation(event: &PaymentFailed) {
    inventory.release(event).await;
}
```

When a transaction-bound event is published with **no** active
transaction — for example a service running without a registered
transaction manager, the same graceful-degradation path the
`#[transactional]` orchestrator itself takes — the listener falls back
to running immediately, as if the work had already committed.
`after_rollback` listeners do not fire on this path. This keeps a
`@TransactionalEventListener` useful in unit tests and datasource-less
setups instead of silently dropping the event.

Listeners are discovered via `inventory`: each macro emits a registration
thunk, and the first `publish_event` drains them once, so listeners
defined anywhere in the crate graph are live without manual wiring.
`register_event_listener` and `TransactionPhase` are the programmatic
counterparts the macros expand to.

> The pre-existing `#[event_listener("topic")]` macro is something else
> entirely: it is the **broker consumer** (`@KafkaListener`-style topic
> subscription) from `firefly-eda`. Use `#[application_event_listener]`
> for the in-process bus described here.

### `LocalTransactionManager`

Transaction-bound dispatch is driven by the transaction's commit /
rollback phases, which means it needs a registered
[`TransactionManager`]. `LocalTransactionManager` is a transaction
manager with **no backing datasource** — the Rust analog of Spring's
`ResourcelessTransactionManager`. It runs the operation and honours the
outcome's commit/rollback decision, driving the after-commit /
after-rollback dispatch without a database. Register it when you want
transaction-bound event semantics but have no SQL datasource, and in
tests that exercise the phases:

```rust
use std::sync::Arc;
use firefly_transactional::{register_transaction_manager, LocalTransactionManager};

register_transaction_manager(Arc::new(LocalTransactionManager));
```

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
