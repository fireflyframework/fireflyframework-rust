# `firefly-data-sqlx`

> **Tier:** Platform · **Status:** Full · **Java original:** Spring Data R2DBC · **pyfly module:** `data.relational.sqlalchemy`

## Overview

`firefly-data-sqlx` is the **relational repository adapter** that implements
the [`firefly-data`](../data) ports over [`sqlx`](https://github.com/launchbadge/sqlx)
for **PostgreSQL, MySQL, and SQLite** from a single codebase. It is the Rust
analogue of pyfly's SQLAlchemy adapter
(`pyfly.data.relational.sqlalchemy.repository.Repository[T, ID]`), which
serves all three relational backends behind one `Repository` surface.

The repositories are generic over the entity `T` and its id, and select the
matching `SqlDialect` at runtime from the connection-pool's backend kind — so
"new relational DB = new pool", **not** "new adapter". `Filter`,
`Specification`, and `Pageable` are compiled through that dialect, so
placeholders (`$n` vs `?`), identifier quoting (`"id"` vs `` `id` ``),
`IN`-list shape, and case-insensitive `LIKE` are all correct per backend, and
`UPSERT` uses each backend's flavour.

## Public surface

| type | role |
|---|---|
| `Db` | a backend-tagged pool (`Postgres(PgPool)` / `MySql(MySqlPool)` / `Sqlite(SqlitePool)`); hands out the matching `SqlDialect` |
| `Backend` | the backend kind (`Postgres` / `MySql` / `Sqlite`) |
| `SqlxReactiveRepository<T, ID>` | streaming `ReactiveCrudRepository` + `ReactiveSpecificationRepository` (Spring Data R2DBC analogue) |
| `SqlxRepository<T, K>` | the blocking-style `Repository` over the same SQL |
| `AnyRow` + `SqlxRowMapper<T>` | one backend-agnostic row mapper, column-name accessors dispatch to the concrete backend row |
| `RowWriter<T>` + `ColumnValue` | the entity's `(column, value)` pairs; the repo builds the dialect-aware `UPSERT` |

### Behaviour

- **Dialect-aware `UPSERT`** — `INSERT … ON CONFLICT(<id>) DO UPDATE` for
  Postgres/SQLite, `INSERT … ON DUPLICATE KEY UPDATE` for MySQL. No
  `RETURNING` is used (MySQL has none); the row is re-read by id to return the
  persisted value.
- **Streaming reads** — `find_all` / `find_all_by_id` / `find_by_spec` /
  `find_by_spec_paged` drive sqlx's `fetch` row stream into a `Flux`, decoding
  and emitting each row as it arrives. There is **no** collect-then-emit
  buffering, so a million-row table never lands fully in memory.
- **Auditing** — an optional `Auditor` (via `.with_auditor(..)`) auto-stamps
  `created_at` / `updated_at` / `created_by` / `updated_by` on every write
  (`created_*` on insert, `updated_*` moved on update).
- **Soft delete** — an optional `SoftDeletePolicy` (via
  `.with_soft_delete(..)`) hides soft-deleted rows from **every** read path
  and turns `delete` into a `deleted_at` stamp instead of a physical `DELETE`.

## Quick start (SQLite — runs on a bare machine)

```rust,ignore
use firefly_data::{ReactiveCrudRepository, TableConfig};
use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
use firefly_kernel::FireflyError;

#[derive(Debug, Clone, PartialEq)]
struct User { id: String, name: String }

let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
sqlx::query(r#"CREATE TABLE "users" ("id" TEXT PRIMARY KEY, "name" TEXT NOT NULL)"#)
    .execute(&pool).await?;

let repo: SqlxReactiveRepository<User, String> = SqlxReactiveRepository::new(
    Db::Sqlite(pool),
    TableConfig::new("users", "id", ["id", "name"]),
    // RowMapper: decode (id, name) — backend-agnostic via AnyRow.
    |row: &AnyRow| Ok::<_, FireflyError>(User {
        id: row.get_str("id")?,
        name: row.get_str("name")?,
    }),
    // RowWriter: the entity's (column, value) pairs.
    |u: &User| vec![
        ColumnValue::new("id", u.id.clone()),
        ColumnValue::new("name", u.name.clone()),
    ],
);

let saved = repo.save(User { id: "u1".into(), name: "alice".into() })
    .block().await?;
```

### With auditing + soft delete

```rust,ignore
use firefly_data::{Auditor, SoftDeletePolicy, UserProvider};
use std::sync::Arc;

let provider: UserProvider = Arc::new(|| Some("alice".to_string()));
let repo = SqlxReactiveRepository::new(db, config, mapper, writer)
    .with_auditor(Auditor::with_user_provider(provider))
    .with_soft_delete(SoftDeletePolicy::new()); // guards the `deleted_at` column
```

### Picking a backend

```rust,ignore
let db = Db::Postgres(sqlx::PgPool::connect(&pg_url).await?);  // $n / "id" / ON CONFLICT
let db = Db::MySql(sqlx::MySqlPool::connect(&my_url).await?);  // ? / `id` / ON DUPLICATE KEY
let db = Db::Sqlite(sqlx::SqlitePool::connect(&sqlite_url).await?); // ? / "id" / ON CONFLICT
```

The repository compiles the same `Filter` / `Specification` / `Pageable`
through `db.dialect()`, so the entity, mapper, and writer are written once and
run against all three.

## Testing

- **SQLite** tests run on a **bare machine** (in-memory and file-backed, no
  `#[ignore]`): `cargo test -p firefly-data-sqlx`.
- **PostgreSQL / MySQL** round-trips are **env-gated**: they run when
  `FIREFLY_TEST_POSTGRES_URL` / `FIREFLY_TEST_MYSQL_URL` are set, and skip
  cleanly when unset so `cargo test` stays green without a database.

```sh
# Run the full suite (CRUD + spec + pageable + auditing + soft-delete) against
# every backend you have running:
export FIREFLY_TEST_POSTGRES_URL="postgres://firefly:firefly@localhost:5442/firefly"
export FIREFLY_TEST_MYSQL_URL="mysql://firefly:firefly@localhost:3307/firefly"
cargo test -p firefly-data-sqlx
```

## Cargo features

All three backends are enabled by default. Disable the ones you do not need
for a smaller build, e.g. a SQLite-only repository:

```toml
firefly-data-sqlx = { version = "26.6.3", default-features = false, features = ["sqlite"] }
```

## License

Apache-2.0 — see the workspace `LICENSE`.
