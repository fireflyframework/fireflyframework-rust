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

//! The `@Entity` contract and the one-call repository factory it feeds.
//!
//! A [`SqlxEntity`] describes a persistence entity the way JPA's `@Entity` /
//! `@Table` / `@Id` / `@Version` annotations do — its table, id column, the
//! column list, an optional optimistic-lock version column, and how a row
//! reads/writes. [`repository_for`] turns that description into a fully-wired
//! [`SqlxReactiveRepository`] (table config + row mapping + `@Version` locking +
//! auditing), so a `#[derive(SqlxRepository)]` bean can be built from nothing
//! but an injected [`Db`] — the Rust analog of Spring Data handing you a
//! repository implementation for a declared entity.

use firefly_data::{Auditor, TableConfig};
use firefly_kernel::FireflyError;

use crate::db::Db;
use crate::repository::{SqlKey, SqlxReactiveRepository};
use crate::row::AnyRow;
use crate::writer::ColumnValue;

/// A persistence entity — the `@Entity` contract a repository is built from.
///
/// Implement it by hand for full control, or derive it with
/// `#[derive(Entity)]`. The associated [`Id`](SqlxEntity::Id) is the primary-key
/// type (any [`SqlKey`]); [`read_row`](SqlxEntity::read_row) /
/// [`write_row`](SqlxEntity::write_row) are the `RowMapper` / `RowWriter`
/// (`@Column` mapping).
pub trait SqlxEntity: Send + Sync + Sized + 'static {
    /// The primary-key type (Spring Data's `ID`).
    type Id: SqlKey + Clone;

    /// The table name (`@Table`).
    fn table() -> &'static str;

    /// The primary-key column (`@Id`).
    fn id_column() -> &'static str;

    /// Every persisted column, in a stable order.
    fn columns() -> &'static [&'static str];

    /// The optimistic-lock version column (`@Version`), if any. Default: none.
    fn version_column() -> Option<&'static str> {
        None
    }

    /// Decodes one row into the entity (the `RowMapper`).
    ///
    /// # Errors
    /// Returns a [`FireflyError`] if a column is missing or fails to decode.
    fn read_row(row: &AnyRow<'_>) -> Result<Self, FireflyError>;

    /// Flattens the entity into its `(column, value)` write set (the `RowWriter`).
    fn write_row(&self) -> Vec<ColumnValue>;
}

/// Builds a [`SqlxReactiveRepository`] for the entity `E` over an open [`Db`].
///
/// Wires, from the entity's [`SqlxEntity`] description: the [`TableConfig`], the
/// row mapper/writer, `@Version` optimistic locking (when
/// [`version_column`](SqlxEntity::version_column) is set), and an [`Auditor`]
/// for `@CreatedDate` / `@LastModifiedDate` (auto-enabled when the entity has a
/// `created_at` or `updated_at` column). This is the one call a
/// `#[derive(SqlxRepository)]` bean makes from its injected `Db`.
#[must_use]
pub fn repository_for<E: SqlxEntity>(db: Db) -> SqlxReactiveRepository<E, E::Id> {
    let columns = E::columns();
    let config = TableConfig::new(E::table(), E::id_column(), columns.iter().copied());
    let mut repo = SqlxReactiveRepository::new(db, config, E::read_row, E::write_row);
    if let Some(version) = E::version_column() {
        repo = repo.with_version_column(version);
    }
    if columns.contains(&"created_at") || columns.contains(&"updated_at") {
        repo = repo.with_auditor(Auditor::new());
    }
    repo
}
