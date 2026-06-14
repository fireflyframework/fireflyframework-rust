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

//! Dialect-aware SQL assembly — building the `SELECT` / `UPSERT` / `DELETE`
//! / `COUNT` statements every repository method runs, for any backend.
//!
//! This module is pure string + argument construction (no I/O): it takes a
//! [`TableConfig`](firefly_data::TableConfig), a [`SqlDialect`], and (for
//! writes) the entity's [`ColumnValue`](crate::ColumnValue) pairs, and
//! returns `(sql, args)` ready to bind. The execution layer
//! ([`repository`](crate::repository)) only has to bind and run it.
//!
//! The one genuinely vendor-specific statement is the `UPSERT`: Postgres
//! and SQLite use `INSERT … ON CONFLICT(<id>) DO UPDATE SET …`, while MySQL
//! uses `INSERT … ON DUPLICATE KEY UPDATE …`. [`upsert_sql`] picks the right
//! flavour from the [`Backend`](crate::Backend).

use firefly_data::{SqlDialect, TableConfig};
use serde_json::Value;

use crate::db::Backend;
use crate::writer::ColumnValue;

/// Joins the projected columns of `cfg`, quoted for `dialect`, into a
/// `SELECT` column list.
fn quoted_columns(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    cfg.columns
        .iter()
        .map(|c| dialect.quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `SELECT <cols> FROM <table>` for `dialect`.
pub(crate) fn select_all(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    format!(
        "SELECT {} FROM {}",
        quoted_columns(cfg, dialect),
        dialect.quote_ident(&cfg.table)
    )
}

/// `SELECT <cols> FROM <table> WHERE <id> = <ph1>` for `dialect`.
pub(crate) fn select_by_id(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    format!(
        "{} WHERE {} = {}",
        select_all(cfg, dialect),
        dialect.quote_ident(&cfg.id_column),
        dialect.placeholder(1)
    )
}

/// `SELECT CASE WHEN EXISTS(SELECT 1 FROM <table> WHERE <id> = <ph1>) THEN 1
/// ELSE 0 END` for `dialect`.
///
/// The `CASE WHEN … THEN 1 ELSE 0 END` wrapper yields an **integer** `0` /
/// `1` on every backend, so the repository decodes it as `i64` uniformly —
/// Postgres's bare `EXISTS(...)` returns a true `BOOL` (which would not
/// decode as `i64`), whereas MySQL / SQLite return an integer.
pub(crate) fn exists_by_id(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    format!(
        "SELECT CASE WHEN EXISTS(SELECT 1 FROM {} WHERE {} = {}) THEN 1 ELSE 0 END",
        dialect.quote_ident(&cfg.table),
        dialect.quote_ident(&cfg.id_column),
        dialect.placeholder(1)
    )
}

/// `SELECT COUNT(*) FROM <table><where>` for `dialect`, where `where_sql` is
/// the already-rendered `" WHERE …"` fragment (possibly empty).
pub(crate) fn count_where(cfg: &TableConfig, dialect: &dyn SqlDialect, where_sql: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {}{}",
        dialect.quote_ident(&cfg.table),
        where_sql
    )
}

/// `DELETE FROM <table> WHERE <id> = <ph1>` for `dialect`.
pub(crate) fn delete_by_id(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    format!(
        "DELETE FROM {} WHERE {} = {}",
        dialect.quote_ident(&cfg.table),
        dialect.quote_ident(&cfg.id_column),
        dialect.placeholder(1)
    )
}

/// `DELETE FROM <table>` for `dialect`.
pub(crate) fn delete_all(cfg: &TableConfig, dialect: &dyn SqlDialect) -> String {
    format!("DELETE FROM {}", dialect.quote_ident(&cfg.table))
}

/// Builds a dialect-aware `UPSERT` statement plus its bound arguments from
/// the entity's `(column, value)` pairs.
///
/// The conflict target is `cfg.id_column`. Every **non-id** column is set on
/// conflict (so an existing row's id is preserved and its other columns are
/// refreshed). The flavour is chosen from `backend`:
///
/// - **Postgres / SQLite** — `INSERT INTO t (cols) VALUES (phs)
///   ON CONFLICT(<id>) DO UPDATE SET col = EXCLUDED.col, …`
/// - **MySQL** — `INSERT INTO t (cols) VALUES (phs)
///   ON DUPLICATE KEY UPDATE col = VALUES(col), …`
///
/// A `RETURNING` clause is **not** emitted (MySQL lacks it); the repository
/// re-reads the row by id after the write to return the persisted value,
/// matching the cross-backend contract.
///
/// Returns `(sql, args)` where `args` are the inserted values in column
/// order, ready to bind as scalar `$n` / `?` parameters.
pub(crate) fn upsert_sql(
    cfg: &TableConfig,
    dialect: &dyn SqlDialect,
    backend: Backend,
    cols: &[ColumnValue],
    version_column: Option<&str>,
) -> (String, Vec<Value>) {
    let table_q = dialect.quote_ident(&cfg.table);
    let id_q = dialect.quote_ident(&cfg.id_column);

    let col_idents: Vec<String> = cols
        .iter()
        .map(|c| dialect.quote_ident(&c.column))
        .collect();
    let placeholders: Vec<String> = (0..cols.len())
        .map(|i| dialect.placeholder(i + 1))
        .collect();
    let args: Vec<Value> = cols.iter().map(|c| c.value.clone()).collect();

    let insert = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_q,
        col_idents.join(", "),
        placeholders.join(", ")
    );

    // Non-id columns updated on conflict.
    let updatable: Vec<&ColumnValue> = cols.iter().filter(|c| c.column != cfg.id_column).collect();

    // Whether an updatable column is the optimistic-locking version column.
    let is_version = |col: &str| version_column == Some(col);

    let conflict = match backend {
        Backend::Postgres | Backend::Sqlite => {
            if updatable.is_empty() {
                // id-only table: nothing to update, so a no-op on conflict.
                format!(" ON CONFLICT({id_q}) DO NOTHING")
            } else {
                let sets: Vec<String> = updatable
                    .iter()
                    .map(|c| {
                        let q = dialect.quote_ident(&c.column);
                        if is_version(&c.column) {
                            // Bump the version on every conflict-update.
                            format!("{q} = EXCLUDED.{q} + 1")
                        } else {
                            format!("{q} = EXCLUDED.{q}")
                        }
                    })
                    .collect();
                let mut clause =
                    format!(" ON CONFLICT({id_q}) DO UPDATE SET {}", sets.join(", "));
                // Optimistic-lock guard: only update when the stored version
                // still matches the loaded one, so a stale write affects 0 rows.
                if let Some(vc) = version_column {
                    let vq = dialect.quote_ident(vc);
                    let target = match backend {
                        Backend::Postgres => format!("{table_q}.{vq}"),
                        // SQLite references the target column unqualified.
                        _ => vq.clone(),
                    };
                    clause.push_str(&format!(" WHERE {target} = EXCLUDED.{vq}"));
                }
                clause
            }
        }
        Backend::MySql => {
            if updatable.is_empty() {
                // MySQL has no DO NOTHING; a harmless self-assignment of the
                // id keeps the statement an idempotent upsert.
                format!(" ON DUPLICATE KEY UPDATE {id_q} = {id_q}")
            } else {
                // MySQL's ON DUPLICATE KEY UPDATE takes no WHERE clause, so the
                // version is bumped but the stale-write guard is not enforced.
                let sets: Vec<String> = updatable
                    .iter()
                    .map(|c| {
                        let q = dialect.quote_ident(&c.column);
                        if is_version(&c.column) {
                            format!("{q} = VALUES({q}) + 1")
                        } else {
                            format!("{q} = VALUES({q})")
                        }
                    })
                    .collect();
                format!(" ON DUPLICATE KEY UPDATE {}", sets.join(", "))
            }
        }
    };

    (format!("{insert}{conflict}"), args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use firefly_data::{PostgresDialect, SqliteDialect};

    fn cfg() -> TableConfig {
        TableConfig::new("users", "id", ["id", "name"])
    }

    #[test]
    fn select_all_quotes_for_postgres() {
        assert_eq!(
            select_all(&cfg(), &PostgresDialect),
            r#"SELECT "id", "name" FROM "users""#
        );
    }

    #[test]
    fn select_by_id_appends_where() {
        assert_eq!(
            select_by_id(&cfg(), &PostgresDialect),
            r#"SELECT "id", "name" FROM "users" WHERE "id" = $1"#
        );
    }

    #[test]
    fn upsert_postgres_on_conflict_do_update() {
        let cols = vec![
            ColumnValue::new("id", "u1"),
            ColumnValue::new("name", "alice"),
        ];
        let (sql, args) = upsert_sql(&cfg(), &PostgresDialect, Backend::Postgres, &cols, None);
        assert_eq!(
            sql,
            r#"INSERT INTO "users" ("id", "name") VALUES ($1, $2) ON CONFLICT("id") DO UPDATE SET "name" = EXCLUDED."name""#
        );
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn upsert_sqlite_on_conflict() {
        let cols = vec![
            ColumnValue::new("id", "u1"),
            ColumnValue::new("name", "alice"),
        ];
        let (sql, _) = upsert_sql(&cfg(), &SqliteDialect, Backend::Sqlite, &cols, None);
        assert!(
            sql.contains(r#"ON CONFLICT("id") DO UPDATE SET "name" = EXCLUDED."name""#),
            "{sql}"
        );
        assert!(sql.contains("VALUES (?, ?)"), "{sql}");
    }

    #[test]
    fn upsert_mysql_on_duplicate_key_update() {
        use firefly_data::MySqlDialect;
        let cols = vec![
            ColumnValue::new("id", "u1"),
            ColumnValue::new("name", "alice"),
        ];
        let (sql, _) = upsert_sql(&cfg(), &MySqlDialect, Backend::MySql, &cols, None);
        assert_eq!(
            sql,
            "INSERT INTO `users` (`id`, `name`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `name` = VALUES(`name`)"
        );
    }

    #[test]
    fn upsert_id_only_table_does_nothing_on_conflict() {
        let c = TableConfig::new("tags", "id", ["id"]);
        let cols = vec![ColumnValue::new("id", "t1")];
        let (sql, _) = upsert_sql(&c, &PostgresDialect, Backend::Postgres, &cols, None);
        assert!(sql.contains(r#"ON CONFLICT("id") DO NOTHING"#), "{sql}");
    }
}
