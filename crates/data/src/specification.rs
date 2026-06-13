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

//! Composable query predicates ([`Specification`]) that lower to the
//! [`Filter`](crate::Filter) DSL.
//!
//! This is the Rust port of pyfly's `Specification` pattern (itself a
//! port of Spring Data's `Specification<T>`). Where pyfly wraps a
//! Python callable and composes via the `&`, `|`, `~` operators, the
//! Rust port models the composition tree explicitly as an algebraic
//! type — [`Specification::Pred`], [`Specification::And`],
//! [`Specification::Or`], [`Specification::Not`] — and overloads the
//! same operators (`&`, `|`, `!`) so call sites read identically.
//!
//! A specification can be:
//!
//! - **lowered to SQL** via [`Specification::to_sql`] — a parenthesised,
//!   parameterised PostgreSQL `WHERE`-clause fragment using `$1`, `$2`, …
//!   placeholders, matching the conventions of [`Filter::to_sql`];
//! - **lowered to a [`Filter`]** via [`Specification::to_filter`] when
//!   the tree is a pure conjunction (AND of predicates), so it plugs
//!   straight into the existing [`Repository`](crate::Repository)
//!   contract;
//! - **evaluated in memory** via [`Specification::matches`] against any
//!   `serde`-serialisable entity, mirroring the behaviour the pyfly test
//!   suite asserts (which selects matching rows).
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Op, Predicate, Specification};
//!
//! let admin = Specification::pred(Predicate::new("role", Op::Eq, "admin"));
//! let active = Specification::pred(Predicate::new("active", Op::Eq, true));
//!
//! // (role = admin) AND (active = true)
//! let spec = admin.clone() & active.clone();
//! let (sql, args) = spec.to_sql();
//! assert_eq!(sql, r#"("role" = $1 AND "active" = $2)"#);
//! assert_eq!(args, vec![serde_json::json!("admin"), serde_json::json!(true)]);
//!
//! // role = admin OR active = true
//! let any = admin | active;
//! let (sql, _) = any.to_sql();
//! assert_eq!(sql, r#"("role" = $1 OR "active" = $2)"#);
//! ```

use std::ops::{BitAnd, BitOr, Not};

use serde::Serialize;
use serde_json::Value;

use crate::dialect::{PostgresDialect, SqlDialect};
use crate::filter::{
    combine_mongo_and, predicate_to_mongo, render_predicate_sql, Filter, Op, Predicate,
};

/// A composable query predicate.
///
/// Build leaves with [`Specification::pred`] and combine them with the
/// `&` (AND), `|` (OR), and `!` (NOT) operators — or the equivalent
/// [`Specification::and`], [`Specification::or`], and
/// [`Specification::not`] builder methods. An empty / no-op
/// specification is [`Specification::all`], which matches every row and
/// renders to an empty SQL fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Specification {
    /// Matches every row; renders to no SQL clause. The identity
    /// element for [`Specification::and`] and the no-op equivalent of
    /// pyfly's `Specification(lambda root, q: q)`.
    #[default]
    All,
    /// A single leaf predicate (column / operator / value).
    Pred(Predicate),
    /// Conjunction — every child must match.
    And(Vec<Specification>),
    /// Disjunction — at least one child must match.
    Or(Vec<Specification>),
    /// Negation — the child must not match.
    Not(Box<Specification>),
}

impl Specification {
    /// Returns the no-op specification matching every row. Equivalent
    /// to pyfly's `Specification(lambda root, q: q)`.
    pub fn all() -> Self {
        Specification::All
    }

    /// Wraps a single [`Predicate`] as a leaf specification.
    pub fn pred(p: Predicate) -> Self {
        Specification::Pred(p)
    }

    /// Convenience leaf: `field = value`.
    pub fn eq(field: impl Into<String>, value: impl Into<Value>) -> Self {
        Specification::Pred(Predicate::new(field, Op::Eq, value))
    }

    /// Combines `self` with `other` under AND.
    ///
    /// Combining with [`Specification::All`] is absorbed (the identity
    /// law), and nested `And` nodes are flattened so the rendered SQL
    /// stays flat: `(a AND b AND c)` rather than `((a AND b) AND c)`.
    pub fn and(self, other: Specification) -> Self {
        match (self, other) {
            (Specification::All, rhs) => rhs,
            (lhs, Specification::All) => lhs,
            (Specification::And(mut a), Specification::And(b)) => {
                a.extend(b);
                Specification::And(a)
            }
            (Specification::And(mut a), rhs) => {
                a.push(rhs);
                Specification::And(a)
            }
            (lhs, Specification::And(mut b)) => {
                b.insert(0, lhs);
                Specification::And(b)
            }
            (lhs, rhs) => Specification::And(vec![lhs, rhs]),
        }
    }

    /// Combines `self` with `other` under OR.
    ///
    /// Like pyfly's `__or__`, a no-op operand falls through to the other
    /// side (`All | x == x`). Nested `Or` nodes are flattened.
    pub fn or(self, other: Specification) -> Self {
        match (self, other) {
            (Specification::All, rhs) => rhs,
            (lhs, Specification::All) => lhs,
            (Specification::Or(mut a), Specification::Or(b)) => {
                a.extend(b);
                Specification::Or(a)
            }
            (Specification::Or(mut a), rhs) => {
                a.push(rhs);
                Specification::Or(a)
            }
            (lhs, Specification::Or(mut b)) => {
                b.insert(0, lhs);
                Specification::Or(b)
            }
            (lhs, rhs) => Specification::Or(vec![lhs, rhs]),
        }
    }

    /// Negates `self`.
    ///
    /// Like pyfly's `__invert__`, negating the no-op specification is
    /// still a no-op (there is no clause to negate), so `!All == All`.
    // `Not::not` is the operator-trait form; this inherent method exists
    // as a fluent alias and intentionally shadows nothing.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        match self {
            Specification::All => Specification::All,
            Specification::Not(inner) => *inner,
            other => Specification::Not(Box::new(other)),
        }
    }

    /// Returns `true` when this specification is a pure conjunction of
    /// leaf predicates (no `Or`, no `Not`), and therefore lowers to a
    /// flat [`Filter`] via [`Specification::to_filter`].
    pub fn is_conjunction(&self) -> bool {
        match self {
            Specification::All => true,
            Specification::Pred(_) => true,
            Specification::And(children) => children.iter().all(Specification::is_conjunction),
            Specification::Or(_) | Specification::Not(_) => false,
        }
    }

    /// Lowers a pure-conjunction specification to a [`Filter`], appending
    /// every leaf predicate as an AND-joined clause. Returns `None` when
    /// the tree contains an `Or` or `Not` node that the flat
    /// AND-only [`Filter`] cannot represent — use [`Specification::to_sql`]
    /// for those.
    pub fn to_filter(&self) -> Option<Filter> {
        if !self.is_conjunction() {
            return None;
        }
        let mut filter = Filter::new();
        self.collect_predicates(&mut filter);
        Some(filter)
    }

    fn collect_predicates(&self, filter: &mut Filter) {
        match self {
            Specification::All => {}
            Specification::Pred(p) => filter.predicates.push(p.clone()),
            Specification::And(children) => {
                for c in children {
                    c.collect_predicates(filter);
                }
            }
            // unreachable for a conjunction, but kept total for safety
            Specification::Or(_) | Specification::Not(_) => {}
        }
    }

    /// Renders the specification to a parenthesised, parameterised
    /// PostgreSQL `WHERE`-clause fragment plus its bound arguments.
    ///
    /// Placeholders are `$1`, `$2`, … numbered across the whole tree;
    /// identifiers are double-quoted; [`Op::IsNil`] renders `IS NULL`
    /// and consumes no argument slot — exactly the conventions of
    /// [`Filter::to_sql`]. The no-op [`Specification::All`] renders to an
    /// empty string with no args (it imposes no restriction).
    ///
    /// Unlike [`Filter::to_sql`], the returned fragment has **no leading
    /// `" WHERE "`** — it is a bare boolean expression suitable for
    /// embedding inside a larger clause, e.g. alongside a soft-delete
    /// guard.
    ///
    /// This is the **PostgreSQL default**: it is exactly
    /// `self.to_sql_with(&PostgresDialect)`. Use
    /// [`Specification::to_sql_with`] to render for MySQL or SQLite.
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        self.to_sql_with(&PostgresDialect)
    }

    /// Renders the specification to a parenthesised, parameterised SQL
    /// fragment for a specific [`SqlDialect`] — the storage-agnostic
    /// rendering path. Identifier quoting, placeholder syntax,
    /// `IN`-list shape, and case-insensitive `LIKE` all defer to
    /// `dialect`, so one [`Specification`] tree renders correct SQL for
    /// PostgreSQL (`$n` / `"id"` / `= ANY` / `ILIKE`), MySQL (`?` /
    /// `` `id` `` / `IN (?,…)` / `LOWER LIKE LOWER`), or SQLite.
    ///
    /// As with [`Specification::to_sql`], the fragment has no leading
    /// `" WHERE "`, and the args vector always matches the placeholders
    /// the dialect emitted (postgres binds an `IN` list as one array
    /// arg; MySQL/SQLite flatten it).
    pub fn to_sql_with(&self, dialect: &dyn SqlDialect) -> (String, Vec<Value>) {
        let mut args: Vec<Value> = Vec::new();
        let mut idx = 1usize;
        let sql = self.render(dialect, &mut args, &mut idx);
        (sql, args)
    }

    fn render(&self, dialect: &dyn SqlDialect, args: &mut Vec<Value>, idx: &mut usize) -> String {
        match self {
            Specification::All => String::new(),
            Specification::Pred(p) => render_predicate_sql(p, dialect, args, idx),
            Specification::And(children) => render_joined(children, "AND", dialect, args, idx),
            Specification::Or(children) => render_joined(children, "OR", dialect, args, idx),
            Specification::Not(inner) => {
                let inner_sql = inner.render(dialect, args, idx);
                if inner_sql.is_empty() {
                    String::new()
                } else {
                    format!("NOT {inner_sql}")
                }
            }
        }
    }

    /// Lowers the specification tree to a MongoDB `$`-operator filter
    /// document — the document-store analogue of
    /// [`Specification::to_sql`], so the **same** spec tree drives SQL,
    /// in-memory matching ([`Specification::matches`]), *and* a document
    /// backend. This keeps the [`Specification`] the single source of
    /// truth for every backend, exactly as pyfly factors the lowering
    /// behind the spec.
    ///
    /// The tree maps node-for-node onto Mongo operators, mirroring
    /// pyfly's `MongoSpecification`:
    ///
    /// - [`Specification::All`] → `{}` (matches everything),
    /// - [`Specification::Pred`] → a single field clause
    ///   (`{field: {$eq: …}}`, `{field: {$gt: …}}`, `like`/`ilike` →
    ///   `$regex`, `in` → `$in`, …),
    /// - [`Specification::And`] → `{"$and": [...]}`,
    /// - [`Specification::Or`] → `{"$or": [...]}`,
    /// - [`Specification::Not`] → `{"$nor": [...]}` (pyfly's `__invert__`).
    ///
    /// As in pyfly, a `$nor` / `$and` / `$or` wrapping an empty child is
    /// collapsed to `{}` so a no-op spec lowers cleanly.
    pub fn to_mongo(&self) -> Value {
        match self {
            Specification::All => Value::Object(serde_json::Map::new()),
            Specification::Pred(p) => predicate_to_mongo(p),
            Specification::And(children) => {
                let clauses = non_empty_mongo_children(children);
                combine_mongo_and(clauses)
            }
            Specification::Or(children) => {
                let clauses = non_empty_mongo_children(children);
                match clauses.len() {
                    0 => Value::Object(serde_json::Map::new()),
                    1 => clauses.into_iter().next().unwrap(),
                    _ => serde_json::json!({ "$or": clauses }),
                }
            }
            Specification::Not(inner) => {
                let doc = inner.to_mongo();
                if is_empty_mongo(&doc) {
                    Value::Object(serde_json::Map::new())
                } else {
                    serde_json::json!({ "$nor": [doc] })
                }
            }
        }
    }

    /// Evaluates the specification in memory against a `serde`-serialisable
    /// entity, returning whether it matches.
    ///
    /// The entity is serialised to a JSON object once; each leaf
    /// predicate is then tested against the named field. This mirrors the
    /// row-selection semantics the pyfly test suite asserts, and lets the
    /// combinators be used against any in-memory collection without a SQL
    /// backend. Returns `false` if the entity does not serialise to a
    /// JSON object.
    pub fn matches<T: Serialize>(&self, entity: &T) -> bool {
        match serde_json::to_value(entity) {
            Ok(Value::Object(map)) => self.eval(&Value::Object(map)),
            _ => false,
        }
    }

    fn eval(&self, row: &Value) -> bool {
        match self {
            Specification::All => true,
            Specification::Pred(p) => eval_predicate(p, row),
            Specification::And(children) => children.iter().all(|c| c.eval(row)),
            Specification::Or(children) => children.iter().any(|c| c.eval(row)),
            Specification::Not(inner) => !inner.eval(row),
        }
    }
}

fn render_joined(
    children: &[Specification],
    joiner: &str,
    dialect: &dyn SqlDialect,
    args: &mut Vec<Value>,
    idx: &mut usize,
) -> String {
    let parts: Vec<String> = children
        .iter()
        .map(|c| c.render(dialect, args, idx))
        .filter(|s| !s.is_empty())
        .collect();
    match parts.len() {
        0 => String::new(),
        1 => parts.into_iter().next().unwrap(),
        _ => format!("({})", parts.join(&format!(" {joiner} "))),
    }
}

/// Lowers each child to a Mongo doc, dropping the no-op (`{}`) children
/// so an `All` mixed into an `And`/`Or` does not pollute the operator
/// array — mirroring pyfly's "empty doc falls through" combinators.
fn non_empty_mongo_children(children: &[Specification]) -> Vec<Value> {
    children
        .iter()
        .map(Specification::to_mongo)
        .filter(|d| !is_empty_mongo(d))
        .collect()
}

/// Whether a lowered Mongo doc is the empty / match-everything `{}`.
fn is_empty_mongo(doc: &Value) -> bool {
    matches!(doc, Value::Object(map) if map.is_empty())
}

fn eval_predicate(p: &Predicate, row: &Value) -> bool {
    let actual = row.get(&p.field).unwrap_or(&Value::Null);
    match p.op {
        Op::Eq => actual == &p.value,
        Op::Ne => actual != &p.value,
        Op::Lt => json_cmp(actual, &p.value)
            .map(|o| o.is_lt())
            .unwrap_or(false),
        Op::Lte => json_cmp(actual, &p.value)
            .map(|o| o.is_le())
            .unwrap_or(false),
        Op::Gt => json_cmp(actual, &p.value)
            .map(|o| o.is_gt())
            .unwrap_or(false),
        Op::Gte => json_cmp(actual, &p.value)
            .map(|o| o.is_ge())
            .unwrap_or(false),
        Op::Like | Op::ILike => like_match(actual, &p.value, p.op == Op::ILike),
        Op::In => match &p.value {
            Value::Array(items) => items.contains(actual),
            _ => false,
        },
        Op::IsNil => actual.is_null(),
    }
}

/// Orders two JSON values for the relational comparison ops. Numbers
/// compare numerically; strings compare lexically; mismatched or
/// non-orderable types yield `None` (so the predicate fails).
fn json_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => None,
        },
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// SQL `LIKE` semantics for in-memory evaluation: `%` matches any run of
/// characters, `_` matches any single character. When `case_insensitive`
/// is set (the `ILIKE` op), both sides are lower-cased first.
fn like_match(actual: &Value, pattern: &Value, case_insensitive: bool) -> bool {
    let (Value::String(text), Value::String(pat)) = (actual, pattern) else {
        return false;
    };
    let (text, pat) = if case_insensitive {
        (text.to_lowercase(), pat.to_lowercase())
    } else {
        (text.clone(), pat.clone())
    };
    like_match_chars(
        &text.chars().collect::<Vec<_>>(),
        &pat.chars().collect::<Vec<_>>(),
    )
}

fn like_match_chars(text: &[char], pat: &[char]) -> bool {
    match pat.split_first() {
        None => text.is_empty(),
        Some((&'%', rest)) => {
            // `%` matches zero or more characters: try every suffix.
            (0..=text.len()).any(|i| like_match_chars(&text[i..], rest))
        }
        Some((&'_', rest)) => !text.is_empty() && like_match_chars(&text[1..], rest),
        Some((&c, rest)) => !text.is_empty() && text[0] == c && like_match_chars(&text[1..], rest),
    }
}

impl BitAnd for Specification {
    type Output = Specification;
    fn bitand(self, rhs: Specification) -> Specification {
        self.and(rhs)
    }
}

impl BitOr for Specification {
    type Output = Specification;
    fn bitor(self, rhs: Specification) -> Specification {
        self.or(rhs)
    }
}

impl Not for Specification {
    type Output = Specification;
    fn not(self) -> Specification {
        Specification::not(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{MySqlDialect, SqliteDialect};
    use serde::Serialize;
    use serde_json::json;

    #[derive(Serialize, Clone)]
    struct User {
        name: String,
        role: String,
        active: bool,
    }

    fn seeded() -> Vec<User> {
        vec![
            User {
                name: "Alice".into(),
                role: "admin".into(),
                active: true,
            },
            User {
                name: "Bob".into(),
                role: "user".into(),
                active: true,
            },
            User {
                name: "Charlie".into(),
                role: "admin".into(),
                active: false,
            },
            User {
                name: "Diana".into(),
                role: "user".into(),
                active: false,
            },
        ]
    }

    /// Apply a spec in memory and return the matching names, sorted —
    /// the exact `_names()` helper of pyfly's test_specification.py.
    fn names(spec: &Specification) -> Vec<String> {
        let mut out: Vec<String> = seeded()
            .into_iter()
            .filter(|u| spec.matches(u))
            .map(|u| u.name)
            .collect();
        out.sort();
        out
    }

    fn admin() -> Specification {
        Specification::eq("role", "admin")
    }
    fn active() -> Specification {
        Specification::eq("active", true)
    }

    // ---- ports of pyfly test_specification.py behaviour ----

    /// Port of `TestSpecificationSingle::test_filter_by_role`.
    #[test]
    fn test_filter_by_role() {
        assert_eq!(names(&admin()), vec!["Alice", "Charlie"]);
    }

    /// Port of `TestSpecificationSingle::test_filter_by_active`.
    #[test]
    fn test_filter_by_active() {
        assert_eq!(names(&active()), vec!["Alice", "Bob"]);
    }

    /// Port of `TestSpecificationAnd::test_and_combination`.
    #[test]
    fn test_and_combination() {
        let combined = admin() & active();
        assert_eq!(names(&combined), vec!["Alice"]);
    }

    /// Port of `TestSpecificationAnd::test_and_three_specs`.
    #[test]
    fn test_and_three_specs() {
        let name_spec = Specification::eq("name", "Alice");
        let combined = admin() & active() & name_spec;
        assert_eq!(names(&combined), vec!["Alice"]);
    }

    /// Port of `TestSpecificationOr::test_or_combination`.
    #[test]
    fn test_or_combination() {
        let combined = admin() | active();
        assert_eq!(names(&combined), vec!["Alice", "Bob", "Charlie"]);
    }

    /// Port of `TestSpecificationNot::test_not_active`.
    #[test]
    fn test_not_active() {
        let inactive = !active();
        assert_eq!(names(&inactive), vec!["Charlie", "Diana"]);
    }

    /// Port of `TestSpecificationNot::test_not_admin`.
    #[test]
    fn test_not_admin() {
        let non_admin = !admin();
        assert_eq!(names(&non_admin), vec!["Bob", "Diana"]);
    }

    /// Port of `TestSpecificationComplex::test_complex_composition`:
    /// `(admin AND active) OR Diana`.
    #[test]
    fn test_complex_composition() {
        let diana = Specification::eq("name", "Diana");
        let combined = (admin() & active()) | diana;
        assert_eq!(names(&combined), vec!["Alice", "Diana"]);
    }

    /// Port of `TestSpecificationComplex::test_not_combined_with_and`:
    /// `NOT admin AND active`.
    #[test]
    fn test_not_combined_with_and() {
        let combined = !admin() & active();
        assert_eq!(names(&combined), vec!["Bob"]);
    }

    /// Port of `TestSpecificationNoop::test_noop_returns_all`.
    #[test]
    fn test_noop_returns_all() {
        let noop = Specification::all();
        assert_eq!(names(&noop), vec!["Alice", "Bob", "Charlie", "Diana"]);
    }

    /// Port of `TestSpecificationNoop::test_noop_and_spec`.
    #[test]
    fn test_noop_and_spec() {
        let combined = Specification::all() & admin();
        assert_eq!(names(&combined), vec!["Alice", "Charlie"]);
        // Absorption: `All & x == x`.
        assert_eq!(combined, admin());
    }

    /// Port of `TestSpecificationNoop::test_noop_or_spec`.
    #[test]
    fn test_noop_or_spec() {
        let combined = Specification::all() | admin();
        assert_eq!(names(&combined), vec!["Alice", "Charlie"]);
    }

    /// Port of `TestSpecificationNoop::test_not_noop`.
    #[test]
    fn test_not_noop() {
        let negated = !Specification::all();
        assert_eq!(names(&negated), vec!["Alice", "Bob", "Charlie", "Diana"]);
        assert_eq!(negated, Specification::all());
    }

    // ---- Rust-specific: SQL lowering ----

    #[test]
    fn test_pred_to_sql() {
        let (sql, args) = admin().to_sql();
        assert_eq!(sql, r#""role" = $1"#);
        assert_eq!(args, vec![json!("admin")]);
    }

    #[test]
    fn test_and_to_sql_is_parenthesised_and_renumbered() {
        let (sql, args) = (admin() & active()).to_sql();
        assert_eq!(sql, r#"("role" = $1 AND "active" = $2)"#);
        assert_eq!(args, vec![json!("admin"), json!(true)]);
    }

    #[test]
    fn test_or_to_sql() {
        let (sql, args) = (admin() | active()).to_sql();
        assert_eq!(sql, r#"("role" = $1 OR "active" = $2)"#);
        assert_eq!(args, vec![json!("admin"), json!(true)]);
    }

    #[test]
    fn test_not_to_sql() {
        let (sql, args) = (!active()).to_sql();
        assert_eq!(sql, r#"NOT "active" = $1"#);
        assert_eq!(args, vec![json!(true)]);
    }

    #[test]
    fn test_complex_to_sql_placeholder_numbering() {
        let diana = Specification::eq("name", "Diana");
        let (sql, args) = ((admin() & active()) | diana).to_sql();
        assert_eq!(sql, r#"(("role" = $1 AND "active" = $2) OR "name" = $3)"#);
        assert_eq!(args, vec![json!("admin"), json!(true), json!("Diana")]);
    }

    #[test]
    fn test_all_renders_empty_sql() {
        let (sql, args) = Specification::all().to_sql();
        assert_eq!(sql, "");
        assert!(args.is_empty());
    }

    #[test]
    fn test_isnil_consumes_no_arg_slot() {
        let spec = Specification::pred(Predicate::is_nil("deleted_at")) & admin();
        let (sql, args) = spec.to_sql();
        assert_eq!(sql, r#"("deleted_at" IS NULL AND "role" = $1)"#);
        assert_eq!(args, vec![json!("admin")]);
    }

    // ---- Rust-specific: lowering to Filter ----

    #[test]
    fn test_conjunction_lowers_to_filter() {
        let spec = admin() & active();
        assert!(spec.is_conjunction());
        let filter = spec.to_filter().unwrap();
        assert_eq!(filter.predicates.len(), 2);
        let (sql, args) = filter.to_sql();
        assert_eq!(sql, r#" WHERE "role" = $1 AND "active" = $2"#);
        assert_eq!(args, vec![json!("admin"), json!(true)]);
    }

    #[test]
    fn test_or_is_not_a_conjunction_and_has_no_filter() {
        let spec = admin() | active();
        assert!(!spec.is_conjunction());
        assert!(spec.to_filter().is_none());
    }

    #[test]
    fn test_not_is_not_a_conjunction() {
        assert!(!(!admin()).is_conjunction());
        assert!((!admin()).to_filter().is_none());
    }

    #[test]
    fn test_all_lowers_to_empty_filter() {
        let filter = Specification::all().to_filter().unwrap();
        assert!(filter.predicates.is_empty());
        assert_eq!(filter.to_sql().0, "");
    }

    // ---- Rust-specific: in-memory operator behaviour ----

    #[test]
    fn test_like_match_in_memory() {
        let spec = Specification::pred(Predicate::new("name", Op::Like, "A%"));
        assert!(spec.matches(&seeded()[0])); // Alice
        assert!(!spec.matches(&seeded()[1])); // Bob
    }

    #[test]
    fn test_ilike_is_case_insensitive() {
        let spec = Specification::pred(Predicate::new("name", Op::ILike, "a%"));
        assert!(spec.matches(&seeded()[0])); // Alice
    }

    #[test]
    fn test_in_op_in_memory() {
        let spec = Specification::pred(Predicate::new("name", Op::In, json!(["Alice", "Bob"])));
        assert!(spec.matches(&seeded()[0]));
        assert!(!spec.matches(&seeded()[2]));
    }

    #[test]
    fn test_relational_ops_in_memory() {
        #[derive(Serialize)]
        struct Score {
            value: i64,
        }
        let gt = Specification::pred(Predicate::new("value", Op::Gt, 10));
        assert!(gt.matches(&Score { value: 11 }));
        assert!(!gt.matches(&Score { value: 10 }));
        let lte = Specification::pred(Predicate::new("value", Op::Lte, 10));
        assert!(lte.matches(&Score { value: 10 }));
    }

    #[test]
    fn test_flatten_keeps_sql_flat() {
        let spec = admin() & active() & Specification::eq("name", "Alice");
        // Three leaves flatten into one AND node, not nested pairs.
        if let Specification::And(children) = &spec {
            assert_eq!(children.len(), 3);
        } else {
            panic!("expected a flat And node");
        }
        let (sql, _) = spec.to_sql();
        assert_eq!(sql, r#"("role" = $1 AND "active" = $2 AND "name" = $3)"#);
    }

    #[test]
    fn test_double_negation_cancels() {
        assert_eq!(!!admin(), admin());
    }

    #[test]
    fn test_non_object_entity_does_not_match() {
        assert!(!admin().matches(&"not an object"));
    }

    // ---- Rust-specific: dialect-aware SQL lowering ------------------

    #[test]
    fn test_to_sql_equals_postgres_default() {
        let spec = (admin() & active()) | Specification::eq("name", "Diana");
        assert_eq!(spec.to_sql(), spec.to_sql_with(&PostgresDialect));
    }

    #[test]
    fn test_and_to_sql_for_mysql() {
        let (sql, args) = (admin() & active()).to_sql_with(&MySqlDialect);
        assert_eq!(sql, "(`role` = ? AND `active` = ?)");
        assert_eq!(args, vec![json!("admin"), json!(true)]);
    }

    #[test]
    fn test_or_to_sql_for_sqlite() {
        let (sql, args) = (admin() | active()).to_sql_with(&SqliteDialect);
        assert_eq!(sql, r#"("role" = ? OR "active" = ?)"#);
        assert_eq!(args, vec![json!("admin"), json!(true)]);
    }

    #[test]
    fn test_complex_to_sql_numbering_postgres_vs_mysql() {
        let diana = Specification::eq("name", "Diana");
        let spec = (admin() & active()) | diana;
        let (pg, pargs) = spec.to_sql_with(&PostgresDialect);
        assert_eq!(pg, r#"(("role" = $1 AND "active" = $2) OR "name" = $3)"#);
        let (my, margs) = spec.to_sql_with(&MySqlDialect);
        assert_eq!(my, "((`role` = ? AND `active` = ?) OR `name` = ?)");
        assert_eq!(pargs, margs);
    }

    #[test]
    fn test_in_spec_expands_for_mysql() {
        let spec = Specification::pred(Predicate::new("role", Op::In, json!(["a", "b"])))
            & Specification::eq("active", true);
        let (sql, args) = spec.to_sql_with(&MySqlDialect);
        assert_eq!(sql, "(`role` IN (?, ?) AND `active` = ?)");
        assert_eq!(args, vec![json!("a"), json!("b"), json!(true)]);
    }

    #[test]
    fn test_in_spec_array_param_for_postgres() {
        let spec = Specification::pred(Predicate::new("role", Op::In, json!(["a", "b"])))
            & Specification::eq("active", true);
        let (sql, args) = spec.to_sql_with(&PostgresDialect);
        assert_eq!(sql, r#"("role" = ANY($1) AND "active" = $2)"#);
        assert_eq!(args, vec![json!(["a", "b"]), json!(true)]);
    }

    #[test]
    fn test_ilike_spec_lowers_for_mysql() {
        let spec = Specification::pred(Predicate::new("name", Op::ILike, "a%"));
        let (sql, _) = spec.to_sql_with(&MySqlDialect);
        assert_eq!(sql, "LOWER(`name`) LIKE LOWER(?)");
    }

    // ---- Rust-specific: document (Mongo) lowering -------------------

    #[test]
    fn test_to_mongo_all_is_empty_doc() {
        assert_eq!(Specification::all().to_mongo(), json!({}));
    }

    #[test]
    fn test_to_mongo_pred_is_eq_clause() {
        assert_eq!(admin().to_mongo(), json!({ "role": { "$eq": "admin" } }));
    }

    #[test]
    fn test_to_mongo_and() {
        let spec = admin() & active();
        assert_eq!(
            spec.to_mongo(),
            json!({ "$and": [
                { "role": { "$eq": "admin" } },
                { "active": { "$eq": true } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_or() {
        let spec = admin() | active();
        assert_eq!(
            spec.to_mongo(),
            json!({ "$or": [
                { "role": { "$eq": "admin" } },
                { "active": { "$eq": true } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_not_uses_nor() {
        let spec = !active();
        assert_eq!(
            spec.to_mongo(),
            json!({ "$nor": [ { "active": { "$eq": true } } ] })
        );
    }

    #[test]
    fn test_to_mongo_complex_tree() {
        let diana = Specification::eq("name", "Diana");
        let spec = (admin() & active()) | diana;
        assert_eq!(
            spec.to_mongo(),
            json!({ "$or": [
                { "$and": [
                    { "role": { "$eq": "admin" } },
                    { "active": { "$eq": true } },
                ] },
                { "name": { "$eq": "Diana" } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_not_of_all_is_empty() {
        // `!All == All`, which lowers to {} — and even a raw Not(All)
        // collapses cleanly.
        assert_eq!((!Specification::all()).to_mongo(), json!({}));
    }

    #[test]
    fn test_to_mongo_isnil_clause() {
        let spec = Specification::pred(Predicate::is_nil("deleted_at"));
        assert_eq!(spec.to_mongo(), json!({ "deleted_at": { "$eq": null } }));
    }

    #[test]
    fn test_to_mongo_gt_lt_in_like() {
        let gt = Specification::pred(Predicate::new("age", Op::Gt, 18));
        assert_eq!(gt.to_mongo(), json!({ "age": { "$gt": 18 } }));
        let lt = Specification::pred(Predicate::new("age", Op::Lt, 65));
        assert_eq!(lt.to_mongo(), json!({ "age": { "$lt": 65 } }));
        let in_op = Specification::pred(Predicate::new("role", Op::In, json!(["a", "b"])));
        assert_eq!(in_op.to_mongo(), json!({ "role": { "$in": ["a", "b"] } }));
        let like = Specification::pred(Predicate::new("name", Op::Like, "A%"));
        assert_eq!(like.to_mongo(), json!({ "name": { "$regex": "A.*" } }));
    }
}
