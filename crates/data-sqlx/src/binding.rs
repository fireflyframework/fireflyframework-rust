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

//! Dynamic argument binding — turning the [`serde_json::Value`] arguments
//! the [`Filter`](firefly_data::Filter) /
//! [`Specification`](firefly_data::Specification) renderers emit into bound
//! sqlx query parameters, **per backend**.
//!
//! The core DSL renders a parameterised SQL fragment plus a `Vec<Value>` of
//! bound arguments; this module binds each [`Value`] onto a concrete sqlx
//! query builder. Because sqlx's `Arguments` type is backend-specific
//! (`PgArguments` / `MySqlArguments` / `SqliteArguments`), each backend gets
//! its own `bind_*` function, but they share one mapping table:
//!
//! | JSON value                | bound SQL type                       |
//! |---------------------------|--------------------------------------|
//! | `null`                    | `NULL` (typed `Option<String>`)      |
//! | `true` / `false`          | boolean                              |
//! | integer                   | `i64`                                |
//! | float                     | `f64`                                |
//! | RFC 3339 instant string   | `DateTime<Utc>` (pg / mysql)         |
//! | other string              | `&str`                               |
//! | array (Postgres `IN`)     | a typed Postgres array (`int8[]` / `text[]`) |
//! | object                    | the JSON text (`String`)             |
//!
//! Strings that parse as a full RFC 3339 timestamp (the form the audit /
//! soft-delete stamps take) bind as a real `DateTime<Utc>` on Postgres and
//! MySQL, so `TIMESTAMP` / `TIMESTAMPTZ` columns accept them; SQLite keeps
//! them as text (its timestamp columns are `TEXT`). A `Value::Array` only
//! reaches [`bind_pg`] for the Postgres `field = ANY($n)` form (the other
//! dialects flatten the list into scalars before binding), where it is bound
//! as one typed array parameter.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// Renders a [`Value`] to the `String` form used when a non-scalar (array /
/// object) value has to be bound as text — and the textual form a `null`
/// would take if a backend needed one. Scalars never go through here.
pub(crate) fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parses a string that is an **RFC 3339 / ISO-8601 instant** (the form the
/// audit / soft-delete stamps are rendered as) into a `DateTime<Utc>`, so
/// timestamp columns bind as a real instant rather than text.
///
/// Returns `None` for any string that is not a full RFC 3339 timestamp, so a
/// plain text value (a name, an id) is never mis-bound as a date. This is the
/// bridge that lets the repository carry audit/`deleted_at` instants through
/// the [`Value`]-typed argument vector and still bind them as `TIMESTAMP` /
/// `TIMESTAMPTZ` on the database side.
pub(crate) fn as_timestamp(v: &Value) -> Option<DateTime<Utc>> {
    let s = v.as_str()?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Binds one [`Value`] onto a Postgres query, advancing it. Postgres infers
/// the parameter type from the column, so `NULL` is bound as a typed
/// `Option<String>`.
#[cfg(feature = "postgres")]
pub(crate) fn bind_pg<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match v {
        Value::Null => q.bind(Option::<String>::None),
        Value::Bool(b) => q.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(u) = n.as_u64() {
                q.bind(u as i64)
            } else {
                q.bind(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(_) if as_timestamp(v).is_some() => q.bind(as_timestamp(v).unwrap()),
        Value::String(s) => q.bind(s.as_str()),
        // A JSON array reaches `bind_pg` only for the Postgres `IN` form
        // (`field = ANY($n)`), which binds the whole list as ONE array
        // parameter. Bind it as a typed Postgres array: an all-integer list
        // as `int8[]`, otherwise the elements' text forms as `text[]` (the
        // common single-column primary-key cases).
        Value::Array(items) => {
            if items.iter().all(|i| i.is_i64() || i.is_u64()) {
                let ints: Vec<i64> = items
                    .iter()
                    .map(|i| i.as_i64().unwrap_or_default())
                    .collect();
                q.bind(ints)
            } else {
                let texts: Vec<String> = items.iter().map(value_as_text).collect();
                q.bind(texts)
            }
        }
        other => q.bind(value_as_text(other)),
    }
}

/// Binds one [`Value`] onto a MySQL query, advancing it.
#[cfg(feature = "mysql")]
pub(crate) fn bind_mysql<'q>(
    q: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    match v {
        Value::Null => q.bind(Option::<String>::None),
        Value::Bool(b) => q.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(u) = n.as_u64() {
                q.bind(u as i64)
            } else {
                q.bind(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(_) if as_timestamp(v).is_some() => q.bind(as_timestamp(v).unwrap()),
        Value::String(s) => q.bind(s.as_str()),
        other => q.bind(value_as_text(other)),
    }
}

/// Binds one [`Value`] onto a SQLite query, advancing it.
#[cfg(feature = "sqlite")]
pub(crate) fn bind_sqlite<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match v {
        Value::Null => q.bind(Option::<String>::None),
        Value::Bool(b) => q.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                q.bind(i)
            } else if let Some(u) = n.as_u64() {
                q.bind(u as i64)
            } else {
                q.bind(n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => q.bind(s.as_str()),
        other => q.bind(value_as_text(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn value_as_text_renders_scalars_and_json() {
        assert_eq!(value_as_text(&json!("alice")), "alice");
        assert_eq!(value_as_text(&json!(42)), "42");
        assert_eq!(value_as_text(&json!([1, 2])), "[1,2]");
        assert_eq!(value_as_text(&json!({"a": 1})), r#"{"a":1}"#);
        assert_eq!(value_as_text(&json!(null)), "null");
    }

    #[test]
    fn as_timestamp_parses_rfc3339_and_rejects_plain_strings() {
        // A full RFC 3339 instant parses to a UTC DateTime.
        let ts = as_timestamp(&json!("2026-06-13T10:30:00+00:00"));
        assert!(ts.is_some(), "rfc3339 should parse");
        // A plain string (a name, an id, a date-only) does not.
        assert!(as_timestamp(&json!("alice")).is_none());
        assert!(as_timestamp(&json!("2026-06-13")).is_none());
        // Non-strings are never timestamps.
        assert!(as_timestamp(&json!(42)).is_none());
        assert!(as_timestamp(&json!(null)).is_none());
    }
}
