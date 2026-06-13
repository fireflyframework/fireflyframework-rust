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

//! Backend-neutral **`@query` custom-query** support — the Rust port of
//! pyfly's `data.query.query` decorator plus the shared parts of its
//! SQLAlchemy [`QueryExecutor`] and MongoDB `MongoQueryExecutor`.
//!
//! pyfly's `@query` attaches a raw query string (`__pyfly_query__`) and a
//! `native` flag to a repository method; the adapter's bean post-processor
//! then compiles that string into an executable callable. Rust has no
//! runtime bean post-processor, so this module instead provides the *pure*
//! compilation building blocks — name + named-parameter binding, JPQL → SQL
//! transpilation, and return-shape inference — that the
//! [`firefly-data-sqlx`](https://docs.rs/firefly-data-sqlx) and
//! [`firefly-data-mongodb`](https://docs.rs/firefly-data-mongodb) adapters
//! execute end-to-end.
//!
//! # The two paths
//!
//! - **Relational** ([`CustomQuery`]): a string with `:param` named
//!   placeholders, either *native* SQL (`native = true`) or *JPQL-like*
//!   (`native = false`, transpiled to SQL via [`transpile_jpql`]). Binding
//!   ([`CustomQuery::bind`]) rewrites every `:param` to the dialect's
//!   positional placeholder (`$1`, `?`, …) and produces the matching ordered
//!   argument vector; the return shape ([`QueryShape`]) is inferred from the
//!   statement (`SELECT COUNT` → count, `EXISTS(` → exists, otherwise list).
//! - **Document** ([`substitute_named_params`]): a JSON filter or
//!   aggregation-pipeline string with `":param"` placeholders, substituted
//!   recursively the way pyfly's `_substitute_params` does — an exact
//!   `":param"` string becomes the *typed* argument value, an embedded
//!   `:param` becomes its string form.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{CustomQuery, PostgresDialect, QueryShape};
//! use serde_json::json;
//! use std::collections::BTreeMap;
//!
//! // A JPQL-like custom query (default, non-native): the entity alias and
//! // boolean literals are transpiled to SQL.
//! let q = CustomQuery::jpql("SELECT u FROM User u WHERE u.email = :email AND u.active = true");
//! let mut params = BTreeMap::new();
//! params.insert("email".to_string(), json!("a@b.com"));
//! let bound = q.bind(&PostgresDialect, "User", "users", &params).unwrap();
//! assert_eq!(bound.sql, r#"SELECT * FROM users WHERE email = $1 AND active = 1"#);
//! assert_eq!(bound.args, vec![json!("a@b.com")]);
//! assert_eq!(bound.shape, QueryShape::List);
//! ```

use std::collections::BTreeMap;

use serde_json::Value;

use crate::dialect::SqlDialect;

/// The inferred return shape of a custom relational query, mirroring
/// pyfly's `QueryExecutor` shape detection: a `SELECT COUNT(...)` query
/// returns a scalar count, a query containing `EXISTS(` returns a boolean,
/// and everything else returns a list of mapped rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryShape {
    /// `SELECT COUNT(...)` — a single `i64` scalar.
    Count,
    /// A query containing `EXISTS(` — a boolean (`scalar > 0`).
    Exists,
    /// Any other `SELECT` — a list of mapped rows.
    List,
}

impl QueryShape {
    /// Infers the shape from an (already-transpiled) SQL statement, the way
    /// pyfly's `QueryExecutor.compile_query_method` does: a leading
    /// `SELECT COUNT` is a [`Count`](QueryShape::Count), a statement
    /// containing `EXISTS(` (case-insensitive) is an
    /// [`Exists`](QueryShape::Exists), otherwise a [`List`](QueryShape::List).
    pub fn infer(sql: &str) -> QueryShape {
        let trimmed = sql.trim_start();
        let upper = trimmed.to_uppercase();
        if upper.starts_with("SELECT COUNT") {
            QueryShape::Count
        } else if contains_exists_call(&upper) {
            QueryShape::Exists
        } else {
            QueryShape::List
        }
    }
}

/// Whether `upper` (an already-upper-cased SQL string) contains an
/// `EXISTS(` call — `EXISTS` followed by optional whitespace and a `(`,
/// mirroring pyfly's `\bEXISTS\s*\(` regex.
fn contains_exists_call(upper: &str) -> bool {
    let bytes = upper.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = upper[search_from..].find("EXISTS") {
        let pos = search_from + rel;
        let after = pos + "EXISTS".len();
        // Skip whitespace after EXISTS, then require an opening paren.
        let mut j = after;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'(' {
            return true;
        }
        search_from = after;
    }
    false
}

/// The error returned when binding a [`CustomQuery`]'s named parameters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CustomQueryError {
    /// The query referenced a `:param` for which no value was supplied.
    #[error("firefly/data: custom query references parameter {name:?} but no value was bound")]
    MissingParameter {
        /// The name of the unbound parameter.
        name: String,
    },
}

/// A bound relational custom query: the final positional-placeholder SQL,
/// the ordered argument values, and the inferred [`QueryShape`].
#[derive(Debug, Clone, PartialEq)]
pub struct BoundQuery {
    /// The SQL with every `:param` rewritten to the dialect's positional
    /// placeholder (`$1`, `?`, …), in first-occurrence order.
    pub sql: String,
    /// The argument values in placeholder order — ready to bind as scalar
    /// parameters.
    pub args: Vec<Value>,
    /// The inferred return shape.
    pub shape: QueryShape,
}

/// A Spring-Data-style `@query` custom relational query — the Rust port of
/// pyfly's `@query(value, native=...)` for the SQLAlchemy adapter.
///
/// Holds a raw query string and a `native` flag. A *native* query is used
/// verbatim (only `:param` binding is applied); a non-native (JPQL-like)
/// query is first transpiled to SQL via [`transpile_jpql`]. Call
/// [`CustomQuery::bind`] with a dialect, the entity name + table, and the
/// named-parameter values to get a [`BoundQuery`] the adapter can execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomQuery {
    value: String,
    native: bool,
}

impl CustomQuery {
    /// Builds a custom query from `value` with an explicit `native` flag —
    /// the direct analogue of pyfly's `query(value, native=...)`.
    pub fn new(value: impl Into<String>, native: bool) -> Self {
        CustomQuery {
            value: value.into(),
            native,
        }
    }

    /// A **native** query — `value` is treated as raw SQL and used verbatim
    /// (after `:param` binding). pyfly's `@query(sql, native=True)`.
    pub fn native(value: impl Into<String>) -> Self {
        CustomQuery::new(value, true)
    }

    /// A **JPQL-like** query — `value` is transpiled to SQL by
    /// [`transpile_jpql`] before `:param` binding. pyfly's default
    /// `@query(jpql)`.
    pub fn jpql(value: impl Into<String>) -> Self {
        CustomQuery::new(value, false)
    }

    /// The raw query string as supplied.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Whether this query is native (raw SQL) rather than JPQL-like.
    pub fn is_native(&self) -> bool {
        self.native
    }

    /// Binds the named parameters and renders the executable SQL for
    /// `dialect`.
    ///
    /// `entity_name` / `table` drive the JPQL transpilation (ignored for a
    /// native query); `params` maps each `:name` placeholder to its value.
    /// Every `:param` occurrence is rewritten to the dialect's positional
    /// placeholder in first-occurrence order (a parameter referenced twice
    /// is bound twice, once per occurrence, preserving positional ordering),
    /// and `args` is the matching value vector. Returns
    /// [`CustomQueryError::MissingParameter`] when the query references a
    /// `:param` absent from `params`.
    pub fn bind(
        &self,
        dialect: &dyn SqlDialect,
        entity_name: &str,
        table: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<BoundQuery, CustomQueryError> {
        let sql_template = if self.native {
            self.value.clone()
        } else {
            transpile_jpql(&self.value, entity_name, table)
        };
        let shape = QueryShape::infer(&sql_template);
        let (sql, args) = bind_named_params(&sql_template, dialect, params)?;
        Ok(BoundQuery { sql, args, shape })
    }
}

/// Rewrites every `:name` named placeholder in `sql_template` to `dialect`'s
/// positional placeholder, returning the rendered SQL and the ordered
/// argument values (one entry per placeholder occurrence, in source order).
///
/// A `:name` is matched as a colon followed by an identifier (`[A-Za-z0-9_]`,
/// not starting with a digit); `::` (a PostgreSQL cast) is left untouched, so
/// `value::text` is preserved. Returns
/// [`CustomQueryError::MissingParameter`] when a referenced name is absent
/// from `params`.
fn bind_named_params(
    sql_template: &str,
    dialect: &dyn SqlDialect,
    params: &BTreeMap<String, Value>,
) -> Result<(String, Vec<Value>), CustomQueryError> {
    let bytes = sql_template.as_bytes();
    let mut out = String::with_capacity(sql_template.len());
    let mut args: Vec<Value> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b':' {
            // Leave a `::` cast operator untouched (consume both colons).
            if i + 1 < bytes.len() && bytes[i + 1] == b':' {
                out.push_str("::");
                i += 2;
                continue;
            }
            // A named placeholder must start with a letter or underscore.
            let start = i + 1;
            if start < bytes.len() && (bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
                let mut j = start;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let name = &sql_template[start..j];
                let value = params
                    .get(name)
                    .ok_or_else(|| CustomQueryError::MissingParameter {
                        name: name.to_string(),
                    })?;
                out.push_str(&dialect.placeholder(args.len() + 1));
                args.push(value.clone());
                i = j;
                continue;
            }
        }
        // Push the byte unchanged (UTF-8-safe: ASCII bytes only matched above).
        out.push(c as char);
        i += 1;
    }
    Ok((out, args))
}

/// Lightweight JPQL → SQL transpiler — the Rust port of pyfly's
/// `QueryExecutor._transpile_jpql`.
///
/// Given the entity's class name (`entity_name`) and table (`table`), it
/// applies the same five rewrites pyfly does:
///
/// 1. `FROM Entity alias` → `FROM <table>`,
/// 2. `SELECT alias` → `SELECT *`,
/// 3. `SELECT COUNT(alias)` → `SELECT COUNT(*)`,
/// 4. `alias.field` → `field` (alias prefix stripped),
/// 5. `= true` / `= false` → `= 1` / `= 0`.
///
/// Matching is case-insensitive for the keywords, exactly as pyfly's regexes
/// are. Named `:param` placeholders are left untouched (they are resolved
/// later by [`bind_named_params`]).
///
/// # Examples
///
/// ```
/// use firefly_data::transpile_jpql;
/// assert_eq!(
///     transpile_jpql("SELECT u FROM User u WHERE u.email = :email", "User", "users"),
///     "SELECT * FROM users WHERE email = :email",
/// );
/// assert_eq!(
///     transpile_jpql("SELECT COUNT(u) FROM User u WHERE u.role = :role", "User", "users"),
///     "SELECT COUNT(*) FROM users WHERE role = :role",
/// );
/// ```
pub fn transpile_jpql(jpql: &str, entity_name: &str, table: &str) -> String {
    // 1. Find the alias in `FROM <Entity> <alias>` (case-insensitive on the
    //    keyword + entity name, anchored to the entity so SQL keywords are
    //    never captured as the alias).
    let alias = find_from_alias(jpql, entity_name);

    // 2. Replace `FROM <Entity> <alias>` with `FROM <table>` (first match).
    let mut sql = replace_from_entity_alias(jpql, entity_name, table);

    if let Some(alias) = &alias {
        // 3. `SELECT <alias>` (a bare entity select) -> `SELECT *`.
        sql = replace_select_alias(&sql, alias);
        // 4. `SELECT COUNT(<alias>)` -> `SELECT COUNT(*)`.
        sql = replace_count_alias(&sql, alias);
        // 5. `<alias>.field` -> `field`.
        sql = strip_alias_prefix(&sql, alias);
    }

    // 6. Boolean literals.
    sql = replace_bool_literal(&sql, "true", "1");
    sql = replace_bool_literal(&sql, "false", "0");
    sql
}

/// Lower-cased ASCII view used for case-insensitive keyword scanning while
/// keeping byte offsets aligned with the original (ASCII-only transforms).
fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Finds the alias `a` in the first `FROM <entity> <a>` occurrence.
fn find_from_alias(jpql: &str, entity_name: &str) -> Option<String> {
    let lc = lower(jpql);
    let needle = format!("from {} ", lower(entity_name));
    let pos = lc.find(&needle)?;
    let after = pos + needle.len();
    let rest = &jpql[after..];
    let alias: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if alias.is_empty() {
        None
    } else {
        Some(alias)
    }
}

/// Replaces the first `FROM <entity> <alias>` with `FROM <table>`.
fn replace_from_entity_alias(jpql: &str, entity_name: &str, table: &str) -> String {
    let lc = lower(jpql);
    let needle = format!("from {} ", lower(entity_name));
    let Some(pos) = lc.find(&needle) else {
        return jpql.to_string();
    };
    let after = pos + needle.len();
    // Length of the alias identifier following the keyword.
    let alias_len = jpql[after..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum::<usize>();
    let mut out = String::with_capacity(jpql.len());
    out.push_str(&jpql[..pos]);
    out.push_str("FROM ");
    out.push_str(table);
    out.push_str(&jpql[after + alias_len..]);
    out
}

/// Replaces `SELECT <alias>` (followed by a non-identifier char) with
/// `SELECT *`.
fn replace_select_alias(sql: &str, alias: &str) -> String {
    let lc = lower(sql);
    let needle = format!("select {}", lower(alias));
    let Some(pos) = lc.find(&needle) else {
        return sql.to_string();
    };
    let end = pos + needle.len();
    // Only a *bare* alias select (next char is not part of an identifier,
    // i.e. not a `.` or alphanumeric) — `SELECT u.x` is handled by the
    // alias-prefix strip instead.
    let next_is_ident = sql[end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if next_is_ident {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    out.push_str(&sql[..pos]);
    out.push_str("SELECT *");
    out.push_str(&sql[end..]);
    out
}

/// Replaces `SELECT COUNT(<alias>)` with `SELECT COUNT(*)`.
fn replace_count_alias(sql: &str, alias: &str) -> String {
    let lc = lower(sql);
    // Scan for `count(` then check the contents are just the alias.
    let mut from = 0;
    while let Some(rel) = lc[from..].find("count(") {
        let open = from + rel + "count(".len();
        // Collect the inner token (trimming whitespace).
        let inner_end = lc[open..].find(')').map(|r| open + r);
        if let Some(close) = inner_end {
            let inner = sql[open..close].trim();
            if inner.eq_ignore_ascii_case(alias) {
                let mut out = String::with_capacity(sql.len());
                out.push_str(&sql[..open]);
                out.push('*');
                out.push_str(&sql[close..]);
                return out;
            }
        }
        from = open;
    }
    sql.to_string()
}

/// Strips every `<alias>.` prefix from `sql`.
fn strip_alias_prefix(sql: &str, alias: &str) -> String {
    let prefix = format!("{alias}.");
    let lc = lower(sql);
    let lc_prefix = lower(&prefix);
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < sql.len() {
        if lc[i..].starts_with(&lc_prefix)
            // The char before must not be an identifier char (so `xu.` is not
            // mistaken for the `u.` alias prefix).
            && (i == 0
                || !sql[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            i += prefix.len();
            continue;
        }
        let ch = sql[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Replaces `= <literal>` (case-insensitive, word-bounded) with `= <repl>`.
fn replace_bool_literal(sql: &str, literal: &str, repl: &str) -> String {
    let lc = lower(sql);
    let lit = lower(literal);
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < sql.len() {
        // Look for `=` optionally followed by whitespace, then the literal.
        if sql.as_bytes()[i] == b'=' {
            let mut j = i + 1;
            while j < sql.len() && sql.as_bytes()[j].is_ascii_whitespace() {
                j += 1;
            }
            if lc[j..].starts_with(&lit) {
                let after = j + lit.len();
                let bounded = after >= sql.len()
                    || !sql.as_bytes()[after].is_ascii_alphanumeric()
                        && sql.as_bytes()[after] != b'_';
                if bounded {
                    out.push('=');
                    out.push(' ');
                    out.push_str(repl);
                    i = after;
                    continue;
                }
            }
        }
        let ch = sql[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Recursively substitutes `":param"` named placeholders in a parsed JSON
/// value — the Rust port of pyfly's MongoDB `_substitute_params`.
///
/// Substitution rules (identical to pyfly):
///
/// - A **string** that is exactly `":name"` (after trimming) and whose
///   `name` is in `params` is replaced by the **typed** parameter value
///   (so `":active"` → a real boolean, `":ids"` → a real array, …).
/// - A **string** that merely *contains* one or more `:name` tokens has each
///   replaced by `str(value)` (the value's plain string form), preserving the
///   surrounding text.
/// - Objects and arrays are recursed into; all other scalars pass through.
///
/// This is what powers a Mongo `@query` JSON filter / aggregation pipeline:
/// the adapter parses the query string once, then calls this with the
/// method's keyword arguments to produce the final BSON-ready document.
///
/// # Examples
///
/// ```
/// use firefly_data::substitute_named_params;
/// use serde_json::json;
/// use std::collections::BTreeMap;
///
/// let mut params = BTreeMap::new();
/// params.insert("email".to_string(), json!("a@b.com"));
/// params.insert("active".to_string(), json!(true));
/// let template = json!({ "email": ":email", "active": ":active" });
/// assert_eq!(
///     substitute_named_params(&template, &params),
///     json!({ "email": "a@b.com", "active": true }),
/// );
/// ```
pub fn substitute_named_params(value: &Value, params: &BTreeMap<String, Value>) -> Value {
    match value {
        Value::Object(map) => {
            let out = map
                .iter()
                .map(|(k, v)| (k.clone(), substitute_named_params(v, params)))
                .collect();
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| substitute_named_params(v, params))
                .collect(),
        ),
        Value::String(s) => substitute_string_param(s, params),
        other => other.clone(),
    }
}

/// Applies the string-level substitution rules for one JSON string value.
fn substitute_string_param(s: &str, params: &BTreeMap<String, Value>) -> Value {
    let stripped = s.trim();
    // Exact-match: the whole string is one `:name` placeholder -> typed value.
    if let Some(name) = stripped.strip_prefix(':') {
        if !name.is_empty() {
            if let Some(value) = params.get(name) {
                return value.clone();
            }
        }
    }
    // Partial / embedded placeholders -> replace each with its string form.
    let mut result = s.to_string();
    for (name, value) in params {
        let placeholder = format!(":{name}");
        if result.contains(&placeholder) {
            result = result.replace(&placeholder, &value_string_form(value));
        }
    }
    Value::String(result)
}

/// The plain string form of a JSON value for embedded substitution (Python
/// `str(value)` parity: a JSON string drops its quotes, everything else
/// renders as its JSON literal).
fn value_string_form(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::{MySqlDialect, PostgresDialect};
    use serde_json::json;

    fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    // ----- QueryShape inference (pyfly QueryExecutor shape detection) -----

    #[test]
    fn shape_count_for_select_count() {
        assert_eq!(
            QueryShape::infer("SELECT COUNT(*) FROM users"),
            QueryShape::Count
        );
        assert_eq!(
            QueryShape::infer("  select count(*) from x"),
            QueryShape::Count
        );
    }

    #[test]
    fn shape_exists_for_exists_call() {
        assert_eq!(
            QueryShape::infer("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)"),
            QueryShape::Exists
        );
        assert_eq!(
            QueryShape::infer("select 1 where exists (select 1)"),
            QueryShape::Exists
        );
    }

    #[test]
    fn shape_list_otherwise() {
        assert_eq!(QueryShape::infer("SELECT * FROM users"), QueryShape::List);
        // A column literally named "existstamp" must NOT trip the exists check.
        assert_eq!(
            QueryShape::infer("SELECT existstamp FROM users"),
            QueryShape::List
        );
    }

    // ----- JPQL transpiler (port of _transpile_jpql cases) -----

    #[test]
    fn jpql_select_alias_to_star() {
        assert_eq!(
            transpile_jpql(
                "SELECT u FROM User u WHERE u.email = :email",
                "User",
                "users"
            ),
            "SELECT * FROM users WHERE email = :email"
        );
    }

    #[test]
    fn jpql_count_alias_to_star() {
        assert_eq!(
            transpile_jpql(
                "SELECT COUNT(u) FROM User u WHERE u.role = :role",
                "User",
                "users"
            ),
            "SELECT COUNT(*) FROM users WHERE role = :role"
        );
    }

    #[test]
    fn jpql_bool_literals() {
        assert_eq!(
            transpile_jpql(
                "SELECT u FROM User u WHERE u.active = true",
                "User",
                "users"
            ),
            "SELECT * FROM users WHERE active = 1"
        );
        assert_eq!(
            transpile_jpql(
                "SELECT u FROM User u WHERE u.active = false",
                "User",
                "users"
            ),
            "SELECT * FROM users WHERE active = 0"
        );
    }

    #[test]
    fn jpql_case_insensitive() {
        // The keyword matching is case-insensitive, but the replacements are
        // emitted with pyfly's canonical casing (`SELECT *` / `FROM <table>`),
        // exactly as its `re.sub` substitutes a literal replacement string.
        assert_eq!(
            transpile_jpql("select u from User u where u.x = :x", "User", "users"),
            "SELECT * FROM users where x = :x"
        );
    }

    #[test]
    fn jpql_strips_only_matching_alias_prefix() {
        // `xu.` must not be confused with the `u.` alias prefix.
        assert_eq!(
            transpile_jpql(
                "SELECT u FROM User u WHERE u.a = :a AND xu = u.b",
                "User",
                "users"
            ),
            "SELECT * FROM users WHERE a = :a AND xu = b"
        );
    }

    // ----- Named-parameter binding -----

    #[test]
    fn bind_native_postgres_positional() {
        let q = CustomQuery::native("SELECT * FROM users WHERE email = :email AND role = :role");
        let bound = q
            .bind(
                &PostgresDialect,
                "User",
                "users",
                &params(&[("email", json!("a@b.com")), ("role", json!("admin"))]),
            )
            .unwrap();
        assert_eq!(
            bound.sql,
            "SELECT * FROM users WHERE email = $1 AND role = $2"
        );
        assert_eq!(bound.args, vec![json!("a@b.com"), json!("admin")]);
        assert_eq!(bound.shape, QueryShape::List);
    }

    #[test]
    fn bind_mysql_question_marks() {
        let q = CustomQuery::native("SELECT COUNT(*) FROM users WHERE role = :role");
        let bound = q
            .bind(
                &MySqlDialect,
                "User",
                "users",
                &params(&[("role", json!("admin"))]),
            )
            .unwrap();
        assert_eq!(bound.sql, "SELECT COUNT(*) FROM users WHERE role = ?");
        assert_eq!(bound.shape, QueryShape::Count);
    }

    #[test]
    fn bind_repeated_param_binds_twice() {
        let q = CustomQuery::native("SELECT * FROM t WHERE a = :v OR b = :v");
        let bound = q
            .bind(&PostgresDialect, "T", "t", &params(&[("v", json!(7))]))
            .unwrap();
        assert_eq!(bound.sql, "SELECT * FROM t WHERE a = $1 OR b = $2");
        assert_eq!(bound.args, vec![json!(7), json!(7)]);
    }

    #[test]
    fn bind_leaves_postgres_cast_untouched() {
        let q = CustomQuery::native("SELECT id::text FROM t WHERE id = :id");
        let bound = q
            .bind(&PostgresDialect, "T", "t", &params(&[("id", json!("x"))]))
            .unwrap();
        assert_eq!(bound.sql, "SELECT id::text FROM t WHERE id = $1");
        assert_eq!(bound.args, vec![json!("x")]);
    }

    #[test]
    fn bind_missing_param_errors() {
        let q = CustomQuery::native("SELECT * FROM t WHERE a = :missing");
        let err = q
            .bind(&PostgresDialect, "T", "t", &params(&[]))
            .unwrap_err();
        assert_eq!(
            err,
            CustomQueryError::MissingParameter {
                name: "missing".to_string()
            }
        );
    }

    #[test]
    fn jpql_query_binds_after_transpile() {
        let q =
            CustomQuery::jpql("SELECT u FROM User u WHERE u.email = :email AND u.active = true");
        let bound = q
            .bind(
                &PostgresDialect,
                "User",
                "users",
                &params(&[("email", json!("a@b.com"))]),
            )
            .unwrap();
        assert_eq!(
            bound.sql,
            "SELECT * FROM users WHERE email = $1 AND active = 1"
        );
        assert_eq!(bound.args, vec![json!("a@b.com")]);
    }

    // ----- Mongo named-parameter substitution (port of _substitute_params) -----

    #[test]
    fn mongo_exact_match_keeps_type() {
        let template = json!({ "email": ":email", "active": ":active" });
        let p = params(&[("email", json!("a@b.com")), ("active", json!(true))]);
        assert_eq!(
            substitute_named_params(&template, &p),
            json!({ "email": "a@b.com", "active": true })
        );
    }

    #[test]
    fn mongo_array_param_kept_typed() {
        let template = json!({ "role": { "$in": ":roles" } });
        let p = params(&[("roles", json!(["admin", "user"]))]);
        assert_eq!(
            substitute_named_params(&template, &p),
            json!({ "role": { "$in": ["admin", "user"] } })
        );
    }

    #[test]
    fn mongo_embedded_param_stringified() {
        let template = json!({ "name": { "$regex": "^:prefix" } });
        let p = params(&[("prefix", json!("al"))]);
        assert_eq!(
            substitute_named_params(&template, &p),
            json!({ "name": { "$regex": "^al" } })
        );
    }

    #[test]
    fn mongo_pipeline_recursed() {
        let template = json!([
            { "$match": { "status": ":status" } },
            { "$group": { "_id": "$category" } },
        ]);
        let p = params(&[("status", json!("active"))]);
        assert_eq!(
            substitute_named_params(&template, &p),
            json!([
                { "$match": { "status": "active" } },
                { "$group": { "_id": "$category" } },
            ])
        );
    }

    #[test]
    fn mongo_unknown_param_passes_through() {
        let template = json!({ "x": ":unknown" });
        let p = params(&[]);
        // Unbound exact placeholder stays as-is (a literal string).
        assert_eq!(
            substitute_named_params(&template, &p),
            json!({ "x": ":unknown" })
        );
    }
}
