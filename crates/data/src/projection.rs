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

//! **DB-level interface projections** — the Rust port of pyfly's
//! `data.projection` (`@projection` / `is_projection` / `projection_fields`)
//! as consumed by the query compilers' `_compile_find`.
//!
//! pyfly's `@projection` marks a `Protocol` declaring a *subset* of an
//! entity's fields; the query compiler then emits `SELECT col1, col2` (only
//! the projection's columns) and returns lightweight projected rows
//! (`SimpleNamespace`) instead of full entities. Where the existing
//! [`Mapper::project`](crate::Mapper::project) projects an
//! *already-fetched* full entity (object→object), this type drives the
//! **narrowing of the SELECT list** so only the projected columns cross the
//! wire — the missing DB-level half pyfly's compiler implements.
//!
//! A [`ColumnProjection`] is just an ordered set of column names plus its
//! projection-type name (for diagnostics). Relational adapters render it as
//! the `SELECT` column list and decode each row into a JSON object keyed by
//! the projected columns; document adapters render it as a Mongo projection
//! document (`{col: 1, …}`).
//!
//! # Quick start
//!
//! ```
//! use firefly_data::ColumnProjection;
//! use serde_json::json;
//!
//! let proj = ColumnProjection::new("OrderSummary", ["id", "status", "total"]);
//! assert_eq!(proj.columns(), ["id", "status", "total"]);
//! // Mongo projection document.
//! assert_eq!(proj.to_mongo(), json!({ "id": 1, "status": 1, "total": 1 }));
//! ```

use serde::Serialize;
use serde_json::{Map, Value};

/// A DB-level column-subset projection — the Rust analogue of a pyfly
/// `@projection` Protocol's [`projection_fields`](Self::columns).
///
/// Holds the projection type's name (for diagnostics / parity with pyfly's
/// `proj_type.__name__`) and the ordered list of columns it declares. The
/// columns drive a narrowed `SELECT` list (relational) or a projection
/// document (document store), and the decoded rows are projected JSON
/// objects rather than full entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnProjection {
    name: String,
    columns: Vec<String>,
}

impl ColumnProjection {
    /// Builds a projection named `name` over the ordered `columns`.
    ///
    /// `name` is informational (it mirrors pyfly's projection class name);
    /// `columns` is the subset of entity fields the projection selects.
    pub fn new<I, S>(name: impl Into<String>, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ColumnProjection {
            name: name.into(),
            columns: columns.into_iter().map(Into::into).collect(),
        }
    }

    /// The projection type's name (informational).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The ordered projected column names — pyfly's `projection_fields`.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Whether the projection declares no columns (a degenerate projection
    /// that selects nothing; adapters should treat it as "select the full
    /// entity").
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Renders the projected columns, quoted by `quote`, into a SQL
    /// `SELECT` column list — e.g. `"id", "status", "total"` for the
    /// PostgreSQL quoter. The caller supplies the dialect's identifier
    /// quoter (typically `|c| dialect.quote_ident(c)`).
    pub fn select_list(&self, quote: impl Fn(&str) -> String) -> String {
        self.columns
            .iter()
            .map(|c| quote(c))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Renders the projection as a MongoDB projection document
    /// (`{col: 1, …}`) — the document-store analogue of [`select_list`](Self::select_list).
    pub fn to_mongo(&self) -> Value {
        let mut map = Map::new();
        for c in &self.columns {
            map.insert(c.clone(), Value::from(1));
        }
        Value::Object(map)
    }

    /// Projects an already-serialised row object down to just this
    /// projection's columns, in declaration order, keeping each column's
    /// value (or `null` when the row lacks it). The narrowed-row decode
    /// path: a relational adapter decodes the `SELECT col1, col2` row into a
    /// JSON object and calls this to guarantee the projected shape; a
    /// document adapter applies it after the projection-document `find`.
    pub fn project_value(&self, row: &Value) -> Value {
        let mut out = Map::new();
        for c in &self.columns {
            let v = row.get(c).cloned().unwrap_or(Value::Null);
            out.insert(c.clone(), v);
        }
        Value::Object(out)
    }

    /// Projects any `serde`-serialisable entity down to the projected
    /// columns — serialises it once and delegates to [`project_value`](Self::project_value).
    /// The in-memory parity path (mirrors pyfly returning a `SimpleNamespace`
    /// of the projected fields).
    pub fn project<T: Serialize>(&self, entity: &T) -> Value {
        let row = serde_json::to_value(entity).unwrap_or(Value::Null);
        self.project_value(&row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;

    #[derive(Serialize)]
    struct Order {
        id: String,
        status: String,
        total: f64,
        customer: String,
    }

    #[test]
    fn columns_and_name() {
        let p = ColumnProjection::new("OrderSummary", ["id", "status", "total"]);
        assert_eq!(p.name(), "OrderSummary");
        assert_eq!(p.columns(), ["id", "status", "total"]);
        assert!(!p.is_empty());
    }

    #[test]
    fn select_list_quotes_each_column() {
        let p = ColumnProjection::new("S", ["id", "status"]);
        assert_eq!(p.select_list(|c| format!("\"{c}\"")), r#""id", "status""#);
    }

    #[test]
    fn to_mongo_projection_document() {
        let p = ColumnProjection::new("S", ["id", "status"]);
        assert_eq!(p.to_mongo(), json!({ "id": 1, "status": 1 }));
    }

    #[test]
    fn project_narrows_entity_to_subset() {
        let order = Order {
            id: "o1".into(),
            status: "PAID".into(),
            total: 42.0,
            customer: "c1".into(),
        };
        let p = ColumnProjection::new("OrderSummary", ["id", "status", "total"]);
        // The full entity (customer) is dropped; only the projection remains.
        assert_eq!(
            p.project(&order),
            json!({ "id": "o1", "status": "PAID", "total": 42.0 })
        );
    }

    #[test]
    fn project_value_fills_missing_with_null() {
        let p = ColumnProjection::new("S", ["a", "b"]);
        assert_eq!(
            p.project_value(&json!({ "a": 1 })),
            json!({ "a": 1, "b": null })
        );
    }

    #[test]
    fn empty_projection_is_flagged() {
        let p = ColumnProjection::new("Empty", Vec::<String>::new());
        assert!(p.is_empty());
    }
}
