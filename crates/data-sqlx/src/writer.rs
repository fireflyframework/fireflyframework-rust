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

//! The write side: turning an entity `T` into the `(column, value)` pairs a
//! dialect-aware `INSERT … UPSERT` is built from.
//!
//! Reads can be fully generic (a [`SqlxRowMapper`](crate::SqlxRowMapper)
//! decodes any backend's rows by column name), but a generic write needs to
//! know *which* columns a `T` carries and *what* to bind for each — Rust has
//! no reflection. A [`RowWriter`] supplies exactly that: an ordered list of
//! `(column_name, value)` pairs, where each value is a [`serde_json::Value`]
//! bound through the same scalar-binding path the filter arguments use.
//!
//! The repository then assembles the column list and placeholders for the
//! backend's `UPSERT` flavour itself, so the same `RowWriter` drives a
//! Postgres `ON CONFLICT`, a SQLite `ON CONFLICT`, *and* a MySQL
//! `ON DUPLICATE KEY UPDATE` without the caller writing any SQL.

use serde_json::Value;

/// One column's name and bound value for a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnValue {
    /// The column name (rendered quoted by the dialect; never interpolated
    /// raw into SQL).
    pub column: String,
    /// The bound value, bound as a scalar parameter (`$n` / `?`).
    pub value: Value,
}

impl ColumnValue {
    /// Builds a `(column, value)` pair.
    pub fn new(column: impl Into<String>, value: impl Into<Value>) -> Self {
        ColumnValue {
            column: column.into(),
            value: value.into(),
        }
    }
}

/// Extracts the ordered `(column, value)` pairs an entity persists as — the
/// write-side analogue of [`SqlxRowMapper`](crate::SqlxRowMapper).
///
/// Implement it directly, or pass a closure (the trait is blanket
/// implemented for any `Fn(&T) -> Vec<ColumnValue>`). The first column the
/// writer emits is conventionally the primary key (so the repository can
/// upsert on it), but the repository keys conflict resolution off the
/// configured [`TableConfig::id_column`](crate::TableConfig), not position,
/// so any ordering is accepted.
///
/// A writer is `Send + Sync` so the repository can be shared across tasks.
pub trait RowWriter<T>: Send + Sync {
    /// Returns the entity's columns and bound values, in column order.
    fn columns(&self, entity: &T) -> Vec<ColumnValue>;

    /// Returns the entity's columns with audit stamping applied — the hook
    /// the repository calls on **every write** when an
    /// [`Auditor`](firefly_data::Auditor) is configured, so audit columns
    /// (`created_at` / `updated_at` / `created_by` / `updated_by`) are
    /// populated automatically.
    ///
    /// `is_insert` distinguishes the first persist (stamp all four columns)
    /// from a later update (move only the modification columns). The default
    /// implementation appends the four standard audit columns derived from a
    /// fresh [`AuditStamps`](firefly_data::AuditStamps) the auditor stamps —
    /// for entities whose table carries the conventional audit columns.
    /// Override it to stamp differently or to skip auditing for a table that
    /// has no audit columns (in which case return [`RowWriter::columns`]).
    ///
    /// When `auditor` is `None`, no stamping happens and this is exactly
    /// [`RowWriter::columns`].
    fn columns_audited(
        &self,
        entity: &T,
        auditor: Option<&firefly_data::Auditor>,
        is_insert: bool,
    ) -> Vec<ColumnValue> {
        let mut cols = self.columns(entity);
        if let Some(auditor) = auditor {
            apply_audit_columns(&mut cols, auditor, is_insert);
        }
        cols
    }
}

/// Auto-stamps the four standard audit columns onto `cols`, replacing any
/// the writer already emitted so the repository's auditor is authoritative.
///
/// On insert all four (`created_at`, `updated_at`, `created_by`,
/// `updated_by`) are set; on update only `updated_at` / `updated_by` move,
/// and `created_*` are left as the writer emitted them (so an update path
/// that re-sends the original creation stamps preserves them).
fn apply_audit_columns(
    cols: &mut Vec<ColumnValue>,
    auditor: &firefly_data::Auditor,
    is_insert: bool,
) {
    use firefly_data::AuditStamps;
    let mut stamps = AuditStamps::new();
    if is_insert {
        auditor.stamp_insert(&mut stamps);
    } else {
        auditor.stamp_update(&mut stamps);
    }
    let set = |cols: &mut Vec<ColumnValue>, column: &str, value: Value| {
        if let Some(existing) = cols.iter_mut().find(|c| c.column == column) {
            existing.value = value;
        } else {
            cols.push(ColumnValue {
                column: column.to_string(),
                value,
            });
        }
    };
    if is_insert {
        if let Some(ts) = stamps.created_at {
            set(cols, "created_at", crate::binding::timestamp_value(ts));
        }
        if let Some(u) = &stamps.created_by {
            set(cols, "created_by", Value::String(u.clone()));
        }
    }
    if let Some(ts) = stamps.updated_at {
        set(cols, "updated_at", crate::binding::timestamp_value(ts));
    }
    if let Some(u) = &stamps.updated_by {
        set(cols, "updated_by", Value::String(u.clone()));
    }
}

impl<T, F> RowWriter<T> for F
where
    F: Fn(&T) -> Vec<ColumnValue> + Send + Sync,
{
    fn columns(&self, entity: &T) -> Vec<ColumnValue> {
        self(entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn closure_writer_returns_columns() {
        struct User {
            id: String,
            name: String,
        }
        let w = |u: &User| {
            vec![
                ColumnValue::new("id", u.id.clone()),
                ColumnValue::new("name", u.name.clone()),
            ]
        };
        let cols = w.columns(&User {
            id: "u1".into(),
            name: "alice".into(),
        });
        assert_eq!(
            cols,
            vec![
                ColumnValue::new("id", json!("u1")),
                ColumnValue::new("name", json!("alice")),
            ]
        );
    }
}
