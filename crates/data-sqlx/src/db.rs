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

//! The [`Db`] handle — a sqlx connection pool tagged with its backend, plus
//! the [`SqlDialect`] selection that drives dialect-aware rendering.
//!
//! A relational repository over sqlx must pick the right SQL dialect at
//! runtime (placeholder syntax, identifier quoting, `IN`-list shape,
//! case-insensitive `LIKE`, and `UPSERT` flavour). [`Db`] makes that choice
//! explicit and central: construct it from a [`PgPool`](sqlx::PgPool),
//! [`MySqlPool`](sqlx::MySqlPool), or [`SqlitePool`](sqlx::SqlitePool), and
//! the repositories ask it for the matching dialect with
//! [`Db::dialect`]. Cloning a [`Db`] clones the underlying pool handle (an
//! `Arc`), so it is cheap and `Send + Sync`.

use firefly_data::{MySqlDialect, PostgresDialect, SqlDialect, SqliteDialect};

/// The relational backend kind, used to select the matching dialect and
/// `UPSERT` flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// PostgreSQL (`$n` placeholders, `ON CONFLICT` upsert).
    Postgres,
    /// MySQL (`?` placeholders, `ON DUPLICATE KEY UPDATE` upsert).
    MySql,
    /// SQLite (`?` placeholders, `ON CONFLICT` upsert).
    Sqlite,
}

/// A sqlx connection pool tagged with its backend.
///
/// The single handle a [`SqlxReactiveRepository`](crate::SqlxReactiveRepository)
/// / [`SqlxRepository`](crate::SqlxRepository) is built over. Clone it
/// freely — every variant holds an `Arc`-backed pool, so a clone is a cheap
/// reference-count bump and is safe to share across tasks.
#[derive(Clone)]
pub enum Db {
    /// A PostgreSQL pool.
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
    /// A MySQL pool.
    #[cfg(feature = "mysql")]
    MySql(sqlx::MySqlPool),
    /// A SQLite pool.
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
}

impl Db {
    /// Returns the backend kind of this pool.
    pub fn backend(&self) -> Backend {
        match self {
            #[cfg(feature = "postgres")]
            Db::Postgres(_) => Backend::Postgres,
            #[cfg(feature = "mysql")]
            Db::MySql(_) => Backend::MySql,
            #[cfg(feature = "sqlite")]
            Db::Sqlite(_) => Backend::Sqlite,
        }
    }

    /// Returns the [`SqlDialect`] for this backend, so the
    /// [`Filter`](firefly_data::Filter) /
    /// [`Specification`](firefly_data::Specification) renderers emit correct
    /// SQL. The returned trait object is zero-sized.
    pub fn dialect(&self) -> Box<dyn SqlDialect + Send + Sync> {
        match self.backend() {
            Backend::Postgres => Box::new(PostgresDialect),
            Backend::MySql => Box::new(MySqlDialect),
            Backend::Sqlite => Box::new(SqliteDialect),
        }
    }
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Db").field(&self.backend()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_db_reports_backend_and_dialect() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let db = Db::Sqlite(pool);
        assert_eq!(db.backend(), Backend::Sqlite);
        // SQLite dialect uses `?` placeholders.
        assert_eq!(db.dialect().placeholder(1), "?");
        assert_eq!(db.dialect().quote_ident("x"), r#""x""#);
    }
}
