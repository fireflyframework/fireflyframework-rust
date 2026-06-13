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
//! | tagged timestamp value    | `DateTime<Utc>` (pg / mysql)         |
//! | string                    | `&str`                               |
//! | array (Postgres `IN`)     | a typed Postgres array (`int8[]` / `text[]`) |
//! | object                    | the JSON text (`String`)             |
//!
//! Only the repository's own audit / soft-delete stamps are bound as a real
//! `DateTime<Utc>` on Postgres and MySQL (so `TIMESTAMP` / `TIMESTAMPTZ`
//! columns accept them); SQLite keeps them as text (its timestamp columns are
//! `TEXT`). Those stamps are carried as a **tagged** [`Value`] —
//! [`timestamp_value`] wraps the instant in a `{ "$firefly_ts": "<rfc3339>" }`
//! object — so the coercion is driven by where the value came from, not by
//! what it happens to look like. A plain [`Value::String`] (an id, a name, a
//! status, a user-supplied filter argument) is **always** bound as `&str`,
//! even when its contents happen to parse as an ISO-8601 instant, so a text /
//! varchar column is never mis-typed. A `Value::Array` only reaches
//! [`bind_pg`] for the Postgres `field = ANY($n)` form (the other dialects
//! flatten the list into scalars before binding), where it is bound as one
//! typed array parameter.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// The JSON object key the repository tags its own timestamp stamps with so
/// the binders bind them as a real `DateTime<Utc>` (rather than text) without
/// guessing from the string contents. See [`timestamp_value`] / [`as_timestamp`].
pub(crate) const TIMESTAMP_TAG: &str = "$firefly_ts";

/// Renders a [`Value`] to the `String` form used when a non-scalar (array /
/// object) value has to be bound as text — and the textual form a `null`
/// would take if a backend needed one. Scalars never go through here.
pub(crate) fn value_as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Wraps an instant in the **tagged** [`Value`] form the repository uses to
/// carry its own audit / `deleted_at` stamps through the [`Value`]-typed
/// argument vector. The binders ([`bind_pg`] / [`bind_mysql`]) recognise this
/// tag and bind it as a real `DateTime<Utc>` so `TIMESTAMP` / `TIMESTAMPTZ`
/// columns accept it; SQLite binds the textual RFC 3339 form (its timestamp
/// columns are `TEXT`).
///
/// Crucially this is the *only* thing bound as a timestamp — a user-supplied
/// [`Value::String`] is never coerced just because it looks like an instant,
/// so a TEXT / VARCHAR column whose value happens to be ISO-8601 is bound as
/// `&str`, with no parameter-type mismatch.
pub(crate) fn timestamp_value(dt: DateTime<Utc>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(TIMESTAMP_TAG.to_string(), Value::String(dt.to_rfc3339()));
    Value::Object(obj)
}

/// The **tagged NULL timestamp** [`Value`] — a typed `NULL` for a timestamp
/// column. The repository uses it to *clear* a `deleted_at` stamp when an
/// UPSERT resurrects a previously soft-deleted row: binding it as a typed
/// `Option<DateTime<Utc>>::None` (rather than a text NULL) keeps Postgres
/// from rejecting a text expression against a `TIMESTAMPTZ` column.
pub(crate) fn timestamp_null_value() -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(TIMESTAMP_TAG.to_string(), Value::Null);
    Value::Object(obj)
}

/// Whether `v` is one of the repository's **tagged** timestamp values
/// ([`timestamp_value`] or [`timestamp_null_value`]) — i.e. a value the
/// binders must bind as a timestamp parameter rather than as text. This is
/// the only thing treated as a timestamp; a plain [`Value::String`] that
/// happens to look like an instant is always bound as text.
pub(crate) fn is_timestamp_tagged(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.contains_key(TIMESTAMP_TAG))
}

/// Recognises the **tagged** timestamp [`Value`] produced by
/// [`timestamp_value`] and parses it back into a `DateTime<Utc>`. Returns
/// `None` for everything else — including a plain [`Value::String`] that
/// happens to be a valid RFC 3339 instant (so user text is never mis-typed)
/// and the [`timestamp_null_value`] tagged NULL (which is a *typed NULL*, not
/// an instant; binders test [`is_timestamp_tagged`] to handle it).
pub(crate) fn as_timestamp(v: &Value) -> Option<DateTime<Utc>> {
    let s = v.as_object()?.get(TIMESTAMP_TAG)?.as_str()?;
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
        // A repository-tagged timestamp stamp -> a real `DateTime<Utc>`, or a
        // typed `NULL` timestamp when it carries the tagged NULL.
        Value::Object(_) if is_timestamp_tagged(v) => match as_timestamp(v) {
            Some(dt) => q.bind(dt),
            None => q.bind(Option::<DateTime<Utc>>::None),
        },
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
        // A repository-tagged timestamp stamp -> a real `DateTime<Utc>`, or a
        // typed `NULL` timestamp when it carries the tagged NULL.
        Value::Object(_) if is_timestamp_tagged(v) => match as_timestamp(v) {
            Some(dt) => q.bind(dt),
            None => q.bind(Option::<DateTime<Utc>>::None),
        },
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
        // A repository-tagged timestamp stamp -> its RFC 3339 text (SQLite's
        // timestamp columns are `TEXT`), or a typed `NULL` for the tagged NULL.
        Value::Object(_) if is_timestamp_tagged(v) => match as_timestamp(v) {
            Some(dt) => q.bind(dt.to_rfc3339()),
            None => q.bind(Option::<String>::None),
        },
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
    fn as_timestamp_only_matches_the_tagged_stamp_value() {
        use chrono::TimeZone;
        let dt = Utc.with_ymd_and_hms(2026, 6, 13, 10, 30, 0).unwrap();

        // The repository's own tagged stamp round-trips to a UTC DateTime.
        let tagged = timestamp_value(dt);
        assert_eq!(as_timestamp(&tagged), Some(dt), "tagged stamp parses");

        // A plain string that *looks* like an RFC 3339 instant is NOT treated
        // as a timestamp — it is user data bound as text (regression for the
        // value-driven mis-typing of TEXT/VARCHAR columns).
        assert!(
            as_timestamp(&json!("2026-06-13T10:30:00+00:00")).is_none(),
            "a bare rfc3339 string must not be coerced"
        );
        // Neither is any other plain string, number, or null.
        assert!(as_timestamp(&json!("alice")).is_none());
        assert!(as_timestamp(&json!("2026-06-13")).is_none());
        assert!(as_timestamp(&json!(42)).is_none());
        assert!(as_timestamp(&json!(null)).is_none());
        // An ordinary JSON object (not the tag) is not a timestamp either.
        assert!(as_timestamp(&json!({"a": 1})).is_none());
    }
}
