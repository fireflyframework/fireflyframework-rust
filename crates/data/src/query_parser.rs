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

//! Spring-Data-style **derived query method** parser — the Rust port of
//! pyfly's `data.query_parser`.
//!
//! Parses method-name-style query specifications like
//! `find_by_status_and_role_order_by_name_desc` into a structured
//! [`ParsedQuery`], then lowers that (with bound argument values) into
//! the existing [`Filter`](crate::Filter) /
//! [`Specification`](crate::Specification) DSL so it can be executed
//! against a [`Repository`](crate::Repository).
//!
//! # Grammar
//!
//! **Prefixes:** `find_by`, `count_by`, `exists_by`, `delete_by`.
//!
//! **Connectors:** `_and_`, `_or_`.
//!
//! **Operator suffixes** (on a field name; checked longest-first so
//! `_greater_than_equal` is not mistaken for `_greater_than`):
//!
//! | suffix | [`QueryOperator`] | meaning |
//! |---|---|---|
//! | *(none)* | [`Eq`](QueryOperator::Eq) | equals (default) |
//! | `_greater_than` | [`Gt`](QueryOperator::Gt) | `>` |
//! | `_less_than` | [`Lt`](QueryOperator::Lt) | `<` |
//! | `_greater_than_equal` | [`Gte`](QueryOperator::Gte) | `>=` |
//! | `_less_than_equal` | [`Lte`](QueryOperator::Lte) | `<=` |
//! | `_between` | [`Between`](QueryOperator::Between) | BETWEEN (2 args) |
//! | `_like` | [`Like`](QueryOperator::Like) | LIKE |
//! | `_containing` | [`Containing`](QueryOperator::Containing) | LIKE `%value%` |
//! | `_in` | [`In`](QueryOperator::In) | IN (list arg) |
//! | `_not` | [`Not`](QueryOperator::Not) | `!=` |
//! | `_is_null` | [`IsNull`](QueryOperator::IsNull) | IS NULL (no arg) |
//! | `_is_not_null` | [`IsNotNull`](QueryOperator::IsNotNull) | IS NOT NULL (no arg) |
//!
//! **Ordering suffix:** `_order_by_{field}_{asc|desc}` (chainable).
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{QueryMethodParser, QueryOperator, QueryPrefix};
//!
//! let parser = QueryMethodParser::new();
//! let parsed = parser.parse("find_by_status_and_role_order_by_name_desc").unwrap();
//! assert_eq!(parsed.prefix, QueryPrefix::Find);
//! assert_eq!(parsed.predicates.len(), 2);
//! assert_eq!(parsed.predicates[0].field, "status");
//! assert_eq!(parsed.predicates[0].operator, QueryOperator::Eq);
//! assert_eq!(parsed.connectors, vec!["and".to_string()]);
//! assert_eq!(parsed.order_clauses[0].field, "name");
//! ```

use serde::Serialize;
use serde_json::Value;

use crate::filter::{Direction, Filter, Op, Predicate, Sort};
use crate::specification::Specification;

/// Operator suffixes, longest-first so partial matches do not win
/// (`_greater_than_equal` before `_greater_than`, `_is_not_null` before
/// `_is_null` / `_not`).
const OPERATORS: &[(&str, QueryOperator)] = &[
    ("_greater_than_equal", QueryOperator::Gte),
    ("_less_than_equal", QueryOperator::Lte),
    ("_greater_than", QueryOperator::Gt),
    ("_less_than", QueryOperator::Lt),
    ("_is_not_null", QueryOperator::IsNotNull),
    ("_is_null", QueryOperator::IsNull),
    ("_containing", QueryOperator::Containing),
    ("_between", QueryOperator::Between),
    ("_not", QueryOperator::Not),
    ("_like", QueryOperator::Like),
    ("_in", QueryOperator::In),
];

/// The query prefix — what kind of operation the method describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryPrefix {
    /// `find_by_…` — select matching rows.
    Find,
    /// `count_by_…` — count matching rows.
    Count,
    /// `exists_by_…` — test whether any row matches.
    Exists,
    /// `delete_by_…` — delete matching rows.
    Delete,
}

impl QueryPrefix {
    /// The canonical string form, matching pyfly's `prefix` value
    /// (`find_by` / `count_by` / `exists_by` / `delete_by`).
    pub fn as_str(self) -> &'static str {
        match self {
            QueryPrefix::Find => "find_by",
            QueryPrefix::Count => "count_by",
            QueryPrefix::Exists => "exists_by",
            QueryPrefix::Delete => "delete_by",
        }
    }
}

/// A comparison operator parsed from a field-name suffix.
///
/// The string forms match pyfly's `OPERATORS` values exactly so a
/// migrating consumer inspecting the parsed shape sees the same tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOperator {
    /// Equals (default, no suffix).
    Eq,
    /// `!=` (suffix `_not`).
    Not,
    /// `>` (suffix `_greater_than`).
    Gt,
    /// `<` (suffix `_less_than`).
    Lt,
    /// `>=` (suffix `_greater_than_equal`).
    Gte,
    /// `<=` (suffix `_less_than_equal`).
    Lte,
    /// BETWEEN, two args (suffix `_between`).
    Between,
    /// LIKE with a caller-supplied pattern (suffix `_like`).
    Like,
    /// LIKE `%value%` (suffix `_containing`).
    Containing,
    /// IN, list arg (suffix `_in`).
    In,
    /// IS NULL, no arg (suffix `_is_null`).
    IsNull,
    /// IS NOT NULL, no arg (suffix `_is_not_null`).
    IsNotNull,
}

impl QueryOperator {
    /// The canonical string form, matching pyfly's `OPERATORS` values.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryOperator::Eq => "eq",
            QueryOperator::Not => "not",
            QueryOperator::Gt => "gt",
            QueryOperator::Lt => "lt",
            QueryOperator::Gte => "gte",
            QueryOperator::Lte => "lte",
            QueryOperator::Between => "between",
            QueryOperator::Like => "like",
            QueryOperator::Containing => "containing",
            QueryOperator::In => "in",
            QueryOperator::IsNull => "is_null",
            QueryOperator::IsNotNull => "is_not_null",
        }
    }

    /// How many bound argument values this operator consumes when the
    /// parsed query is lowered: `0` for the null tests, `2` for
    /// `BETWEEN`, `1` otherwise.
    pub fn arity(self) -> usize {
        match self {
            QueryOperator::IsNull | QueryOperator::IsNotNull => 0,
            QueryOperator::Between => 2,
            _ => 1,
        }
    }
}

/// A single field predicate parsed from a method name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPredicate {
    /// The field name.
    pub field: String,
    /// The comparison operator.
    pub operator: QueryOperator,
}

/// A single order-by clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderClause {
    /// The field to order by.
    pub field: String,
    /// The sort direction (`asc` by default).
    pub direction: Direction,
}

/// The structured result of parsing a query method name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// The operation kind.
    pub prefix: QueryPrefix,
    /// The field predicates, in declaration order.
    pub predicates: Vec<FieldPredicate>,
    /// The `"and"` / `"or"` connectors between predicates (one fewer
    /// than `predicates` for a well-formed query).
    pub connectors: Vec<String>,
    /// The order-by clauses.
    pub order_clauses: Vec<OrderClause>,
}

/// The error returned when a method name cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryParseError {
    /// The method name did not start with a recognised prefix.
    #[error(
        "Method name must start with one of [find_by_, count_by_, exists_by_, delete_by_]: {0}"
    )]
    UnknownPrefix(String),
}

/// The error returned when lowering a [`ParsedQuery`] to a [`Filter`] /
/// [`Specification`] with bound arguments.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueryBindError {
    /// The number of supplied arguments did not match the total arity of
    /// the parsed predicates.
    #[error("firefly/data: query expected {expected} argument(s), got {got}")]
    ArgumentCount {
        /// The number of arguments the parsed query requires.
        expected: usize,
        /// The number of arguments supplied.
        got: usize,
    },
}

/// Parses method names into [`ParsedQuery`] descriptions.
///
/// Port of pyfly's `QueryMethodParser`.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryMethodParser;

impl QueryMethodParser {
    /// Creates a parser.
    pub fn new() -> Self {
        QueryMethodParser
    }

    /// Parses `method_name` into a [`ParsedQuery`].
    ///
    /// Returns [`QueryParseError::UnknownPrefix`] when the name does not
    /// begin with one of the four recognised prefixes.
    pub fn parse(&self, method_name: &str) -> Result<ParsedQuery, QueryParseError> {
        // 1. Extract prefix.
        let prefixes = [
            ("find_by_", QueryPrefix::Find),
            ("count_by_", QueryPrefix::Count),
            ("exists_by_", QueryPrefix::Exists),
            ("delete_by_", QueryPrefix::Delete),
        ];
        let (prefix, mut body) = prefixes
            .iter()
            .find_map(|(p, kind)| {
                method_name
                    .strip_prefix(p)
                    .map(|rest| (*kind, rest.to_string()))
            })
            .ok_or_else(|| QueryParseError::UnknownPrefix(method_name.to_string()))?;

        // 2. Split off the _order_by_ suffix.
        let mut order_clauses = Vec::new();
        if let Some(idx) = body.find("_order_by_") {
            let order_body = body[idx + "_order_by_".len()..].to_string();
            body.truncate(idx);
            order_clauses = Self::parse_order(&order_body);
        }

        // 3. Split the body by connectors and parse each predicate.
        let (predicates, connectors) = Self::parse_predicates(&body);

        Ok(ParsedQuery {
            prefix,
            predicates,
            connectors,
            order_clauses,
        })
    }

    /// Parses `field_asc_field2_desc` into [`OrderClause`]s.
    fn parse_order(order_body: &str) -> Vec<OrderClause> {
        let parts: Vec<&str> = order_body.split('_').collect();
        let mut clauses = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let mut field_parts = Vec::new();
            while i < parts.len() && parts[i] != "asc" && parts[i] != "desc" {
                field_parts.push(parts[i]);
                i += 1;
            }
            let field = field_parts.join("_");
            let mut direction = Direction::Asc;
            if i < parts.len() && (parts[i] == "asc" || parts[i] == "desc") {
                direction = if parts[i] == "desc" {
                    Direction::Desc
                } else {
                    Direction::Asc
                };
                i += 1;
            }
            if !field.is_empty() {
                clauses.push(OrderClause { field, direction });
            }
        }
        clauses
    }

    /// Splits a predicate body by `_and_` / `_or_` and parses each
    /// segment, returning the predicates and the connectors between them.
    fn parse_predicates(body: &str) -> (Vec<FieldPredicate>, Vec<String>) {
        if body.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut predicates = Vec::new();
        let mut connectors = Vec::new();

        // Walk the body, splitting on the first occurrence of either
        // connector each step — equivalent to pyfly's re.split that keeps
        // the connector tokens.
        let mut rest = body;
        loop {
            let and_pos = rest.find("_and_");
            let or_pos = rest.find("_or_");
            let next = match (and_pos, or_pos) {
                (Some(a), Some(o)) if a < o => Some((a, "_and_", "and")),
                (Some(_), Some(o)) => Some((o, "_or_", "or")),
                (Some(a), None) => Some((a, "_and_", "and")),
                (None, Some(o)) => Some((o, "_or_", "or")),
                (None, None) => None,
            };
            match next {
                Some((pos, token, connector)) => {
                    predicates.push(Self::parse_single_predicate(&rest[..pos]));
                    connectors.push(connector.to_string());
                    rest = &rest[pos + token.len()..];
                }
                None => {
                    predicates.push(Self::parse_single_predicate(rest));
                    break;
                }
            }
        }
        (predicates, connectors)
    }

    /// Parses a single `field[_operator]` segment.
    fn parse_single_predicate(segment: &str) -> FieldPredicate {
        for (suffix, op) in OPERATORS {
            if let Some(field) = segment.strip_suffix(suffix) {
                return FieldPredicate {
                    field: field.to_string(),
                    operator: *op,
                };
            }
        }
        FieldPredicate {
            field: segment.to_string(),
            operator: QueryOperator::Eq,
        }
    }
}

impl ParsedQuery {
    /// The total number of bound argument values this query expects (the
    /// sum of every predicate's [`QueryOperator::arity`]).
    pub fn arg_count(&self) -> usize {
        self.predicates.iter().map(|p| p.operator.arity()).sum()
    }

    /// Lowers this parsed query, with bound `args`, into a
    /// [`Specification`] tree.
    ///
    /// Arguments are consumed left-to-right in predicate order:
    /// `BETWEEN` takes two (`field >= lo AND field <= hi`), the null
    /// tests take none, every other operator takes one. `OR` connectors
    /// group their two operands (left-associative), matching pyfly's
    /// compiler. `_containing` wraps the argument in `%…%`; `_is_not_null`
    /// becomes `NOT (field IS NULL)`.
    ///
    /// Returns [`QueryBindError::ArgumentCount`] when the supplied
    /// argument count does not match [`ParsedQuery::arg_count`].
    pub fn to_specification(&self, args: &[Value]) -> Result<Specification, QueryBindError> {
        let expected = self.arg_count();
        if args.len() != expected {
            return Err(QueryBindError::ArgumentCount {
                expected,
                got: args.len(),
            });
        }

        // Build each predicate's specification, consuming args in order.
        let mut idx = 0usize;
        let mut specs: Vec<Specification> = Vec::with_capacity(self.predicates.len());
        for p in &self.predicates {
            let spec = Self::predicate_to_spec(p, args, &mut idx);
            specs.push(spec);
        }

        // Combine left-to-right honouring connectors. `and` flattens into
        // the running conjunction; `or` groups the running term with the
        // next operand.
        if specs.is_empty() {
            return Ok(Specification::all());
        }
        let mut iter = specs.into_iter();
        let mut acc = iter.next().unwrap();
        for (connector, spec) in self.connectors.iter().zip(iter) {
            acc = match connector.as_str() {
                "or" => acc.or(spec),
                _ => acc.and(spec),
            };
        }
        Ok(acc)
    }

    /// Lowers this parsed query, with bound `args`, into a [`Filter`].
    ///
    /// This succeeds only when the query is a pure conjunction (no `or`
    /// connector and no `_is_not_null`/`_not`-into-OR shape) — the same
    /// constraint [`Specification::to_filter`] enforces. Order-by clauses
    /// always project onto the filter's ORDER BY. Returns `Ok(None)` when
    /// the predicate tree cannot be represented as a flat AND-only
    /// [`Filter`]; use [`ParsedQuery::to_specification`] for those.
    pub fn to_filter(&self, args: &[Value]) -> Result<Option<Filter>, QueryBindError> {
        let spec = self.to_specification(args)?;
        let filter = spec.to_filter().map(|mut f| {
            f.sorts = self.sort_clauses();
            f
        });
        Ok(filter)
    }

    /// Lowers this parsed query, with bound `args`, into a MongoDB
    /// `$`-operator filter document — the document-store execution path,
    /// the analogue of [`ParsedQuery::to_filter`] for SQL.
    ///
    /// It builds the [`Specification`] via
    /// [`ParsedQuery::to_specification`] and lowers that with
    /// [`Specification::to_mongo`], so the parsed-query → Mongo lowering
    /// goes through the **same** spec tree as the SQL and in-memory
    /// paths (the spec is the single source of truth), matching pyfly's
    /// `MongoQueryMethodCompiler`. The order-by clauses are *not* part of
    /// the filter document — use [`ParsedQuery::mongo_sort`] for the
    /// cursor sort spec.
    ///
    /// Returns [`QueryBindError::ArgumentCount`] when the supplied
    /// argument count does not match [`ParsedQuery::arg_count`].
    pub fn to_mongo(&self, args: &[Value]) -> Result<Value, QueryBindError> {
        Ok(self.to_specification(args)?.to_mongo())
    }

    /// The MongoDB sort spec for this query's order-by clauses, as an
    /// ordered document `{field: 1 | -1, …}` (`1` ascending, `-1`
    /// descending) — what a Mongo adapter passes to the cursor's `sort`.
    pub fn mongo_sort(&self) -> Value {
        let mut map = serde_json::Map::new();
        for o in &self.order_clauses {
            let dir = match o.direction {
                Direction::Asc => 1,
                Direction::Desc => -1,
            };
            map.insert(o.field.clone(), Value::from(dir));
        }
        Value::Object(map)
    }

    /// The order-by clauses lowered to the filter DSL's [`Sort`].
    pub fn sort_clauses(&self) -> Vec<Sort> {
        self.order_clauses
            .iter()
            .map(|o| Sort {
                field: o.field.clone(),
                direction: o.direction,
            })
            .collect()
    }

    /// Evaluates this query (with bound `args`) against an in-memory
    /// slice of `serde`-serialisable entities, returning the matching
    /// rows in declaration order.
    ///
    /// This is the storage-agnostic execution path: it builds the
    /// [`Specification`] via [`ParsedQuery::to_specification`] and runs
    /// [`Specification::matches`] over each entity, then applies the
    /// order-by clauses. It serves [`QueryPrefix::Find`] semantics; use
    /// [`ParsedQuery::count`] / [`ParsedQuery::exists`] for the count /
    /// exists prefixes.
    pub fn evaluate<'a, T>(
        &self,
        entities: &'a [T],
        args: &[Value],
    ) -> Result<Vec<&'a T>, QueryBindError>
    where
        T: Serialize,
    {
        let spec = self.to_specification(args)?;
        let mut matched: Vec<&'a T> = entities.iter().filter(|e| spec.matches(*e)).collect();
        self.apply_order(&mut matched);
        Ok(matched)
    }

    /// Counts the entities matching this query (with bound `args`) — the
    /// [`QueryPrefix::Count`] execution path.
    pub fn count<T>(&self, entities: &[T], args: &[Value]) -> Result<usize, QueryBindError>
    where
        T: Serialize,
    {
        let spec = self.to_specification(args)?;
        Ok(entities.iter().filter(|e| spec.matches(*e)).count())
    }

    /// Whether any entity matches this query (with bound `args`) — the
    /// [`QueryPrefix::Exists`] execution path.
    pub fn exists<T>(&self, entities: &[T], args: &[Value]) -> Result<bool, QueryBindError>
    where
        T: Serialize,
    {
        let spec = self.to_specification(args)?;
        Ok(entities.iter().any(|e| spec.matches(e)))
    }

    fn apply_order<T>(&self, matched: &mut [&T])
    where
        T: Serialize,
    {
        if self.order_clauses.is_empty() {
            return;
        }
        // Serialise once per row for stable comparison.
        matched.sort_by(|a, b| {
            let va = serde_json::to_value(a).unwrap_or(Value::Null);
            let vb = serde_json::to_value(b).unwrap_or(Value::Null);
            for clause in &self.order_clauses {
                let fa = va.get(&clause.field).unwrap_or(&Value::Null);
                let fb = vb.get(&clause.field).unwrap_or(&Value::Null);
                let ord = json_order(fa, fb);
                let ord = match clause.direction {
                    Direction::Asc => ord,
                    Direction::Desc => ord.reverse(),
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        });
    }

    fn predicate_to_spec(p: &FieldPredicate, args: &[Value], idx: &mut usize) -> Specification {
        match p.operator {
            QueryOperator::Eq => Self::take1(&p.field, Op::Eq, args, idx),
            QueryOperator::Not => Self::take1(&p.field, Op::Ne, args, idx),
            QueryOperator::Gt => Self::take1(&p.field, Op::Gt, args, idx),
            QueryOperator::Lt => Self::take1(&p.field, Op::Lt, args, idx),
            QueryOperator::Gte => Self::take1(&p.field, Op::Gte, args, idx),
            QueryOperator::Lte => Self::take1(&p.field, Op::Lte, args, idx),
            QueryOperator::Like => Self::take1(&p.field, Op::Like, args, idx),
            QueryOperator::In => Self::take1(&p.field, Op::In, args, idx),
            QueryOperator::Containing => {
                let value = args.get(*idx).cloned().unwrap_or(Value::Null);
                *idx += 1;
                let pattern = match value {
                    Value::String(s) => Value::String(format!("%{s}%")),
                    other => other,
                };
                Specification::pred(Predicate::new(&p.field, Op::Like, pattern))
            }
            QueryOperator::Between => {
                let lo = args.get(*idx).cloned().unwrap_or(Value::Null);
                let hi = args.get(*idx + 1).cloned().unwrap_or(Value::Null);
                *idx += 2;
                Specification::pred(Predicate::new(&p.field, Op::Gte, lo))
                    .and(Specification::pred(Predicate::new(&p.field, Op::Lte, hi)))
            }
            QueryOperator::IsNull => Specification::pred(Predicate::is_nil(&p.field)),
            QueryOperator::IsNotNull => Specification::pred(Predicate::is_nil(&p.field)).not(),
        }
    }

    fn take1(field: &str, op: Op, args: &[Value], idx: &mut usize) -> Specification {
        let value = args.get(*idx).cloned().unwrap_or(Value::Null);
        *idx += 1;
        Specification::pred(Predicate::new(field, op, value))
    }
}

/// Total order over JSON scalars for in-memory sorting: numbers compare
/// numerically, strings lexically, bools false<true, nulls last; mixed
/// types fall back to a stable type-tag order.
fn json_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&y.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater,
        (_, Value::Null) => Ordering::Less,
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 4,
        Value::Bool(_) => 0,
        Value::Number(_) => 1,
        Value::String(_) => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;

    fn parser() -> QueryMethodParser {
        QueryMethodParser::new()
    }

    // ===== Parser tests (port of TestParser* in test_query_parser.py) =

    #[test]
    fn test_find_by_email() {
        let result = parser().parse("find_by_email").unwrap();
        assert_eq!(result.prefix, QueryPrefix::Find);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "email");
        assert_eq!(result.predicates[0].operator, QueryOperator::Eq);
        assert!(result.connectors.is_empty());
    }

    #[test]
    fn test_count_by_active() {
        let result = parser().parse("count_by_active").unwrap();
        assert_eq!(result.prefix, QueryPrefix::Count);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "active");
        assert_eq!(result.predicates[0].operator, QueryOperator::Eq);
    }

    #[test]
    fn test_exists_by_email() {
        let result = parser().parse("exists_by_email").unwrap();
        assert_eq!(result.prefix, QueryPrefix::Exists);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "email");
    }

    #[test]
    fn test_delete_by_status() {
        let result = parser().parse("delete_by_status").unwrap();
        assert_eq!(result.prefix, QueryPrefix::Delete);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "status");
    }

    #[test]
    fn test_invalid_prefix_raises() {
        let err = parser().parse("get_by_name").unwrap_err();
        assert!(matches!(err, QueryParseError::UnknownPrefix(_)));
        assert!(err.to_string().contains("must start with one of"));
    }

    #[test]
    fn test_find_by_status_and_role() {
        let result = parser().parse("find_by_status_and_role").unwrap();
        assert_eq!(result.predicates.len(), 2);
        assert_eq!(result.predicates[0].field, "status");
        assert_eq!(result.predicates[0].operator, QueryOperator::Eq);
        assert_eq!(result.predicates[1].field, "role");
        assert_eq!(result.predicates[1].operator, QueryOperator::Eq);
        assert_eq!(result.connectors, vec!["and".to_string()]);
    }

    #[test]
    fn test_find_by_status_or_role() {
        let result = parser().parse("find_by_status_or_role").unwrap();
        assert_eq!(result.predicates.len(), 2);
        assert_eq!(result.predicates[0].field, "status");
        assert_eq!(result.predicates[1].field, "role");
        assert_eq!(result.connectors, vec!["or".to_string()]);
    }

    #[test]
    fn test_find_by_age_greater_than() {
        let result = parser().parse("find_by_age_greater_than").unwrap();
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Gt);
    }

    #[test]
    fn test_find_by_name_like() {
        let result = parser().parse("find_by_name_like").unwrap();
        assert_eq!(result.predicates[0].field, "name");
        assert_eq!(result.predicates[0].operator, QueryOperator::Like);
    }

    #[test]
    fn test_find_by_age_between() {
        let result = parser().parse("find_by_age_between").unwrap();
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Between);
    }

    #[test]
    fn test_find_by_email_is_null() {
        let result = parser().parse("find_by_email_is_null").unwrap();
        assert_eq!(result.predicates[0].field, "email");
        assert_eq!(result.predicates[0].operator, QueryOperator::IsNull);
    }

    #[test]
    fn test_find_by_name_containing() {
        let result = parser().parse("find_by_name_containing").unwrap();
        assert_eq!(result.predicates[0].field, "name");
        assert_eq!(result.predicates[0].operator, QueryOperator::Containing);
    }

    #[test]
    fn test_find_by_role_in() {
        let result = parser().parse("find_by_role_in").unwrap();
        assert_eq!(result.predicates[0].field, "role");
        assert_eq!(result.predicates[0].operator, QueryOperator::In);
    }

    #[test]
    fn test_find_by_name_order_by_created_at_desc() {
        let result = parser()
            .parse("find_by_name_order_by_created_at_desc")
            .unwrap();
        assert_eq!(result.prefix, QueryPrefix::Find);
        assert_eq!(result.predicates.len(), 1);
        assert_eq!(result.predicates[0].field, "name");
        assert_eq!(result.predicates[0].operator, QueryOperator::Eq);
        assert_eq!(result.order_clauses.len(), 1);
        assert_eq!(result.order_clauses[0].field, "created_at");
        assert_eq!(result.order_clauses[0].direction, Direction::Desc);
    }

    #[test]
    fn test_order_by_multiple_fields() {
        let result = parser()
            .parse("find_by_status_order_by_name_asc_price_desc")
            .unwrap();
        assert_eq!(result.order_clauses.len(), 2);
        assert_eq!(result.order_clauses[0].field, "name");
        assert_eq!(result.order_clauses[0].direction, Direction::Asc);
        assert_eq!(result.order_clauses[1].field, "price");
        assert_eq!(result.order_clauses[1].direction, Direction::Desc);
    }

    #[test]
    fn test_order_by_default_asc() {
        let result = parser().parse("find_by_status_order_by_name").unwrap();
        assert_eq!(result.order_clauses.len(), 1);
        assert_eq!(result.order_clauses[0].field, "name");
        assert_eq!(result.order_clauses[0].direction, Direction::Asc);
    }

    // ----- Edge cases (port of TestParserEdgeCases) -------------------

    #[test]
    fn test_greater_than_equal() {
        let result = parser().parse("find_by_age_greater_than_equal").unwrap();
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Gte);
    }

    #[test]
    fn test_less_than_equal() {
        let result = parser().parse("find_by_age_less_than_equal").unwrap();
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Lte);
    }

    #[test]
    fn test_less_than() {
        let result = parser().parse("find_by_age_less_than").unwrap();
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Lt);
    }

    #[test]
    fn test_is_not_null() {
        let result = parser().parse("find_by_email_is_not_null").unwrap();
        assert_eq!(result.predicates[0].field, "email");
        assert_eq!(result.predicates[0].operator, QueryOperator::IsNotNull);
    }

    #[test]
    fn test_not_operator() {
        let result = parser().parse("find_by_status_not").unwrap();
        assert_eq!(result.predicates[0].field, "status");
        assert_eq!(result.predicates[0].operator, QueryOperator::Not);
    }

    #[test]
    fn test_operator_with_and_connector() {
        let result = parser()
            .parse("find_by_age_greater_than_and_status")
            .unwrap();
        assert_eq!(result.predicates.len(), 2);
        assert_eq!(result.predicates[0].field, "age");
        assert_eq!(result.predicates[0].operator, QueryOperator::Gt);
        assert_eq!(result.predicates[1].field, "status");
        assert_eq!(result.predicates[1].operator, QueryOperator::Eq);
        assert_eq!(result.connectors, vec!["and".to_string()]);
    }

    // ===== Execution tests (port of TestCompiler* in pyfly) ===========

    #[derive(Serialize)]
    struct Product {
        name: String,
        status: String,
        role: String,
        email: Option<String>,
        age: i64,
        price: f64,
        active: bool,
    }

    fn product(
        name: &str,
        status: &str,
        role: &str,
        email: Option<&str>,
        age: i64,
        price: f64,
        active: bool,
    ) -> Product {
        Product {
            name: name.into(),
            status: status.into(),
            role: role.into(),
            email: email.map(Into::into),
            age,
            price,
            active,
        }
    }

    fn seeded() -> Vec<Product> {
        vec![
            product(
                "Alpha",
                "active",
                "admin",
                Some("alpha@test.com"),
                25,
                10.0,
                true,
            ),
            product(
                "Beta",
                "active",
                "user",
                Some("beta@test.com"),
                30,
                20.0,
                true,
            ),
            product("Gamma", "inactive", "admin", None, 35, 30.0, false),
            product(
                "Delta",
                "inactive",
                "user",
                Some("delta@test.com"),
                40,
                40.0,
                false,
            ),
        ]
    }

    fn names(rows: &[&Product]) -> Vec<String> {
        let mut n: Vec<String> = rows.iter().map(|p| p.name.clone()).collect();
        n.sort();
        n
    }

    #[test]
    fn test_find_by_name() {
        let data = seeded();
        let parsed = parser().parse("find_by_name").unwrap();
        let rows = parsed.evaluate(&data, &[json!("Alpha")]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Alpha");
    }

    #[test]
    fn test_find_by_status_and_role_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_status_and_role").unwrap();
        let rows = parsed
            .evaluate(&data, &[json!("active"), json!("admin")])
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Alpha");
    }

    #[test]
    fn test_find_by_age_greater_than_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_age_greater_than").unwrap();
        let rows = parsed.evaluate(&data, &[json!(30)]).unwrap();
        assert_eq!(names(&rows), vec!["Delta", "Gamma"]);
    }

    #[test]
    fn test_find_by_active_order_by_price_desc() {
        let data = seeded();
        let parsed = parser()
            .parse("find_by_active_order_by_price_desc")
            .unwrap();
        let rows = parsed.evaluate(&data, &[json!(true)]).unwrap();
        assert_eq!(rows.len(), 2);
        // ordered by price desc: Beta(20) before Alpha(10)
        assert_eq!(rows[0].name, "Beta");
        assert_eq!(rows[1].name, "Alpha");
    }

    #[test]
    fn test_find_by_status_or_role_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_status_or_role").unwrap();
        // status=inactive OR role=admin -> Gamma, Delta, Alpha
        let rows = parsed
            .evaluate(&data, &[json!("inactive"), json!("admin")])
            .unwrap();
        assert_eq!(names(&rows), vec!["Alpha", "Delta", "Gamma"]);
    }

    #[test]
    fn test_find_by_email_is_null_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_email_is_null").unwrap();
        let rows = parsed.evaluate(&data, &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Gamma");
    }

    #[test]
    fn test_find_by_email_is_not_null_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_email_is_not_null").unwrap();
        let rows = parsed.evaluate(&data, &[]).unwrap();
        assert_eq!(names(&rows), vec!["Alpha", "Beta", "Delta"]);
    }

    #[test]
    fn test_find_by_name_containing_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_name_containing").unwrap();
        let rows = parsed.evaluate(&data, &[json!("lph")]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Alpha");
    }

    #[test]
    fn test_find_by_name_like_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_name_like").unwrap();
        let rows = parsed.evaluate(&data, &[json!("A%")]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Alpha");
    }

    #[test]
    fn test_find_by_role_in_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_role_in").unwrap();
        let rows = parsed.evaluate(&data, &[json!(["admin"])]).unwrap();
        assert_eq!(names(&rows), vec!["Alpha", "Gamma"]);
    }

    #[test]
    fn test_find_by_age_between_exec() {
        let data = seeded();
        let parsed = parser().parse("find_by_age_between").unwrap();
        let rows = parsed.evaluate(&data, &[json!(28), json!(38)]).unwrap();
        assert_eq!(names(&rows), vec!["Beta", "Gamma"]);
    }

    #[test]
    fn test_count_by_active_exec() {
        let parsed = parser().parse("count_by_active").unwrap();
        let count = parsed.count(&seeded(), &[json!(true)]).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_exists_by_email_exec() {
        let parsed = parser().parse("exists_by_email").unwrap();
        assert!(parsed
            .exists(&seeded(), &[json!("alpha@test.com")])
            .unwrap());
        assert!(!parsed
            .exists(&seeded(), &[json!("nonexistent@test.com")])
            .unwrap());
    }

    // ----- Lowering / binding tests -----------------------------------

    #[test]
    fn test_to_filter_for_conjunction() {
        let parsed = parser()
            .parse("find_by_status_and_role_order_by_name_desc")
            .unwrap();
        let filter = parsed
            .to_filter(&[json!("active"), json!("admin")])
            .unwrap()
            .expect("conjunction lowers to a filter");
        assert_eq!(filter.predicates.len(), 2);
        assert_eq!(filter.predicates[0].field, "status");
        assert_eq!(filter.predicates[1].field, "role");
        assert_eq!(filter.sorts.len(), 1);
        assert_eq!(filter.sorts[0].field, "name");
        assert_eq!(filter.sorts[0].direction, Direction::Desc);
    }

    #[test]
    fn test_to_filter_none_for_or_query() {
        let parsed = parser().parse("find_by_status_or_role").unwrap();
        let filter = parsed
            .to_filter(&[json!("active"), json!("admin")])
            .unwrap();
        assert!(filter.is_none(), "OR query cannot lower to a flat Filter");
    }

    #[test]
    fn test_between_lowers_to_two_predicates_in_filter() {
        let parsed = parser().parse("find_by_age_between").unwrap();
        let filter = parsed
            .to_filter(&[json!(28), json!(38)])
            .unwrap()
            .expect("between is a conjunction");
        assert_eq!(filter.predicates.len(), 2);
        assert_eq!(filter.predicates[0].op, Op::Gte);
        assert_eq!(filter.predicates[1].op, Op::Lte);
    }

    #[test]
    fn test_arg_count_mismatch_errors() {
        let parsed = parser().parse("find_by_age_between").unwrap();
        let err = parsed.to_specification(&[json!(1)]).unwrap_err();
        assert_eq!(
            err,
            QueryBindError::ArgumentCount {
                expected: 2,
                got: 1
            }
        );
    }

    #[test]
    fn test_arg_count_zero_for_null_tests() {
        let parsed = parser().parse("find_by_email_is_null").unwrap();
        assert_eq!(parsed.arg_count(), 0);
    }

    // ----- Document (Mongo) lowering ----------------------------------

    #[test]
    fn test_to_mongo_single_eq() {
        let parsed = parser().parse("find_by_status").unwrap();
        let doc = parsed.to_mongo(&[json!("active")]).unwrap();
        assert_eq!(doc, json!({ "status": { "$eq": "active" } }));
    }

    #[test]
    fn test_to_mongo_and_query() {
        let parsed = parser().parse("find_by_status_and_role").unwrap();
        let doc = parsed.to_mongo(&[json!("active"), json!("admin")]).unwrap();
        assert_eq!(
            doc,
            json!({ "$and": [
                { "status": { "$eq": "active" } },
                { "role": { "$eq": "admin" } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_or_query() {
        let parsed = parser().parse("find_by_status_or_role").unwrap();
        let doc = parsed
            .to_mongo(&[json!("inactive"), json!("admin")])
            .unwrap();
        assert_eq!(
            doc,
            json!({ "$or": [
                { "status": { "$eq": "inactive" } },
                { "role": { "$eq": "admin" } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_between_lowers_to_gte_lte_and() {
        let parsed = parser().parse("find_by_age_between").unwrap();
        let doc = parsed.to_mongo(&[json!(18), json!(65)]).unwrap();
        assert_eq!(
            doc,
            json!({ "$and": [
                { "age": { "$gte": 18 } },
                { "age": { "$lte": 65 } },
            ] })
        );
    }

    #[test]
    fn test_to_mongo_containing_is_regex() {
        // `_containing` lowers to `LIKE '%lph%'`, i.e. an anchored
        // `$regex: "^.*lph.*$"`. The `^…$` anchors keep the document
        // backend a full-value match consistent with SQL LIKE and the
        // in-memory matcher; with the leading/trailing `.*` this is exactly
        // a substring match, so `_containing` still selects the same rows.
        let parsed = parser().parse("find_by_name_containing").unwrap();
        let doc = parsed.to_mongo(&[json!("lph")]).unwrap();
        assert_eq!(doc, json!({ "name": { "$regex": "^.*lph.*$" } }));
    }

    #[test]
    fn test_to_mongo_is_not_null_is_nor() {
        let parsed = parser().parse("find_by_email_is_not_null").unwrap();
        let doc = parsed.to_mongo(&[]).unwrap();
        assert_eq!(doc, json!({ "$nor": [ { "email": { "$eq": null } } ] }));
    }

    #[test]
    fn test_to_mongo_in_query() {
        let parsed = parser().parse("find_by_role_in").unwrap();
        let doc = parsed.to_mongo(&[json!(["admin", "user"])]).unwrap();
        assert_eq!(doc, json!({ "role": { "$in": ["admin", "user"] } }));
    }

    #[test]
    fn test_to_mongo_arg_count_mismatch_errors() {
        let parsed = parser().parse("find_by_age_between").unwrap();
        let err = parsed.to_mongo(&[json!(1)]).unwrap_err();
        assert_eq!(
            err,
            QueryBindError::ArgumentCount {
                expected: 2,
                got: 1
            }
        );
    }

    #[test]
    fn test_mongo_sort_maps_order_clauses() {
        let parsed = parser()
            .parse("find_by_status_order_by_name_asc_price_desc")
            .unwrap();
        assert_eq!(parsed.mongo_sort(), json!({ "name": 1, "price": -1 }));
    }

    #[test]
    fn test_prefix_and_operator_string_forms() {
        assert_eq!(QueryPrefix::Find.as_str(), "find_by");
        assert_eq!(QueryPrefix::Count.as_str(), "count_by");
        assert_eq!(QueryPrefix::Exists.as_str(), "exists_by");
        assert_eq!(QueryPrefix::Delete.as_str(), "delete_by");
        assert_eq!(QueryOperator::Gte.as_str(), "gte");
        assert_eq!(QueryOperator::Between.as_str(), "between");
        assert_eq!(QueryOperator::IsNotNull.as_str(), "is_not_null");
        assert_eq!(QueryOperator::Containing.as_str(), "containing");
    }
}
