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

//! firefly-data — persistence abstractions every Firefly service shares.
//!
//! This crate is the Rust port of the Go module `data` (Java original:
//! `firefly-common-data`). It provides three things:
//!
//! - a generic **filter DSL** ([`Filter`], [`Predicate`], [`Op`],
//!   [`Sort`], [`Direction`]) that renders to parameterised PostgreSQL
//!   (`$1`, `$2`, … placeholders, double-quoted identifiers),
//! - the canonical [`Page`] paged-result envelope, wire-compatible with
//!   the Java/.NET/Go ports' `Page<T>` JSON shape, and
//! - a typed [`Repository`] CRUD contract with an in-memory
//!   implementation ([`MemoryRepository`]) for tests.
//!
//! Services that talk to a real database implement [`Repository`]
//! against their driver of choice, using [`Filter::to_sql`] to render
//! the WHERE / ORDER BY / LIMIT clauses.
//!
//! ## pyfly parity
//!
//! On top of the Go-parity surface above, this crate ports pyfly's
//! Spring-Data-style data primitives, all kept storage-agnostic (no SQL
//! engine is implied):
//!
//! - **[`Specification`]** — composable query predicates (`Pred` /
//!   `And` / `Or` / `Not`, combined with `&` / `|` / `!`) that lower to
//!   the [`Filter`] DSL ([`Specification::to_filter`]), render to a
//!   parenthesised parameterised SQL fragment
//!   ([`Specification::to_sql`]), lower to a MongoDB `$`-operator filter
//!   document ([`Specification::to_mongo`]), or evaluate in memory
//!   ([`Specification::matches`]).
//! - **[`SqlDialect`]** — the dialect abstraction
//!   ([`PostgresDialect`] / [`MySqlDialect`] / [`SqliteDialect`]) that
//!   keeps the DSL technology-agnostic: [`Filter::to_sql_with`] /
//!   [`Specification::to_sql_with`] render the *same* tree as
//!   PostgreSQL, MySQL, or SQLite, so a relational adapter just picks a
//!   dialect at runtime. [`Filter::to_sql`] / [`Specification::to_sql`]
//!   stay the PostgreSQL default for back-compat.
//! - **[`AuditStamps`] + [`Auditor`]** — automatic
//!   `created_at` / `updated_at` / `created_by` / `updated_by` stamping
//!   on insert and update.
//! - **[`SoftDelete`] + [`SoftDeletePolicy`]** — a `deleted_at` column
//!   helper plus predicate injection that hides soft-deleted rows from
//!   every read path.
//! - **[`RoutingPolicy`] + [`read_only`]** — read/write datasource
//!   routing; and **[`NamedDataSources`]**, a registry of additional
//!   named datasources.
//! - **[`Mapper`]** — a runtime object-to-object mapper (MapStruct
//!   equivalent): [`map`](Mapper::map) / [`map_list`](Mapper::map_list) /
//!   [`project`](Mapper::project) with source→dest field renaming,
//!   value transformers, exclusion, and serde-driven nested-model
//!   recursion.
//! - **[`Pageable`] + [`RequestSort`] + [`Order`]** — Spring-style
//!   pagination *request* types (1-based `page >= 1` validation,
//!   `of` / `unpaged` / `next` / `previous` / `offset`, sort
//!   composition), distinct from the [`Page`] *response* envelope, and
//!   wired into the repository paging API via
//!   [`Repository::find_page`].
//! - **[`QueryMethodParser`]** — Spring-Data derived query methods:
//!   parses `find_by_x_and_y_order_by_z`-style names into a
//!   [`ParsedQuery`] that lowers (with bound arguments) to the
//!   [`Filter`] / [`Specification`] DSL, renders a *complete*
//!   dialect-aware SQL statement per prefix
//!   ([`ParsedQuery::to_sql`] → `find` / `count` / `exists` /
//!   `delete`), and executes against an in-memory collection. The sqlx /
//!   MongoDB adapters run these end-to-end via their
//!   `query_method` / `derived_query` helpers.
//! - **[`CustomQuery`] + [`transpile_jpql`] + [`substitute_named_params`]**
//!   — the `@query` custom-query path: a raw SQL / JPQL string with `:param`
//!   named placeholders (relational, bound to positional placeholders with
//!   [`QueryShape`] return-shape inference) and a JSON filter /
//!   aggregation-pipeline string with `":param"` placeholders (document).
//! - **[`ColumnProjection`]** — DB-level interface projections: a
//!   column-subset that narrows the `SELECT` list (or Mongo projection
//!   document) so only the projected columns cross the wire, the DB-side
//!   complement of [`Mapper::project`].
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Direction, Filter, MemoryRepository, Repository};
//!
//! #[derive(Clone)]
//! struct User {
//!     id: String,
//!     name: String,
//! }
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let repo = MemoryRepository::new(|u: &User| u.id.clone());
//! repo.save(User { id: "u1".into(), name: "alice".into() })
//!     .await
//!     .unwrap();
//!
//! let f = Filter::new()
//!     .where_eq("name", "alice")
//!     .order_by("id", Direction::Asc)
//!     .paged(0, 10);
//! let page = repo.find(&f).await.unwrap();
//! assert_eq!(page.total_elements, 1);
//! # });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auditing;
mod custom_query;
mod dialect;
mod filter;
mod mapper;
mod page;
mod pageable;
mod projection;
mod query_parser;
mod reactive;
mod repository;
mod routing;
mod soft_delete;
mod specification;

pub use auditing::{AuditStamps, Auditor, UserProvider};
pub use custom_query::{
    substitute_named_params, transpile_jpql, BoundQuery, CustomQuery, CustomQueryError, QueryShape,
};
pub use dialect::{MySqlDialect, PostgresDialect, SqlDialect, SqliteDialect};
pub use filter::{Direction, Filter, Op, Predicate, Sort};
pub use mapper::{MapError, Mapper, Mapping, Projection};
pub use page::Page;
pub use pageable::{Order, Pageable, PageableError, RequestSort, UNPAGED_SIZE};
pub use projection::ColumnProjection;
pub use query_parser::{
    DerivedSql, FieldPredicate, OrderClause, ParsedQuery, QueryBindError, QueryMethodParser,
    QueryOperator, QueryParseError, QueryPrefix,
};
pub use reactive::{
    PostgresReactiveRepository, ReactiveCrudRepository, ReactiveMemoryRepository,
    ReactiveSortingRepository, ReactiveSpecificationRepository, RowMapper, TableConfig,
};
pub use repository::{DataError, MemoryRepository, Repository};
pub use routing::{
    is_read_only, read_only, NamedDataSources, ReadOnlyGuard, RoutingError, RoutingPolicy,
};
pub use soft_delete::{SoftDelete, SoftDeletePolicy, DEFAULT_DELETED_AT_COLUMN};
pub use specification::Specification;

/// Framework version stamp.
pub const VERSION: &str = "26.6.4";
