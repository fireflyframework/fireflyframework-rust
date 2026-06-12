# `firefly-cache-postgres`

> **Tier:** Platform · **Status:** Full · **pyfly original:** `pyfly.cache.adapters.postgres.PostgresCacheAdapter`

## Overview

`firefly-cache-postgres` is the PostgreSQL implementation of the
[`firefly_cache::Adapter`](../cache) port — the Rust port of pyfly's
`PostgresCacheAdapter`. It stores cache entries in a single key/value/expiry
table and speaks SQL over [`tokio-postgres`](https://crates.io/crates/tokio-postgres),
so a `PostgresCacheAdapter` drops in wherever an `Arc<dyn Adapter>` is
expected (CQRS query cache, idempotency guards, `FallbackAdapter` primaries,
the `Typed<T>` facade).

```
firefly_cache::Adapter  (port)
        ▲
        │  impl
 PostgresCacheAdapter ──► tokio_postgres::Client ──► PostgreSQL
                                                      (firefly_cache_entries)
```

## Table shape

The adapter owns one table, created on `init()` (`CREATE TABLE IF NOT
EXISTS`), identical in shape to pyfly's `pyfly_cache_entries` under the Rust
framework's `firefly_` prefix:

```sql
CREATE TABLE IF NOT EXISTS firefly_cache_entries (
    cache_key   TEXT PRIMARY KEY,
    value       BYTEA NOT NULL,
    expires_at  TIMESTAMPTZ NULL
);
```

* **`cache_key`** — the primary key; upserts use `ON CONFLICT (cache_key)`.
* **`value`** — opaque `BYTEA`. Values cross the port as raw bytes; JSON
  encoding lives in [`firefly_cache::Typed`], so the table is byte-transparent
  and wire-compatible with the memory and Redis adapters.
* **`expires_at`** — `NULL` for a persistent entry, otherwise an absolute
  UTC timestamp (`now + ttl`). Expiry is enforced **lazily at read time** by
  an `expires_at IS NULL OR expires_at > now` predicate — there is no
  background sweeper, exactly as in pyfly.

## Port mapping

| Port method     | SQL                                                                |
|-----------------|-------------------------------------------------------------------|
| `get`           | `SELECT value … WHERE cache_key = $1 AND (not expired)`            |
| `set`           | `INSERT … ON CONFLICT (cache_key) DO UPDATE`                       |
| `set_if_absent` | `INSERT … ON CONFLICT (cache_key) DO NOTHING` (rows affected)      |
| `delete`        | `DELETE … WHERE cache_key = $1`                                    |
| `exists`        | `SELECT 1 … WHERE cache_key = $1 AND (not expired)`                |
| `delete_prefix` | `DELETE … WHERE cache_key LIKE $1 ESCAPE '\'`                      |
| `clear`         | `DELETE FROM firefly_cache_entries`                               |
| `stats`         | `SELECT COUNT(*) … (not expired)` + in-process hit/miss counters   |
| `health_check`  | `SELECT 1`                                                        |

Extras beyond the port (parity with pyfly): `keys(pattern, limit)`
(`get_keys`) and `is_available()` (fail-soft `SELECT 1`).

### `set_if_absent` and expired rows

Like pyfly's `put_if_absent`, `set_if_absent` keeps the fast `ON CONFLICT DO
NOTHING` path. An **expired** row still physically exists and therefore still
blocks the insert, even though `get`/`exists` treat it as a miss. Callers must
not rely on `set_if_absent` overwriting a stale entry — use `set` for that.

## Usage

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use firefly_cache::{Adapter, Typed};
use firefly_cache_postgres::PostgresCacheAdapter;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let adapter = Arc::new(
    PostgresCacheAdapter::connect("postgresql://localhost/app").await?,
);
adapter.init().await?;                       // CREATE TABLE IF NOT EXISTS
adapter.set("k", b"v", Some(Duration::from_secs(60))).await?;
assert_eq!(adapter.get("k").await?, b"v");
# Ok(())
# }
```

The connection string accepts a `postgresql://` URL, a `tokio-postgres`
keyword/value string (`host=… user=…`), or a SQLAlchemy-style URL with a
dialect marker (`postgresql+asyncpg://…`) — the marker is stripped
automatically so pyfly-style URLs connect unchanged. You can also inject an
already-built `tokio_postgres::Client` via `from_client`.

Unlike pyfly (which has explicit `start()`/`stop()` hooks over an injected
SQLAlchemy engine), this adapter's `init()` runs the DDL and there is no
`stop` — the `Client`'s lifecycle belongs to its owner.

## Custom table name

By default every statement targets `firefly_cache_entries`. To point an
adapter at a **different** table — to give two logical caches their own
storage, or to isolate parallel integration tests — use the additive
`_with_table` constructors:

```rust,no_run
use firefly_cache_postgres::PostgresCacheAdapter;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
// From a connection string:
let cache = PostgresCacheAdapter::connect_with_table(
    "postgresql://localhost/app",
    "orders_cache",
).await?;
cache.init().await?;                 // CREATE TABLE IF NOT EXISTS orders_cache (…)
assert_eq!(cache.table(), "orders_cache");

// Or from an already-built client (returns Result — the name is validated):
# let client: tokio_postgres::Client = unreachable!();
let cache = PostgresCacheAdapter::from_client_with_table(client, "orders_cache")?;
# Ok(())
# }
```

Every statement (DDL / UPSERT / SELECT / EXISTS / DELETE / DELETE-prefix /
CLEAR / COUNT / SELECT-KEYS) is rendered once at construction from the chosen
name. The table name is **validated strictly** before it ever reaches SQL —
ASCII `[a-z0-9_]` only, must start with a letter or underscore, at most 63
bytes (Postgres's identifier limit) — and an invalid name (e.g. anything with
spaces, quotes, dots, or a `; DROP TABLE …` payload) is rejected with
`CacheError::Backend` rather than being interpolated, so there is no SQL
injection surface. The plain `connect` / `from_client` constructors and the
public `TABLE` / `DDL` / `UPSERT_SQL` / … consts are unchanged and still
default to `firefly_cache_entries`. The standalone `validate_table_name`
helper is public for callers who want to validate a candidate name up front.

## Testing

Unit tests (`src/lib.rs`) cover everything verifiable without a live database:
the SQL/DDL string shapes, the glob→`LIKE` / TTL→timestamp / DSN-normalisation
logic, and `Adapter` object-safety. They run with a plain `cargo test`.

The behavioural round-trips (`tests/postgres_cache_adapter_test.rs`, ported
from pyfly's `tests/cache/test_postgres_cache_adapter.py`) are **env-gated**:
they read `FIREFLY_TEST_POSTGRES_URL` (falling back to `DATABASE_URL` /
`POSTGRES_URL`). When it is unset each test prints a one-line `skipping …` and
returns, so a plain `cargo test` on a bare machine is green; when it is set
they run the genuine set / get / delete / stats round-trip against a live
database:

```sh
export FIREFLY_TEST_POSTGRES_URL="postgres://firefly:firefly@localhost:5432/firefly"
cargo test -p firefly-cache-postgres
```

Each test gets its **own uniquely-named table** (via `connect_with_table`,
named `fftest_cache_<slug>_<pid>_<n>`), `init()`s it, and `DROP`s it on
teardown (even on panic). Because no two tests touch the same table,
assertions on table-wide state — `stats`' `COUNT(*)`, `keys`, hit/miss
semantics — are immune to other tests' rows, so the suite is correct under the
**default parallel** test runner with no `--test-threads=1` needed.
