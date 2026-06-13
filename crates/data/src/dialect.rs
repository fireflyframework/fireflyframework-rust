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

//! SQL dialect abstraction so the storage-agnostic
//! [`Filter`](crate::Filter) / [`Specification`](crate::Specification)
//! DSL can render correct SQL for PostgreSQL, MySQL, **and** SQLite —
//! without baking any one vendor's syntax into the core tree.
//!
//! This is the Rust analogue of pyfly's `QueryMethodCompilerPort`: pyfly
//! factors compilation behind a port so each relational adapter
//! (SQLAlchemy across pg/mysql/sqlite) lowers a backend-neutral
//! `ParsedQuery` / `Specification` its own way. In Rust the lowering
//! lives next to the DSL, but the *vendor-specific* bits — placeholder
//! syntax, identifier quoting, `IN`-list rendering, and case-insensitive
//! `LIKE` — are pushed behind the [`SqlDialect`] trait so a single
//! `Filter` / `Specification` tree renders correctly for every backend.
//!
//! The four backends differ in exactly four places:
//!
//! | concern | PostgreSQL | MySQL | SQLite |
//! |---|---|---|---|
//! | placeholder | `$1`, `$2`, … | `?` | `?` |
//! | identifier quote | `"ident"` | `` `ident` `` | `"ident"` |
//! | `IN` list | `= ANY($n)` (one array param) | `IN (?, ?, …)` | `IN (?, ?, …)` |
//! | case-insensitive LIKE | `field ILIKE $n` | `LOWER(field) LIKE LOWER(?)` | `LOWER(field) LIKE LOWER(?)` |
//!
//! Everything else (the relational operators `=`, `<>`, `<`, `<=`, `>`,
//! `>=`, `LIKE`, `IS NULL`) is standard SQL shared by all three, so the
//! trait stays small.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Filter, MySqlDialect, PostgresDialect, SqliteDialect};
//!
//! let f = Filter::new().where_eq("name", "alice");
//!
//! // Postgres default — back-compatible with `to_sql()`.
//! let (sql, _) = f.to_sql_with(&PostgresDialect);
//! assert_eq!(sql, r#" WHERE "name" = $1"#);
//!
//! // MySQL — `?` placeholders, backtick identifiers.
//! let (sql, _) = f.to_sql_with(&MySqlDialect);
//! assert_eq!(sql, " WHERE `name` = ?");
//!
//! // SQLite — `?` placeholders, double-quote identifiers.
//! let (sql, _) = f.to_sql_with(&SqliteDialect);
//! assert_eq!(sql, r#" WHERE "name" = ?"#);
//! ```

/// A SQL dialect: the four vendor-specific rendering decisions that
/// separate PostgreSQL, MySQL, and SQLite.
///
/// Implement this trait to teach the [`Filter`](crate::Filter) /
/// [`Specification`](crate::Specification) renderers about a new
/// relational backend, then call
/// [`Filter::to_sql_with`](crate::Filter::to_sql_with) /
/// [`Specification::to_sql_with`](crate::Specification::to_sql_with) with
/// it. The three shipped impls — [`PostgresDialect`], [`MySqlDialect`],
/// [`SqliteDialect`] — are zero-sized, so passing `&PostgresDialect`
/// costs nothing.
///
/// The trait deliberately works over *positions* (a 1-based argument
/// index) and pre-quoted field strings so the core renderer keeps a
/// single, dialect-independent argument-numbering walk: it asks the
/// dialect only how to *spell* each piece.
pub trait SqlDialect {
    /// Renders the placeholder for the `n`-th bound argument (1-based).
    ///
    /// PostgreSQL returns positional `$1`, `$2`, …; MySQL and SQLite
    /// ignore `n` and return a bare `?`.
    fn placeholder(&self, n: usize) -> String;

    /// Quotes a column / table identifier.
    ///
    /// PostgreSQL and SQLite use ANSI double quotes (`"ident"`); MySQL
    /// uses backticks (`` `ident` ``). Any embedded quote character is
    /// doubled so an identifier never breaks out of its quoting.
    fn quote_ident(&self, id: &str) -> String;

    /// Renders a membership (`IN`) test over `n_args` values starting at
    /// the 1-based placeholder index `start`, for the already-quoted
    /// `field`.
    ///
    /// PostgreSQL renders a single array parameter — `field = ANY($start)`
    /// — and so consumes exactly **one** argument slot regardless of
    /// `n_args` (the whole list is bound as one array). MySQL and SQLite
    /// have no array parameter, so they expand to
    /// `field IN (?, ?, …)` with `n_args` placeholders, consuming
    /// `n_args` slots.
    ///
    /// See [`SqlDialect::in_arg_count`] for how many argument slots the
    /// dialect actually consumes — the renderer uses it to keep its
    /// placeholder numbering in step.
    fn render_in(&self, field: &str, start: usize, n_args: usize) -> String;

    /// How many bound-argument slots [`SqlDialect::render_in`] consumes
    /// for an `IN` list of `n_args` values.
    ///
    /// PostgreSQL binds the whole list as one array parameter, so it
    /// consumes `1`; the expanded `IN (?, …)` dialects consume `n_args`.
    /// The default expanded behaviour returns `n_args`; PostgreSQL
    /// overrides it to `1`.
    fn in_arg_count(&self, n_args: usize) -> usize {
        n_args
    }

    /// How a single `IN` value is bound for this dialect.
    ///
    /// PostgreSQL keeps the list as **one** array argument (so the
    /// caller binds the original JSON array unchanged); the expanded
    /// dialects flatten the array into `n_args` scalar arguments. The
    /// renderer uses this to keep the argument vector aligned with the
    /// placeholders it emitted.
    fn expands_in_list(&self) -> bool {
        true
    }

    /// Renders a case-insensitive pattern match between the already-quoted
    /// `field_q` and the placeholder `ph`.
    ///
    /// PostgreSQL has a native `ILIKE` operator (`field ILIKE $n`); MySQL
    /// and SQLite have none, so they lower to
    /// `LOWER(field) LIKE LOWER(?)` — which is portable and case-folds
    /// both sides.
    fn ilike(&self, field_q: &str, ph: &str) -> String;
}

/// The PostgreSQL dialect: `$n` placeholders, `"ident"` quoting,
/// `= ANY($n)` membership (one array parameter), and native `ILIKE`.
///
/// This reproduces the syntax the original `Filter::to_sql` /
/// `Specification::to_sql` hard-coded, so it is the **default** dialect:
/// `to_sql()` is exactly `to_sql_with(&PostgresDialect)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostgresDialect;

impl SqlDialect for PostgresDialect {
    fn placeholder(&self, n: usize) -> String {
        format!("${n}")
    }

    fn quote_ident(&self, id: &str) -> String {
        format!("\"{}\"", id.replace('"', "\"\""))
    }

    fn render_in(&self, field: &str, start: usize, _n_args: usize) -> String {
        format!("{field} = ANY({})", self.placeholder(start))
    }

    fn in_arg_count(&self, _n_args: usize) -> usize {
        1
    }

    fn expands_in_list(&self) -> bool {
        false
    }

    fn ilike(&self, field_q: &str, ph: &str) -> String {
        format!("{field_q} ILIKE {ph}")
    }
}

/// The MySQL dialect: `?` placeholders, `` `ident` `` backtick quoting,
/// expanded `IN (?, ?, …)` membership, and `LOWER(field) LIKE LOWER(?)`
/// for case-insensitive matching (MySQL has no `ILIKE`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MySqlDialect;

impl SqlDialect for MySqlDialect {
    fn placeholder(&self, _n: usize) -> String {
        "?".to_string()
    }

    fn quote_ident(&self, id: &str) -> String {
        format!("`{}`", id.replace('`', "``"))
    }

    fn render_in(&self, field: &str, start: usize, n_args: usize) -> String {
        render_in_expanded(self, field, start, n_args)
    }

    fn ilike(&self, field_q: &str, ph: &str) -> String {
        format!("LOWER({field_q}) LIKE LOWER({ph})")
    }
}

/// The SQLite dialect: `?` placeholders, `"ident"` double-quote quoting,
/// expanded `IN (?, ?, …)` membership, and `LOWER(field) LIKE LOWER(?)`
/// for case-insensitive matching.
///
/// SQLite's `LIKE` is ASCII-case-insensitive by default, but only for
/// ASCII; lowering through `LOWER(...)` on both sides matches the MySQL
/// behaviour and keeps the rendered SQL identical across the two
/// `?`-placeholder backends (only the identifier quoting differs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SqliteDialect;

impl SqlDialect for SqliteDialect {
    fn placeholder(&self, _n: usize) -> String {
        "?".to_string()
    }

    fn quote_ident(&self, id: &str) -> String {
        format!("\"{}\"", id.replace('"', "\"\""))
    }

    fn render_in(&self, field: &str, start: usize, n_args: usize) -> String {
        render_in_expanded(self, field, start, n_args)
    }

    fn ilike(&self, field_q: &str, ph: &str) -> String {
        format!("LOWER({field_q}) LIKE LOWER({ph})")
    }
}

/// Shared `IN (?, ?, …)` expansion for the non-array dialects: emit one
/// placeholder per value, numbered from `start`. An empty list renders
/// `IN ()`, which (correctly) matches nothing.
fn render_in_expanded(
    dialect: &dyn SqlDialect,
    field: &str,
    start: usize,
    n_args: usize,
) -> String {
    let phs: Vec<String> = (0..n_args)
        .map(|i| dialect.placeholder(start + i))
        .collect();
    format!("{field} IN ({})", phs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_postgres_placeholder_is_positional() {
        assert_eq!(PostgresDialect.placeholder(1), "$1");
        assert_eq!(PostgresDialect.placeholder(7), "$7");
    }

    #[test]
    fn test_mysql_and_sqlite_placeholder_is_question_mark() {
        assert_eq!(MySqlDialect.placeholder(1), "?");
        assert_eq!(MySqlDialect.placeholder(99), "?");
        assert_eq!(SqliteDialect.placeholder(1), "?");
        assert_eq!(SqliteDialect.placeholder(99), "?");
    }

    #[test]
    fn test_quote_ident_per_dialect() {
        assert_eq!(PostgresDialect.quote_ident("name"), r#""name""#);
        assert_eq!(SqliteDialect.quote_ident("name"), r#""name""#);
        assert_eq!(MySqlDialect.quote_ident("name"), "`name`");
    }

    #[test]
    fn test_quote_ident_escapes_embedded_quote() {
        // Embedded double quote is doubled for the ANSI dialects.
        assert_eq!(PostgresDialect.quote_ident(r#"a"b"#), r#""a""b""#);
        assert_eq!(SqliteDialect.quote_ident(r#"a"b"#), r#""a""b""#);
        // Embedded backtick is doubled for MySQL.
        assert_eq!(MySqlDialect.quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn test_postgres_render_in_uses_array_param() {
        let q = PostgresDialect.quote_ident("role");
        assert_eq!(PostgresDialect.render_in(&q, 3, 5), r#""role" = ANY($3)"#);
        // regardless of list length, postgres consumes one slot
        assert_eq!(PostgresDialect.in_arg_count(5), 1);
        assert!(!PostgresDialect.expands_in_list());
    }

    #[test]
    fn test_mysql_render_in_expands() {
        let q = MySqlDialect.quote_ident("role");
        assert_eq!(MySqlDialect.render_in(&q, 1, 3), "`role` IN (?, ?, ?)");
        assert_eq!(MySqlDialect.in_arg_count(3), 3);
        assert!(MySqlDialect.expands_in_list());
    }

    #[test]
    fn test_sqlite_render_in_expands() {
        let q = SqliteDialect.quote_ident("role");
        assert_eq!(SqliteDialect.render_in(&q, 2, 2), r#""role" IN (?, ?)"#);
        assert_eq!(SqliteDialect.in_arg_count(2), 2);
    }

    #[test]
    fn test_render_in_empty_list() {
        let q = MySqlDialect.quote_ident("role");
        assert_eq!(MySqlDialect.render_in(&q, 1, 0), "`role` IN ()");
    }

    #[test]
    fn test_ilike_per_dialect() {
        let pgq = PostgresDialect.quote_ident("name");
        assert_eq!(
            PostgresDialect.ilike(&pgq, &PostgresDialect.placeholder(1)),
            r#""name" ILIKE $1"#
        );
        let myq = MySqlDialect.quote_ident("name");
        assert_eq!(
            MySqlDialect.ilike(&myq, &MySqlDialect.placeholder(1)),
            "LOWER(`name`) LIKE LOWER(?)"
        );
        let sqq = SqliteDialect.quote_ident("name");
        assert_eq!(
            SqliteDialect.ilike(&sqq, &SqliteDialect.placeholder(1)),
            r#"LOWER("name") LIKE LOWER(?)"#
        );
    }

    #[test]
    fn test_dialect_is_object_safe() {
        let dialects: Vec<&dyn SqlDialect> = vec![&PostgresDialect, &MySqlDialect, &SqliteDialect];
        for d in dialects {
            // every trait object can render the basic pieces
            let _ = d.placeholder(1);
            let _ = d.quote_ident("x");
        }
    }
}
