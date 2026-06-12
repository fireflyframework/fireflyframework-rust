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

//! The generic filter DSL that renders to parameterised SQL.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Op is the comparison operator of a single [`Predicate`].
///
/// String values (`eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `like`, `ilike`,
/// `in`, `isnil`) match the Go port's `Op` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// Equality (`=`).
    Eq,
    /// Inequality (`<>`).
    Ne,
    /// Less than (`<`).
    Lt,
    /// Less than or equal (`<=`).
    Lte,
    /// Greater than (`>`).
    Gt,
    /// Greater than or equal (`>=`).
    Gte,
    /// Case-sensitive pattern match (`LIKE`).
    Like,
    /// Case-insensitive pattern match (`ILIKE`, PostgreSQL).
    ILike,
    /// Membership test (`= ANY($n)`, PostgreSQL array parameter).
    In,
    /// Null test (`IS NULL`); consumes no argument slot.
    IsNil,
}

impl Op {
    /// Returns the canonical string form of the operator — the same
    /// values as the Go port's `Op` string constants.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::Like => "like",
            Op::ILike => "ilike",
            Op::In => "in",
            Op::IsNil => "isnil",
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Predicate is one filter clause: column + operator + value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    /// Column name (rendered double-quoted, PostgreSQL identifier).
    pub field: String,
    /// Comparison operator.
    pub op: Op,
    /// Bound argument value; ignored (use [`Value::Null`]) for
    /// [`Op::IsNil`].
    pub value: Value,
}

impl Predicate {
    /// Constructs a predicate from column, operator, and value.
    pub fn new(field: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        Predicate {
            field: field.into(),
            op,
            value: value.into(),
        }
    }

    /// Constructs an [`Op::IsNil`] predicate — `"field" IS NULL`,
    /// consuming no argument slot.
    pub fn is_nil(field: impl Into<String>) -> Self {
        Predicate {
            field: field.into(),
            op: Op::IsNil,
            value: Value::Null,
        }
    }
}

/// Direction enumerates ascending/descending sort order.
///
/// String values (`asc`, `desc`) match the Go port's constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Ascending order.
    Asc,
    /// Descending order.
    Desc,
}

impl Direction {
    /// Returns the canonical string form (`asc` / `desc`).
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Asc => "asc",
            Direction::Desc => "desc",
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sort is a single ORDER BY clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    /// Column name (rendered double-quoted).
    pub field: String,
    /// Sort direction.
    pub direction: Direction,
}

/// Filter combines AND-joined predicates, sorts, and paging.
///
/// Builder methods consume and return `self`, so a filter chains the
/// same way the Go port's pointer-receiver builder does:
///
/// ```
/// use firefly_data::{Direction, Filter};
///
/// let f = Filter::new()
///     .where_eq("name", "alice")
///     .order_by("id", Direction::Asc)
///     .paged(0, 10);
/// let (sql, args) = f.to_sql();
/// assert_eq!(sql, r#" WHERE "name" = $1 ORDER BY "id" ASC LIMIT 10 OFFSET 0"#);
/// assert_eq!(args, vec![serde_json::json!("alice")]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// AND-joined filter clauses.
    pub predicates: Vec<Predicate>,
    /// ORDER BY clauses, applied in declaration order.
    pub sorts: Vec<Sort>,
    /// Zero-based page index.
    pub page: usize,
    /// Page size; `0` disables the LIMIT/OFFSET clause.
    pub size: usize,
}

impl Filter {
    /// Returns an empty filter: no predicates, no sorts, no paging.
    pub fn new() -> Self {
        Filter::default()
    }

    /// Appends an equality predicate (the Go port's `Where`).
    pub fn where_eq(self, field: impl Into<String>, value: impl Into<Value>) -> Self {
        self.add(Predicate::new(field, Op::Eq, value))
    }

    /// Appends an arbitrary predicate.
    // Named after the Go port's `Filter.Add`; it is a builder step, not
    // arithmetic, so the std::ops::Add lint does not apply.
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, p: Predicate) -> Self {
        self.predicates.push(p);
        self
    }

    /// Appends a sort clause.
    pub fn order_by(mut self, field: impl Into<String>, direction: Direction) -> Self {
        self.sorts.push(Sort {
            field: field.into(),
            direction,
        });
        self
    }

    /// Sets the paging window.
    pub fn paged(mut self, page: usize, size: usize) -> Self {
        self.page = page;
        self.size = size;
        self
    }

    /// Renders the filter into a parameterised SQL fragment plus its
    /// args, suitable for appending to a base `SELECT ... FROM t` query.
    /// Field names are quoted with double quotes (PostgreSQL identifier)
    /// and argument placeholders use `$1`, `$2`, … — [`Op::IsNil`]
    /// renders `IS NULL` and skips its argument slot.
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        let mut parts: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::with_capacity(self.predicates.len());
        let mut idx = 1usize;
        for p in &self.predicates {
            let clause = match p.op {
                Op::Eq => format!(r#""{}" = ${idx}"#, p.field),
                Op::Ne => format!(r#""{}" <> ${idx}"#, p.field),
                Op::Lt => format!(r#""{}" < ${idx}"#, p.field),
                Op::Lte => format!(r#""{}" <= ${idx}"#, p.field),
                Op::Gt => format!(r#""{}" > ${idx}"#, p.field),
                Op::Gte => format!(r#""{}" >= ${idx}"#, p.field),
                Op::Like => format!(r#""{}" LIKE ${idx}"#, p.field),
                Op::ILike => format!(r#""{}" ILIKE ${idx}"#, p.field),
                Op::In => format!(r#""{}" = ANY(${idx})"#, p.field),
                Op::IsNil => format!(r#""{}" IS NULL"#, p.field),
            };
            parts.push(clause);
            if p.op != Op::IsNil {
                args.push(p.value.clone());
                idx += 1;
            }
        }

        let mut sql = String::new();
        if !parts.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&parts.join(" AND "));
        }
        if !self.sorts.is_empty() {
            sql.push_str(" ORDER BY ");
            let ss: Vec<String> = self
                .sorts
                .iter()
                .map(|s| {
                    let d = match s.direction {
                        Direction::Asc => "ASC",
                        Direction::Desc => "DESC",
                    };
                    format!(r#""{}" {d}"#, s.field)
                })
                .collect();
            sql.push_str(&ss.join(", "));
        }
        if self.size > 0 {
            sql.push_str(&format!(
                " LIMIT {} OFFSET {}",
                self.size,
                self.page * self.size
            ));
        }
        (sql, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Port of Go `TestFilterToSQL`.
    #[test]
    fn test_filter_to_sql() {
        let f = Filter::new()
            .where_eq("id", 42)
            .add(Predicate::new("name", Op::ILike, "%a%"))
            .order_by("created_at", Direction::Desc)
            .paged(2, 10);
        let (q, args) = f.to_sql();
        assert!(q.contains(r#""id" = $1"#), "predicates: {q}");
        assert!(q.contains(r#""name" ILIKE $2"#), "predicates: {q}");
        assert!(q.contains(r#"ORDER BY "created_at" DESC"#), "sort: {q}");
        assert!(q.contains("LIMIT 10 OFFSET 20"), "paging: {q}");
        assert_eq!(args.len(), 2, "args: {args:?}");
        assert_eq!(args[0], json!(42), "args: {args:?}");
        // Rust-specific: the exact rendered fragment, including the
        // leading space, must match the Go port byte for byte.
        assert_eq!(
            q,
            r#" WHERE "id" = $1 AND "name" ILIKE $2 ORDER BY "created_at" DESC LIMIT 10 OFFSET 20"#
        );
    }

    /// Port of Go `TestFilterIsNil`.
    #[test]
    fn test_filter_is_nil() {
        let f = Filter::new()
            .add(Predicate::is_nil("deleted_at"))
            .where_eq("id", 1);
        let (q, args) = f.to_sql();
        assert!(q.contains(r#""deleted_at" IS NULL"#), "isnil: {q}");
        assert!(q.contains(r#""id" = $1"#), "idx after isnil: {q}");
        assert_eq!(args.len(), 1, "args: {args:?}");
    }

    /// Every operator renders its SQL form and consumes (or skips) an
    /// argument slot correctly.
    #[test]
    fn test_every_op_renders() {
        let cases: Vec<(Op, Value, &str, usize)> = vec![
            (Op::Eq, json!(1), r#""f" = $1"#, 1),
            (Op::Ne, json!(1), r#""f" <> $1"#, 1),
            (Op::Lt, json!(1), r#""f" < $1"#, 1),
            (Op::Lte, json!(1), r#""f" <= $1"#, 1),
            (Op::Gt, json!(1), r#""f" > $1"#, 1),
            (Op::Gte, json!(1), r#""f" >= $1"#, 1),
            (Op::Like, json!("%a%"), r#""f" LIKE $1"#, 1),
            (Op::ILike, json!("%a%"), r#""f" ILIKE $1"#, 1),
            (Op::In, json!([1, 2, 3]), r#""f" = ANY($1)"#, 1),
            (Op::IsNil, Value::Null, r#""f" IS NULL"#, 0),
        ];
        for (op, value, want, want_args) in cases {
            let f = Filter::new().add(Predicate::new("f", op, value));
            let (q, args) = f.to_sql();
            assert_eq!(q, format!(" WHERE {want}"), "op {op}");
            assert_eq!(args.len(), want_args, "op {op} args: {args:?}");
        }
    }

    #[test]
    fn test_empty_filter_renders_nothing() {
        let (q, args) = Filter::new().to_sql();
        assert_eq!(q, "");
        assert!(args.is_empty());
    }

    #[test]
    fn test_multiple_sorts_joined_with_comma() {
        let f = Filter::new()
            .order_by("a", Direction::Asc)
            .order_by("b", Direction::Desc);
        let (q, args) = f.to_sql();
        assert_eq!(q, r#" ORDER BY "a" ASC, "b" DESC"#);
        assert!(args.is_empty());
    }

    #[test]
    fn test_zero_size_disables_limit() {
        let f = Filter::new().where_eq("id", 7).paged(3, 0);
        let (q, _) = f.to_sql();
        assert!(!q.contains("LIMIT"), "no LIMIT expected: {q}");
        assert!(!q.contains("OFFSET"), "no OFFSET expected: {q}");
    }

    /// Op/Direction string values are the Go port's constants.
    #[test]
    fn test_op_and_direction_string_values() {
        let ops = [
            (Op::Eq, "eq"),
            (Op::Ne, "ne"),
            (Op::Lt, "lt"),
            (Op::Lte, "lte"),
            (Op::Gt, "gt"),
            (Op::Gte, "gte"),
            (Op::Like, "like"),
            (Op::ILike, "ilike"),
            (Op::In, "in"),
            (Op::IsNil, "isnil"),
        ];
        for (op, want) in ops {
            assert_eq!(op.as_str(), want);
            assert_eq!(op.to_string(), want);
            assert_eq!(serde_json::to_string(&op).unwrap(), format!("\"{want}\""));
            let back: Op = serde_json::from_str(&format!("\"{want}\"")).unwrap();
            assert_eq!(back, op);
        }
        assert_eq!(Direction::Asc.as_str(), "asc");
        assert_eq!(Direction::Desc.as_str(), "desc");
        assert_eq!(serde_json::to_string(&Direction::Asc).unwrap(), "\"asc\"");
        assert_eq!(
            serde_json::from_str::<Direction>("\"desc\"").unwrap(),
            Direction::Desc
        );
    }
}
