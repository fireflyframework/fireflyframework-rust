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

//! firefly-data-sqlx — the **relational** repository adapter implementing
//! the [`firefly_data`] ports over [`sqlx`] for PostgreSQL, MySQL, **and**
//! SQLite from a single codebase.
//!
//! This is the Rust analogue of pyfly's SQLAlchemy adapter
//! (`pyfly.data.relational.sqlalchemy.repository.Repository[T, ID]`), which
//! serves all three relational backends behind one `Repository` surface. The
//! Rust port does the same: [`SqlxReactiveRepository`] and [`SqlxRepository`]
//! are generic over the entity `T` and its id, and select the right
//! [`SqlDialect`](firefly_data::SqlDialect) at runtime from the [`Db`] pool's
//! backend kind — so "new relational DB = new pool", not "new adapter".
//!
//! # Architecture
//!
//! | concern | type | notes |
//! |---|---|---|
//! | pool + dialect | [`Db`] | tags a `PgPool` / `MySqlPool` / `SqlitePool` with its [`Backend`], hands out the matching [`SqlDialect`](firefly_data::SqlDialect) |
//! | row decoding | [`SqlxRowMapper`] over [`AnyRow`] | one mapper decodes every backend's rows by column name |
//! | write columns | [`RowWriter`] → [`ColumnValue`] | the entity's `(column, value)` pairs; the repo builds the dialect-aware `UPSERT` |
//! | reactive CRUD | [`SqlxReactiveRepository`] | streams reads as a [`Flux`](firefly_reactive::Flux), implements [`ReactiveCrudRepository`](firefly_data::ReactiveCrudRepository) + [`ReactiveSpecificationRepository`](firefly_data::ReactiveSpecificationRepository) |
//! | blocking CRUD | [`SqlxRepository`] | the awaited-value [`Repository`](firefly_data::Repository) over the same SQL |
//!
//! ## Dialect-aware behaviour
//!
//! - [`Filter`](firefly_data::Filter) /
//!   [`Specification`](firefly_data::Specification) /
//!   [`Pageable`](firefly_data::Pageable) are compiled through the pool's
//!   [`SqlDialect`](firefly_data::SqlDialect), so placeholders (`$n` vs `?`),
//!   identifier quoting (`"id"` vs `` `id` ``), `IN`-list shape, and
//!   case-insensitive `LIKE` are all correct per backend.
//! - **`UPSERT`** is dialect-aware: `ON CONFLICT(<id>) DO UPDATE` for
//!   Postgres/SQLite, `ON DUPLICATE KEY UPDATE` for MySQL.
//! - Reads **stream** lazily off sqlx's
//!   [`fetch`](sqlx::Executor) row stream into a [`Flux`](firefly_reactive::Flux)
//!   — no collect-then-emit.
//! - An optional [`Auditor`](firefly_data::Auditor) auto-stamps audit
//!   columns on every write; an optional
//!   [`SoftDeletePolicy`](firefly_data::SoftDeletePolicy) hides soft-deleted
//!   rows from every read and turns `delete` into a `deleted_at` stamp.
//!
//! ## Derived & custom queries (executed end-to-end)
//!
//! The adapter runs Spring-Data **derived query methods** and **`@query`
//! custom queries** against the live pool — the Rust analogue of pyfly's
//! repository bean post-processor:
//!
//! - [`find_by_derived`](SqlxReactiveRepository::find_by_derived) /
//!   [`count_by_derived`](SqlxReactiveRepository::count_by_derived) /
//!   [`exists_by_derived`](SqlxReactiveRepository::exists_by_derived) /
//!   [`delete_by_derived`](SqlxReactiveRepository::delete_by_derived) take a
//!   `find_by_status_and_role`-style method name plus the bound arguments,
//!   parse it with [`QueryMethodParser`](firefly_data::QueryMethodParser),
//!   render the dialect-aware SQL, and execute it.
//! - [`query_list`](SqlxReactiveRepository::query_list) /
//!   [`query_count`](SqlxReactiveRepository::query_count) /
//!   [`query_exists`](SqlxReactiveRepository::query_exists) /
//!   [`query_execute`](SqlxReactiveRepository::query_execute) run a
//!   [`CustomQuery`](firefly_data::CustomQuery) (native SQL or JPQL-like)
//!   with `:param` named-parameter binding and count/exists/list
//!   return-shape inference.
//! - [`project_by_spec`](SqlxReactiveRepository::project_by_spec) runs a
//!   DB-level [`ColumnProjection`](firefly_data::ColumnProjection): it
//!   `SELECT`s only the projected columns and streams the narrowed rows.
//!
//! ## Actuator integration (feature `actuator`)
//!
//! With the `actuator` feature enabled, [`SqlxHealthIndicator`] contributes a
//! `db` component to `GET /actuator/health` (`SELECT 1`, reporting the
//! backend kind) and [`SqlxQueryMetrics`] records the
//! `pyfly_db_query_duration_seconds` / `pyfly_db_queries_total` /
//! `pyfly_db_query_errors_total` metrics with a bounded `operation` label.
//!
//! # Quick start (SQLite, runs on a bare machine)
//!
//! ```
//! use firefly_data::{ReactiveCrudRepository, TableConfig};
//! use firefly_data_sqlx::{AnyRow, ColumnValue, Db, SqlxReactiveRepository};
//! use firefly_kernel::FireflyError;
//!
//! #[derive(Debug, Clone, PartialEq)]
//! struct User {
//!     id: String,
//!     name: String,
//! }
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
//! sqlx::query(r#"CREATE TABLE "users" ("id" TEXT PRIMARY KEY, "name" TEXT NOT NULL)"#)
//!     .execute(&pool)
//!     .await
//!     .unwrap();
//!
//! let repo: SqlxReactiveRepository<User, String> = SqlxReactiveRepository::new(
//!     Db::Sqlite(pool),
//!     TableConfig::new("users", "id", ["id", "name"]),
//!     // RowMapper: decode (id, name) — backend-agnostic via AnyRow.
//!     |row: &AnyRow| {
//!         Ok::<_, FireflyError>(User {
//!             id: row.get_str("id")?,
//!             name: row.get_str("name")?,
//!         })
//!     },
//!     // RowWriter: the entity's (column, value) pairs.
//!     |u: &User| {
//!         vec![
//!             ColumnValue::new("id", u.id.clone()),
//!             ColumnValue::new("name", u.name.clone()),
//!         ]
//!     },
//! );
//!
//! let saved = repo.save(User { id: "u1".into(), name: "alice".into() })
//!     .block().await.unwrap();
//! assert_eq!(saved, Some(User { id: "u1".into(), name: "alice".into() }));
//! # });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod binding;
mod connect;
mod db;
mod entity;
#[cfg(feature = "actuator")]
mod observe;
mod repository;
mod row;
mod sql;
mod tx;
mod writer;

pub use connect::{auto_configure, DataSourceProperties};
pub use db::{Backend, Db};
pub use entity::{parse_timestamp, repository_for, SqlxEntity};
pub use repository::{is_optimistic_lock, SqlKey, SqlxReactiveRepository, SqlxRepository};
pub use row::{AnyRow, SqlxRowMapper, TryGetAcross};
pub use tx::SqlxTransactionManager;
pub use writer::{ColumnValue, RowWriter};

#[cfg(feature = "actuator")]
pub use observe::{
    operation_label, SqlxHealthIndicator, SqlxQueryMetrics, DB_QUERIES_TOTAL, DB_QUERY_DURATION,
    DB_QUERY_ERRORS_TOTAL,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.13";
