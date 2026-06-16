# `firefly-data-sqlx`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-data-sqlx` is the **relational repository adapter** that implements
the [`firefly-data`](../data) ports over [`sqlx`](https://github.com/launchbadge/sqlx)
for **PostgreSQL, MySQL, and SQLite** from a single codebase. It serves all
three relational backends behind one `Repository` surface.

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
| `SqlxReactiveRepository<T, ID>` | streaming `ReactiveCrudRepository` + `ReactiveSpecificationRepository` |
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

### Derived & custom queries (executed end-to-end)

The adapter runs **derived query methods** and **`@query` custom queries**
against the live pool. Rust has no runtime reflection, so the query is
named/described at the call site rather than via a stub method:

```rust,ignore
use firefly_data::CustomQuery;
use std::collections::BTreeMap;

// Derived query method names, parsed + rendered + executed:
let active = repo.find_by_derived("find_by_active", &[json!(true)]);            // Flux<T>
let n      = repo.count_by_derived("count_by_active", &[json!(true)]);          // Mono<i64>
let any    = repo.exists_by_derived("exists_by_email", &[json!("a@b.com")]);    // Mono<bool>
let gone   = repo.delete_by_derived("delete_by_status", &[json!("expired")]);   // Mono<u64>
// Connectors, operators, and order_by all work:
let combo  = repo.find_by_derived(
    "find_by_active_and_score_greater_than_order_by_score_desc",
    &[json!(true), json!(5)],
);

// @query custom queries with :param named binding + return-shape inference:
let mut params = BTreeMap::new();
params.insert("min".into(), json!(20));
let q = CustomQuery::native("SELECT * FROM users WHERE score >= :min ORDER BY score");
let rows = repo.query_list(&q, "User", &params);                                // Flux<T>
let cnt  = repo.query_count(&CustomQuery::native("SELECT COUNT(*) FROM users WHERE active = :f"),
                            "User", &params);                                    // Mono<i64>
let upd  = repo.query_execute(&CustomQuery::native("UPDATE users SET x = :v WHERE id = :id"),
                              "User", &params);                                  // Mono<u64>
// JPQL is transpiled to SQL first:
let jpql = CustomQuery::jpql("SELECT u FROM User u WHERE u.email = :email");
let list = repo.query_list(&jpql, "User", &params);

// DB-level interface projection — only the projected columns cross the wire:
use firefly_data::{ColumnProjection, Specification, Predicate, Op};
let proj = ColumnProjection::new("UserSummary", ["id", "name"]);
let summaries = repo.project_by_spec(&proj, Specification::pred(Predicate::new("active", Op::Eq, true)));
// -> Flux<serde_json::Value>, each value an object of just {id, name}.
```

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

## Actuator integration (feature `actuator`)

Enable the `actuator` feature to get a database health component and per-query
metrics:

```toml
firefly-data-sqlx = { version = "26.6", features = ["actuator"] }
```

- `SqlxHealthIndicator` implements `firefly_actuator::HealthIndicator`: it
  runs `SELECT 1` and reports `UP` (with the backend kind on
  `details.database`) — the `db` component on `GET /actuator/health`.
  `SqlxHealthIndicator::named(db, "db-reporting")` probes a named datasource
  under its own component name.
- `SqlxQueryMetrics` records `firefly_db_query_duration_seconds` /
  `firefly_db_queries_total` / `firefly_db_query_errors_total`, all labelled by
  a **bounded** `operation` (`SELECT` / `INSERT` / `UPDATE` / `DELETE` /
  `OTHER`).

## Cargo features

All three backends are enabled by default. Disable the ones you do not need
for a smaller build, e.g. a SQLite-only repository:

```toml
firefly-data-sqlx = { version = "26.6", default-features = false, features = ["sqlite"] }
```

The `actuator` feature (off by default) adds the health/metrics integration
above.

## License

Apache-2.0 — see the workspace `LICENSE`.
