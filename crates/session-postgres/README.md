# `firefly-session-postgres`

> **Tier:** Platform · **Status:** Full · **pyfly original:** `pyfly.session.adapters.postgres_registry.PostgresSessionRegistry`

## Overview

`firefly-session-postgres` is the **Postgres-backed, durable, distributed**
implementation of the [`firefly_session::SessionRegistry`](../session) port —
the Rust port of pyfly's `PostgresSessionRegistry`. It gives relational-only
deployments (no Redis required) a shared, per-principal index of live sessions:
every application instance reads and writes the same table, so the per-principal
concurrency cap enforced by
[`firefly_session::SessionConcurrencyController`](../session) holds **across the
whole cluster**, not just within one process (the limit of the in-process
[`MemorySessionRegistry`](../session)).

Plug it into the existing session-concurrency machinery with **no API change**:
it implements the same `SessionRegistry` trait the in-memory registry does
(`register` / `deregister` / `list_sessions` / `count`).

## Data model

A single table indexes every principal's live sessions, keyed by the session id
(so the same session id can never be double-registered):

```sql
CREATE TABLE IF NOT EXISTS firefly_session_registry (
    session_id  TEXT PRIMARY KEY,
    principal   TEXT NOT NULL,
    created_at  BIGINT NOT NULL
)
-- + a supporting index on (principal)
```

`created_at` is the session's epoch-millis creation time, stored as a `BIGINT`
so it round-trips the `SessionRegistry` contract's `i64` exactly (pyfly uses
`DOUBLE PRECISION`; the Rust trait's timestamp is an integer, so `BIGINT` is the
faithful column type).

| `SessionRegistry` method | SQL                                                                            |
|--------------------------|--------------------------------------------------------------------------------|
| `register`               | `INSERT … ON CONFLICT (session_id) DO UPDATE SET principal, created_at`         |
| `deregister`             | `DELETE … WHERE principal = $1 AND session_id = $2`                            |
| `list_sessions`          | `SELECT session_id, created_at … WHERE principal = $1 ORDER BY created_at ASC` |
| `count`                  | `SELECT COUNT(*) … WHERE principal = $1`                                       |

The `ORDER BY created_at ASC` makes `list_sessions` **oldest-first** (matching
the in-process registry and the Redis adapter), and the `ON CONFLICT … DO
UPDATE` makes `register` an idempotent upsert.

## Auto-DDL

Like pyfly, the backing table is created **lazily and idempotently** on first
use: the first registry method to run executes the `CREATE TABLE IF NOT EXISTS`
(plus the supporting index) exactly once, guarded by an async mutex so
concurrent first calls don't race the DDL. Call `init()` to force the DDL
eagerly at startup if you prefer to fail fast on a permission problem rather
than on the first login.

## Custom table names

By default the table is `firefly_session_registry` (`TABLE`). To target a
different table — e.g. to isolate parallel integration tests — use a
`_with_table` constructor. The table name is validated strictly (ASCII
`[a-z0-9_]`, leading letter or underscore, ≤ 63 bytes) and an invalid name is
rejected with `RegistryError::Backend` rather than interpolated into SQL, so
there is no injection surface.

## Usage

```rust,no_run
use std::sync::Arc;
use firefly_session::{SessionRegistry, SessionConcurrencyController, ConcurrencyPolicy, Strategy};
use firefly_session_postgres::PostgresSessionRegistry;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
// Connect from a URL (SQLAlchemy dialect markers are stripped) …
let registry = Arc::new(
    PostgresSessionRegistry::connect("postgresql://localhost/app").await?,
);
registry.init().await?; // optional: create the table eagerly

// … or inject an already-built tokio_postgres::Client (the DI entry point):
// let registry = Arc::new(PostgresSessionRegistry::from_client(client));

let controller = SessionConcurrencyController::new(
    registry.clone(),
    ConcurrencyPolicy { max_sessions: 2, strategy: Strategy::EvictOldest },
);
controller.on_login("alice", "session-1", 1_700_000_000_000).await;
assert_eq!(registry.count("alice").await, 1);
# Ok(())
# }
```

### Construction

| Constructor                              | Use                                                          |
|------------------------------------------|--------------------------------------------------------------|
| `connect(conn)`                          | Connect from a URL / DSN, default table.                     |
| `connect_with_table(conn, table)`        | Connect targeting a custom (validated) table.                |
| `from_client(client)`                    | Wrap an existing `tokio_postgres::Client` (DI), default table.|
| `from_client_with_table(client, table)`  | Wrap an existing client targeting a custom table.            |

The `SessionRegistry` trait methods are **infallible by contract**, so a
per-operation Postgres failure is logged via `tracing` and swallowed — the
concurrency cap simply isn't enforced for that one login. Only the constructors
and `init()` return a `RegistryError` (connection / table-name / DDL failures).

## Testing

- **Unit tests** (`src/lib.rs`) cover everything verifiable without a live DB:
  the SQL/DDL string shape, table-name validation (including injection
  attempts), the DSN normalisation, and `SessionRegistry` object-safety. They
  run on every `cargo test`.
- **Env-gated live tests** (`tests/postgres_registry_test.rs`) perform genuine
  round-trips — auto-DDL, register / list / count / deregister, upsert, and the
  evict-oldest controller flow — each on its own uniquely-named table (dropped
  via an RAII guard) so the suite is correct under the parallel runner. They
  read `FIREFLY_TEST_POSTGRES_URL` (falling back to `DATABASE_URL` /
  `POSTGRES_URL`); when unset they print a one-line `skipping …` and pass, so
  `cargo test` on a bare machine is green.

```sh
export FIREFLY_TEST_POSTGRES_URL="postgres://firefly:firefly@localhost:5432/firefly"
cargo test -p firefly-session-postgres
```

## See also

- [`firefly-session`](../session) — the session tier and the `SessionRegistry`
  port + in-process `MemorySessionRegistry` this crate implements.
- [`firefly-session-redis`](../session-redis) — the Redis-backed alternative
  (sorted-set index with a sliding TTL).
- [`firefly-cache-postgres`](../cache-postgres) — the sibling Postgres cache
  adapter whose connection / DSN / table-validation convention this crate
  follows.
