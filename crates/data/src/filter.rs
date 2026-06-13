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

use crate::dialect::{PostgresDialect, SqlDialect};

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
    ///
    /// This is the **PostgreSQL default**: it is exactly
    /// `self.to_sql_with(&PostgresDialect)`. Use [`Filter::to_sql_with`]
    /// to render for MySQL or SQLite.
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        self.to_sql_with(&PostgresDialect)
    }

    /// Renders the filter into a parameterised SQL fragment plus its args
    /// for a specific [`SqlDialect`] — the storage-agnostic rendering
    /// path. Identifier quoting, placeholder syntax, `IN`-list shape, and
    /// case-insensitive `LIKE` all defer to `dialect`, so one [`Filter`]
    /// renders correct SQL for PostgreSQL (`$n` / `"id"` / `= ANY` /
    /// `ILIKE`), MySQL (`?` / `` `id` `` / `IN (?,…)` / `LOWER LIKE
    /// LOWER`), or SQLite.
    ///
    /// `IN` lists are bound differently per dialect: PostgreSQL binds the
    /// whole JSON array as a single argument, while MySQL/SQLite flatten
    /// the array into one scalar argument per element — the returned args
    /// vector always matches the placeholders the dialect emitted.
    /// [`Op::IsNil`] renders `IS NULL` and consumes no argument slot.
    pub fn to_sql_with(&self, dialect: &dyn SqlDialect) -> (String, Vec<Value>) {
        let mut parts: Vec<String> = Vec::with_capacity(self.predicates.len());
        let mut args: Vec<Value> = Vec::with_capacity(self.predicates.len());
        let mut idx = 1usize;
        for p in &self.predicates {
            parts.push(render_predicate_sql(p, dialect, &mut args, &mut idx));
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
                    format!("{} {d}", dialect.quote_ident(&s.field))
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

    /// Lowers the filter's AND-joined predicates to a MongoDB
    /// `$`-operator filter document — the document-store analogue of
    /// [`Filter::to_sql`], so the **same** filter tree drives SQL,
    /// in-memory matching, *and* a document backend.
    ///
    /// Each predicate becomes one field clause
    /// (`{field: {$eq: value}}`, `{field: {$gt: value}}`, etc.); the
    /// whole filter is the conjunction of those clauses. An empty filter
    /// lowers to `{}` (matches everything); a single predicate lowers to
    /// the bare clause; two or more lower to `{"$and": [...]}`. Sorting
    /// and paging are *not* part of the filter document — a Mongo adapter
    /// applies those via the cursor's `sort` / `skip` / `limit`; use
    /// [`Filter::mongo_sort`] for the sort spec.
    ///
    /// `LIKE` / `ILIKE` patterns lower to an **anchored** `$regex`
    /// (translating SQL `%` → `.*` and `_` → `.`, regex-escaping every
    /// other character, then wrapping the body in `^` … `$`). The anchors
    /// make the document backend a full-value match, matching SQL `LIKE`
    /// and the in-memory matcher — and pyfly's own `MongoSpecification.like`
    /// — rather than Mongo's default unanchored substring `$regex`. `ILIKE`
    /// adds the `i` option for case-insensitivity.
    pub fn to_mongo(&self) -> Value {
        let clauses: Vec<Value> = self.predicates.iter().map(predicate_to_mongo).collect();
        combine_mongo_and(clauses)
    }

    /// The MongoDB sort spec for this filter's ORDER BY clauses, as an
    /// ordered document `{field: 1 | -1, …}` (`1` ascending, `-1`
    /// descending) — the value a Mongo adapter passes to the cursor's
    /// `sort`. Returns an empty document when the filter is unsorted.
    pub fn mongo_sort(&self) -> Value {
        let mut map = serde_json::Map::new();
        for s in &self.sorts {
            let dir = match s.direction {
                Direction::Asc => 1,
                Direction::Desc => -1,
            };
            map.insert(s.field.clone(), Value::from(dir));
        }
        Value::Object(map)
    }
}

/// Renders one predicate to a dialect-specific SQL clause, pushing its
/// bound argument(s) and advancing the 1-based placeholder index. Shared
/// by [`Filter::to_sql_with`] and the [`Specification`](crate::Specification)
/// renderer so both honour the dialect identically.
pub(crate) fn render_predicate_sql(
    p: &Predicate,
    dialect: &dyn SqlDialect,
    args: &mut Vec<Value>,
    idx: &mut usize,
) -> String {
    let field_q = dialect.quote_ident(&p.field);
    match p.op {
        Op::Eq => {
            let clause = format!("{field_q} = {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Ne => {
            let clause = format!("{field_q} <> {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Lt => {
            let clause = format!("{field_q} < {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Lte => {
            let clause = format!("{field_q} <= {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Gt => {
            let clause = format!("{field_q} > {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Gte => {
            let clause = format!("{field_q} >= {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::Like => {
            let clause = format!("{field_q} LIKE {}", dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::ILike => {
            let clause = dialect.ilike(&field_q, &dialect.placeholder(*idx));
            args.push(p.value.clone());
            *idx += 1;
            clause
        }
        Op::In => {
            // How many placeholders the expanded dialects emit; the array
            // dialect (postgres) ignores it and emits a single param.
            let n_args = match &p.value {
                Value::Array(items) => items.len(),
                _ => 1,
            };
            let clause = dialect.render_in(&field_q, *idx, n_args);
            if dialect.expands_in_list() {
                // Flatten the JSON array into one scalar arg per element.
                match &p.value {
                    Value::Array(items) => {
                        for item in items {
                            args.push(item.clone());
                        }
                    }
                    other => args.push(other.clone()),
                }
            } else {
                // Bind the whole list as a single array parameter.
                args.push(p.value.clone());
            }
            *idx += dialect.in_arg_count(n_args);
            clause
        }
        Op::IsNil => format!("{field_q} IS NULL"),
    }
}

/// Lowers a single [`Predicate`] to a MongoDB `$`-operator clause.
/// Shared by [`Filter::to_mongo`] and the
/// [`Specification`](crate::Specification) document lowering.
pub(crate) fn predicate_to_mongo(p: &Predicate) -> Value {
    use serde_json::json;
    let field = p.field.as_str();
    match p.op {
        Op::Eq => json!({ field: { "$eq": p.value } }),
        Op::Ne => json!({ field: { "$ne": p.value } }),
        Op::Lt => json!({ field: { "$lt": p.value } }),
        Op::Lte => json!({ field: { "$lte": p.value } }),
        Op::Gt => json!({ field: { "$gt": p.value } }),
        Op::Gte => json!({ field: { "$gte": p.value } }),
        Op::Like => json!({ field: { "$regex": like_to_regex(&p.value) } }),
        Op::ILike => {
            json!({ field: { "$regex": like_to_regex(&p.value), "$options": "i" } })
        }
        Op::In => json!({ field: { "$in": p.value } }),
        Op::IsNil => json!({ field: { "$eq": Value::Null } }),
    }
}

/// Translates a SQL `LIKE` pattern into a MongoDB `$regex` body: `%` →
/// `.*`, `_` → `.`, every other character regex-escaped. The body is
/// anchored with `^` … `$` so the regex must match the *entire* field
/// value — exactly like SQL `LIKE` (anchored at start, terminated by the
/// pattern's end) and the in-memory `like_match_chars` full-match
/// semantics, and like pyfly's own `MongoSpecification.like` (which wraps
/// the body in `^…$`). Without the anchors a Mongo `$regex` is a
/// substring match, so `name LIKE 'A%'` would silently match `"bAr"` on
/// the document backend while excluding it on SQL/in-memory — the same
/// Specification must yield the same rows on every backend. A non-string
/// pattern is rendered as its plain JSON string form (it never matches a
/// wildcard).
fn like_to_regex(pattern: &Value) -> String {
    let text = match pattern {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut out = String::with_capacity(text.len() * 2 + 2);
    out.push('^');
    for ch in text.chars() {
        match ch {
            '%' => out.push_str(".*"),
            '_' => out.push('.'),
            // Regex metacharacters that must be escaped to match literally.
            '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out.push('$');
    out
}

/// Combines MongoDB clauses under `$and`: `[]` → `{}`, one clause → bare,
/// many → `{"$and": [...]}`. Shared by [`Filter::to_mongo`] and the
/// conjunction case of [`Specification`](crate::Specification) lowering.
pub(crate) fn combine_mongo_and(mut clauses: Vec<Value>) -> Value {
    match clauses.len() {
        0 => Value::Object(serde_json::Map::new()),
        1 => clauses.pop().unwrap(),
        _ => serde_json::json!({ "$and": clauses }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{MySqlDialect, SqliteDialect};
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

    // ===== Dialect-aware rendering (to_sql_with) ======================

    /// `to_sql()` is exactly `to_sql_with(&PostgresDialect)`.
    #[test]
    fn test_to_sql_equals_postgres_default() {
        let f = Filter::new()
            .where_eq("id", 42)
            .add(Predicate::new("name", Op::ILike, "%a%"))
            .order_by("created_at", Direction::Desc)
            .paged(2, 10);
        assert_eq!(f.to_sql(), f.to_sql_with(&PostgresDialect));
    }

    #[test]
    fn test_mysql_dialect_placeholders_and_backticks() {
        let f = Filter::new().where_eq("id", 42).where_eq("name", "alice");
        let (q, args) = f.to_sql_with(&MySqlDialect);
        assert_eq!(q, " WHERE `id` = ? AND `name` = ?");
        assert_eq!(args, vec![json!(42), json!("alice")]);
    }

    #[test]
    fn test_sqlite_dialect_placeholders_and_double_quotes() {
        let f = Filter::new().where_eq("id", 42).where_eq("name", "alice");
        let (q, args) = f.to_sql_with(&SqliteDialect);
        assert_eq!(q, r#" WHERE "id" = ? AND "name" = ?"#);
        assert_eq!(args, vec![json!(42), json!("alice")]);
    }

    #[test]
    fn test_in_renders_array_param_for_postgres_one_arg() {
        let f = Filter::new().add(Predicate::new("role", Op::In, json!(["a", "b", "c"])));
        let (q, args) = f.to_sql_with(&PostgresDialect);
        assert_eq!(q, r#" WHERE "role" = ANY($1)"#);
        // postgres binds the whole list as ONE array argument
        assert_eq!(args, vec![json!(["a", "b", "c"])]);
    }

    #[test]
    fn test_in_expands_for_mysql_and_renumbers_following_placeholders() {
        let f = Filter::new()
            .add(Predicate::new("role", Op::In, json!(["a", "b", "c"])))
            .where_eq("active", true);
        let (q, args) = f.to_sql_with(&MySqlDialect);
        assert_eq!(q, " WHERE `role` IN (?, ?, ?) AND `active` = ?");
        // the array is flattened into one scalar arg per element, then `active`
        assert_eq!(args, vec![json!("a"), json!("b"), json!("c"), json!(true)]);
    }

    #[test]
    fn test_in_expands_for_sqlite() {
        let f = Filter::new().add(Predicate::new("id", Op::In, json!([1, 2])));
        let (q, args) = f.to_sql_with(&SqliteDialect);
        assert_eq!(q, r#" WHERE "id" IN (?, ?)"#);
        assert_eq!(args, vec![json!(1), json!(2)]);
    }

    #[test]
    fn test_in_then_next_placeholder_numbering_postgres() {
        // After an IN (one array slot for postgres) the next placeholder is $2.
        let f = Filter::new()
            .add(Predicate::new("role", Op::In, json!(["a", "b"])))
            .where_eq("active", true);
        let (q, args) = f.to_sql_with(&PostgresDialect);
        assert_eq!(q, r#" WHERE "role" = ANY($1) AND "active" = $2"#);
        assert_eq!(args, vec![json!(["a", "b"]), json!(true)]);
    }

    #[test]
    fn test_ilike_lowers_to_lower_like_lower_for_mysql_sqlite() {
        let f = Filter::new().add(Predicate::new("name", Op::ILike, "%a%"));
        let (mq, _) = f.to_sql_with(&MySqlDialect);
        assert_eq!(mq, " WHERE LOWER(`name`) LIKE LOWER(?)");
        let (sq, _) = f.to_sql_with(&SqliteDialect);
        assert_eq!(sq, r#" WHERE LOWER("name") LIKE LOWER(?)"#);
    }

    #[test]
    fn test_isnil_consumes_no_arg_across_dialects() {
        let f = Filter::new()
            .add(Predicate::is_nil("deleted_at"))
            .where_eq("id", 1);
        for (sql, _) in [
            f.to_sql_with(&PostgresDialect),
            f.to_sql_with(&MySqlDialect),
            f.to_sql_with(&SqliteDialect),
        ] {
            assert!(sql.contains("IS NULL"), "isnil: {sql}");
        }
        // mysql: the only placeholder is for `id`, still `?`
        let (mq, margs) = f.to_sql_with(&MySqlDialect);
        assert_eq!(mq, " WHERE `deleted_at` IS NULL AND `id` = ?");
        assert_eq!(margs, vec![json!(1)]);
    }

    #[test]
    fn test_dialect_sort_quoting() {
        let f = Filter::new().order_by("created_at", Direction::Desc);
        assert_eq!(
            f.to_sql_with(&MySqlDialect).0,
            " ORDER BY `created_at` DESC"
        );
        assert_eq!(
            f.to_sql_with(&SqliteDialect).0,
            r#" ORDER BY "created_at" DESC"#
        );
    }

    // ===== Document lowering (to_mongo) ===============================

    #[test]
    fn test_to_mongo_empty_filter_is_empty_doc() {
        assert_eq!(Filter::new().to_mongo(), json!({}));
    }

    #[test]
    fn test_to_mongo_single_predicate_is_bare_clause() {
        let f = Filter::new().where_eq("name", "alice");
        assert_eq!(f.to_mongo(), json!({ "name": { "$eq": "alice" } }));
    }

    #[test]
    fn test_to_mongo_multiple_predicates_are_anded() {
        let f = Filter::new()
            .where_eq("name", "alice")
            .where_eq("active", true);
        assert_eq!(
            f.to_mongo(),
            json!({ "$and": [
                { "name": { "$eq": "alice" } },
                { "active": { "$eq": true } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_every_op() {
        let cases: Vec<(Op, Value, Value)> = vec![
            (Op::Eq, json!(1), json!({ "f": { "$eq": 1 } })),
            (Op::Ne, json!(1), json!({ "f": { "$ne": 1 } })),
            (Op::Lt, json!(1), json!({ "f": { "$lt": 1 } })),
            (Op::Lte, json!(1), json!({ "f": { "$lte": 1 } })),
            (Op::Gt, json!(1), json!({ "f": { "$gt": 1 } })),
            (Op::Gte, json!(1), json!({ "f": { "$gte": 1 } })),
            (Op::In, json!([1, 2]), json!({ "f": { "$in": [1, 2] } })),
            (Op::IsNil, Value::Null, json!({ "f": { "$eq": null } })),
        ];
        for (op, value, want) in cases {
            let f = Filter::new().add(Predicate::new("f", op, value));
            assert_eq!(f.to_mongo(), want, "op {op}");
        }
    }

    #[test]
    fn test_to_mongo_like_translates_wildcards_to_regex() {
        let f = Filter::new().add(Predicate::new("name", Op::Like, "a%c_"));
        assert_eq!(f.to_mongo(), json!({ "name": { "$regex": "^a.*c.$" } }));
    }

    #[test]
    fn test_to_mongo_like_escapes_regex_metachars() {
        let f = Filter::new().add(Predicate::new("name", Op::Like, "a.b+"));
        assert_eq!(f.to_mongo(), json!({ "name": { "$regex": r"^a\.b\+$" } }));
    }

    #[test]
    fn test_to_mongo_ilike_adds_case_insensitive_option() {
        let f = Filter::new().add(Predicate::new("name", Op::ILike, "a%"));
        assert_eq!(
            f.to_mongo(),
            json!({ "name": { "$regex": "^a.*$", "$options": "i" } })
        );
    }

    /// Regression: `Op::Like` lowers to an **anchored** `$regex` (`^…$`),
    /// so the document backend is a full-value match — matching the SQL
    /// `LIKE` and in-memory matcher rather than Mongo's default unanchored
    /// substring `$regex`. `name LIKE 'A%'` would, when unanchored, lower
    /// to `A.*` which Mongo treats as a substring match (selecting `"bAr"`),
    /// whereas SQL `LIKE`/in-memory matching require the pattern to consume
    /// the whole value. The anchors realign the document backend with the
    /// others.
    #[test]
    fn test_to_mongo_like_is_anchored_full_match() {
        let f = Filter::new().add(Predicate::new("name", Op::Like, "A%"));
        let doc = f.to_mongo();
        assert_eq!(doc, json!({ "name": { "$regex": "^A.*$" } }));
        let regex = doc["name"]["$regex"].as_str().unwrap();
        assert!(
            regex.starts_with('^') && regex.ends_with('$'),
            "Mongo $regex must be anchored at both ends so it is a \
             full-value match like SQL LIKE / in-memory matching: {regex}"
        );
    }

    #[test]
    fn test_mongo_sort_maps_directions() {
        let f = Filter::new()
            .order_by("a", Direction::Asc)
            .order_by("b", Direction::Desc);
        assert_eq!(f.mongo_sort(), json!({ "a": 1, "b": -1 }));
        assert_eq!(Filter::new().mongo_sort(), json!({}));
    }
}
