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

//! AST node types for the rule DSL — the Rust counterpart of the Go
//! `ruleengine/models` package.
//!
//! The types here are pure data: [`RuleSet`] → [`Rule`] → [`Logic`] →
//! [`Condition`] / [`Action`]. They carry serde attributes whose field
//! names and omission rules match the Go struct tags exactly, so YAML
//! rule files (and their JSON projections) transfer across the Java,
//! .NET, Go, Python, and Rust runtimes verbatim.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Error returned when a YAML document cannot be parsed into (or an AST
/// cannot be rendered back to) the rule DSL.
///
/// The wrapped string is the underlying serde message — useful verbatim
/// in `400 Bad Request` responses from the [`crate::web`] layer.
#[derive(Debug, Error)]
#[error("ruleengine: invalid rule DSL: {0}")]
pub struct DslError(pub String);

/// `Op` enumerates the predicate operators recognised by the rule DSL —
/// kept identical to the Java/.NET/Go ports so YAML rule files transfer
/// across runtimes verbatim.
///
/// Like Go's `type Op string`, the set is **open**: an unrecognised
/// string parses into [`Op::Other`] and is only rejected at evaluation
/// time (see [`crate::core::EvalError::UnknownOp`]), never at parse
/// time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op {
    /// Deep equality (`eq`).
    Eq,
    /// Deep inequality (`ne`).
    Ne,
    /// Numeric less-than (`lt`).
    Lt,
    /// Numeric less-than-or-equal (`lte`).
    Lte,
    /// Numeric greater-than (`gt`).
    Gt,
    /// Numeric greater-than-or-equal (`gte`).
    Gte,
    /// Membership of the fact value in the condition's list (`in`).
    In,
    /// Negated membership (`notIn`).
    NotIn,
    /// Substring or list-element containment (`contains`).
    Contains,
    /// String prefix test (`startsWith`).
    StartsWith,
    /// String suffix test (`endsWith`).
    EndsWith,
    /// Regular-expression match (`matches`).
    Matches,
    /// The fact path resolves to null / is absent (`isNull`).
    IsNull,
    /// The fact path resolves to a non-null value (`isNotNull`).
    IsNotNull,
    /// Inclusive range check (`between`): the operand must be a
    /// two-element list `[lo, hi]` and the predicate holds when
    /// `lo <= fact <= hi`. A null/absent fact never matches. Ported
    /// from pyfly's `between` leaf operator.
    Between,
    /// Negated containment (`notContains`): the inverse of
    /// [`Op::Contains`]. A null/absent fact never matches (so neither
    /// `contains` nor `notContains` holds when the fact is absent),
    /// matching pyfly's `not_contains`.
    NotContains,
    /// The fact path is present **and** non-null (`exists`). The
    /// converse of [`Op::IsNull`]; the operand is ignored. Ported from
    /// pyfly's `exists`.
    Exists,
    /// The fact is null/absent, the empty string, the empty list, or
    /// the empty object (`isEmpty`). The operand is ignored. Ported
    /// from pyfly's `is_empty`.
    IsEmpty,
    /// Any operator string the engine does not implement; evaluation
    /// fails with an unknown-op error, mirroring Go's open `Op string`.
    Other(String),
}

impl Op {
    /// Every operator the evaluator implements, in documentation order.
    pub const ALL: [Op; 18] = [
        Op::Eq,
        Op::Ne,
        Op::Lt,
        Op::Lte,
        Op::Gt,
        Op::Gte,
        Op::In,
        Op::NotIn,
        Op::Contains,
        Op::StartsWith,
        Op::EndsWith,
        Op::Matches,
        Op::IsNull,
        Op::IsNotNull,
        Op::Between,
        Op::NotContains,
        Op::Exists,
        Op::IsEmpty,
    ];

    /// The wire spelling of the operator — exactly the string used in
    /// YAML/JSON rule documents (`"eq"`, `"notIn"`, `"isNotNull"`, …).
    ///
    /// The four pyfly-parity operators added on top of the Go set keep
    /// the crate's camelCase convention (`"notContains"`, `"isEmpty"`);
    /// [`Op::from`] additionally accepts pyfly's snake_case spellings
    /// (`"not_contains"`, `"is_empty"`, `"not_in"`, `"is_null"`, …) so
    /// rule documents authored against the pyfly DSL parse unchanged.
    pub fn as_str(&self) -> &str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::In => "in",
            Op::NotIn => "notIn",
            Op::Contains => "contains",
            Op::StartsWith => "startsWith",
            Op::EndsWith => "endsWith",
            Op::Matches => "matches",
            Op::IsNull => "isNull",
            Op::IsNotNull => "isNotNull",
            Op::Between => "between",
            Op::NotContains => "notContains",
            Op::Exists => "exists",
            Op::IsEmpty => "isEmpty",
            Op::Other(s) => s,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Op {
    fn from(s: &str) -> Self {
        match s {
            "eq" => Op::Eq,
            "ne" => Op::Ne,
            "lt" => Op::Lt,
            "lte" => Op::Lte,
            "gt" => Op::Gt,
            "gte" => Op::Gte,
            "in" => Op::In,
            // Canonical camelCase plus pyfly's snake_case alias.
            "notIn" | "not_in" => Op::NotIn,
            "contains" => Op::Contains,
            "startsWith" | "starts_with" => Op::StartsWith,
            "endsWith" | "ends_with" => Op::EndsWith,
            // pyfly spells regex `regex`; the Go/Rust spelling is `matches`.
            "matches" | "regex" => Op::Matches,
            "isNull" | "is_null" => Op::IsNull,
            "isNotNull" | "is_not_null" => Op::IsNotNull,
            "between" => Op::Between,
            "notContains" | "not_contains" => Op::NotContains,
            "exists" => Op::Exists,
            "isEmpty" | "is_empty" => Op::IsEmpty,
            other => Op::Other(other.to_owned()),
        }
    }
}

impl Serialize for Op {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OpVisitor;
        impl Visitor<'_> for OpVisitor {
            type Value = Op;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a rule operator string")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Op, E> {
                Ok(Op::from(v))
            }
        }
        deserializer.deserialize_str(OpVisitor)
    }
}

/// `Condition` is a single predicate against a fact path.
///
/// `path` is a dot-separated route into the fact object
/// (`user.address.country`); `value` is the comparison operand and is
/// omitted from the wire when null, matching Go's `omitempty`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// Dot-separated fact path the predicate reads.
    pub path: String,
    /// The predicate operator.
    pub op: Op,
    /// The comparison operand; [`Value::Null`] for unary operators
    /// (`isNull` / `isNotNull`) and omitted from serialized output.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
}

impl Condition {
    /// Builds a condition from its three parts.
    pub fn new(path: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        Condition {
            path: path.into(),
            op,
            value: value.into(),
        }
    }
}

/// `Logic` combines conditions via AND / OR / NOT.
///
/// Exactly one branch is normally populated; when several are set the
/// evaluator checks them in the fixed order `cond` → `all` → `any` →
/// `not` (same as the Go `switch`). An entirely empty `Logic` evaluates
/// **true** — a rule with no `when` always fires.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Logic {
    /// Conjunction: every sub-logic must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all: Vec<Logic>,
    /// Disjunction: at least one sub-logic must hold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<Logic>,
    /// Negation of the inner logic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not: Option<Box<Logic>>,
    /// Leaf predicate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cond: Option<Condition>,
}

impl Logic {
    /// Leaf logic node wrapping a single [`Condition`] — the Rust
    /// spelling of the `cond(path, op, v)` helper the Go tests define.
    pub fn cond(path: impl Into<String>, op: Op, value: impl Into<Value>) -> Self {
        Logic {
            cond: Some(Condition::new(path, op, value)),
            ..Logic::default()
        }
    }

    /// AND-composition of sub-logics.
    pub fn all(items: Vec<Logic>) -> Self {
        Logic {
            all: items,
            ..Logic::default()
        }
    }

    /// OR-composition of sub-logics.
    pub fn any(items: Vec<Logic>) -> Self {
        Logic {
            any: items,
            ..Logic::default()
        }
    }

    /// Negation of the inner logic.
    ///
    /// Named after the DSL keyword `not`; it is an associated
    /// constructor, not a `!` operator, hence the lint allowance.
    #[allow(clippy::should_implement_trait)]
    pub fn not(inner: Logic) -> Self {
        Logic {
            not: Some(Box::new(inner)),
            ..Logic::default()
        }
    }
}

/// `Action` is the side-effect emitted when a rule's [`Logic`]
/// evaluates true. The engine does not execute actions — it only
/// returns them in the [`crate::Verdict`] for the caller to interpret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// Discriminator the caller dispatches on (serialized as `type`,
    /// matching the Go field tag).
    #[serde(rename = "type")]
    pub action_type: String,
    /// Free-form action parameters; omitted from the wire when empty.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
}

impl Action {
    /// Builds an action with the given `type` and no parameters.
    pub fn new(action_type: impl Into<String>) -> Self {
        Action {
            action_type: action_type.into(),
            params: Map::new(),
        }
    }

    /// Adds one parameter, builder-style.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }
}

fn priority_is_zero(p: &i64) -> bool {
    *p == 0
}

/// Default for [`Rule::enabled`] — a rule is enabled unless the document
/// explicitly sets `enabled: false`.
fn enabled_default() -> bool {
    true
}

/// Serde skip predicate for [`Rule::enabled`]: an enabled rule is the
/// default, so `enabled: true` is omitted from the wire to keep the
/// Go-parity byte stream unchanged for rules that never opt out.
fn enabled_is_default(enabled: &bool) -> bool {
    *enabled
}

/// `Rule` is the top-level DSL document: an id, an optional priority,
/// a `when` logic tree and the `then` actions emitted on match.
///
/// On top of the Go set, the Rust port carries pyfly's `otherwise` and
/// `enabled` fields:
///
/// * `otherwise` — the **else-branch** actions emitted when `when`
///   evaluates **false** (pyfly's `Rule.otherwise`). Empty by default
///   and omitted from the wire, so a rule that never uses it serializes
///   byte-for-byte as before.
/// * `enabled` — when `false`, the rule is **skipped** entirely (it
///   never matches and fires neither `then` nor `otherwise`), matching
///   pyfly's disabled-rule short-circuit. Defaults to `true` and is
///   omitted from the wire when enabled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable identifier reported in [`crate::Verdict::matched`].
    pub id: String,
    /// Human-readable description; omitted from the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Evaluation priority — higher fires first; omitted from the wire
    /// when `0`, matching Go's `omitempty`.
    #[serde(default, skip_serializing_if = "priority_is_zero")]
    pub priority: i64,
    /// Whether the rule participates in evaluation. A disabled rule
    /// (`enabled: false`) is skipped entirely — it never matches and
    /// fires no actions. Defaults to `true`; omitted from the wire when
    /// enabled (pyfly parity).
    #[serde(
        default = "enabled_default",
        skip_serializing_if = "enabled_is_default"
    )]
    pub enabled: bool,
    /// Guard logic; an absent/empty `when` always fires.
    #[serde(default)]
    pub when: Logic,
    /// Actions emitted when `when` evaluates true.
    #[serde(default)]
    pub then: Vec<Action>,
    /// Else-branch actions emitted when `when` evaluates false (pyfly's
    /// `otherwise`). Empty by default and omitted from the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub otherwise: Vec<Action>,
}

impl Rule {
    /// Builds a rule with the given id and guard logic, no description,
    /// priority `0`, enabled, and no actions.
    pub fn new(id: impl Into<String>, when: Logic) -> Self {
        Rule {
            id: id.into(),
            description: String::new(),
            priority: 0,
            enabled: true,
            when,
            then: Vec::new(),
            otherwise: Vec::new(),
        }
    }

    /// Sets the description, builder-style.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Sets the priority, builder-style.
    #[must_use]
    pub fn with_priority(mut self, priority: i64) -> Self {
        self.priority = priority;
        self
    }

    /// Sets the `enabled` flag, builder-style. A disabled rule is
    /// skipped entirely during evaluation (pyfly parity).
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Appends one `then` action, builder-style.
    #[must_use]
    pub fn with_action(mut self, action: Action) -> Self {
        self.then.push(action);
        self
    }

    /// Appends one `otherwise` (else-branch) action, builder-style —
    /// fired when `when` evaluates false (pyfly parity).
    #[must_use]
    pub fn with_otherwise(mut self, action: Action) -> Self {
        self.otherwise.push(action);
        self
    }
}

/// Accepts any YAML/JSON scalar where a string is expected, mirroring
/// `gopkg.in/yaml.v3`, which decodes `version: 1` into a Go `string`
/// field as `"1"`. An explicit null (`version: null`, a bare
/// `version:`, JSON `"version": null`) decodes to the empty string,
/// exactly as Go leaves the field at its zero value.
fn de_stringish<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    struct StringishVisitor;
    impl Visitor<'_> for StringishVisitor {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string or scalar")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_owned())
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_unit<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_none<E: de::Error>(self) -> Result<String, E> {
            Ok(String::new())
        }
    }
    deserializer.deserialize_any(StringishVisitor)
}

/// `RuleSet` is a named collection of rules. Rules are evaluated in
/// descending priority order (higher = first); ties broken by document
/// order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RuleSet {
    /// Logical name of the document (always serialized, even when
    /// empty — Go's `name` tag has no `omitempty`).
    #[serde(default)]
    pub name: String,
    /// Free-form document version; scalars such as `version: 1` are
    /// coerced to `"1"` exactly as Go's yaml.v3 does. Omitted from the
    /// wire when empty.
    #[serde(
        default,
        deserialize_with = "de_stringish",
        skip_serializing_if = "String::is_empty"
    )]
    pub version: String,
    /// The rules, in document order.
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Builds an empty named rule set.
    pub fn new(name: impl Into<String>) -> Self {
        RuleSet {
            name: name.into(),
            ..RuleSet::default()
        }
    }

    /// Sets the version, builder-style.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Appends one rule, builder-style.
    #[must_use]
    pub fn with_rule(mut self, rule: Rule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Parses a YAML rule document (the DSL shown in the crate README)
    /// into the AST.
    pub fn from_yaml(yaml: &str) -> Result<Self, DslError> {
        serde_yaml::from_str(yaml).map_err(|e| DslError(e.to_string()))
    }

    /// Renders the AST back to a YAML rule document.
    pub fn to_yaml(&self) -> Result<String, DslError> {
        serde_yaml::to_string(self).map_err(|e| DslError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const README_YAML: &str = r#"
name: vip-tagging
version: 1
rules:
  - id: premium
    priority: 10
    when:
      all:
        - cond: { path: user.age,     op: gte, value: 18 }
        - cond: { path: user.country, op: in,  value: [ES, FR] }
    then:
      - type: tag
        params: { name: premium }
  - id: vip
    priority: 5
    when:
      any:
        - cond: { path: user.spend,    op: gt,        value: 1000 }
        - cond: { path: user.referral, op: isNotNull }
    then:
      - type: tag
        params: { name: vip }
"#;

    #[test]
    fn yaml_parse_readme_ruleset() {
        let rs = RuleSet::from_yaml(README_YAML).unwrap();
        assert_eq!(rs.name, "vip-tagging");
        // yaml.v3 parity: scalar `version: 1` decodes into the string "1".
        assert_eq!(rs.version, "1");
        assert_eq!(rs.rules.len(), 2);

        let premium = &rs.rules[0];
        assert_eq!(premium.id, "premium");
        assert_eq!(premium.priority, 10);
        assert_eq!(premium.when.all.len(), 2);
        let age = premium.when.all[0].cond.as_ref().unwrap();
        assert_eq!(age.path, "user.age");
        assert_eq!(age.op, Op::Gte);
        assert_eq!(age.value, json!(18));
        assert_eq!(premium.then[0].action_type, "tag");
        assert_eq!(premium.then[0].params["name"], json!("premium"));

        let vip = &rs.rules[1];
        assert_eq!(vip.when.any.len(), 2);
        let referral = vip.when.any[1].cond.as_ref().unwrap();
        assert_eq!(referral.op, Op::IsNotNull);
        assert_eq!(referral.value, Value::Null);
    }

    #[test]
    fn json_wire_format_matches_go_tags() {
        let rs = RuleSet::from_yaml(README_YAML).unwrap();
        // Field names and omissions must match the Go struct tags:
        // `value` omitted when nil, `description` when empty, `priority`
        // when 0, `params` when empty, `version` when empty; `name`,
        // `when` and `then` always present.
        let expected = json!({
            "name": "vip-tagging",
            "version": "1",
            "rules": [
                {
                    "id": "premium",
                    "priority": 10,
                    "when": {"all": [
                        {"cond": {"path": "user.age", "op": "gte", "value": 18}},
                        {"cond": {"path": "user.country", "op": "in", "value": ["ES", "FR"]}}
                    ]},
                    "then": [{"type": "tag", "params": {"name": "premium"}}]
                },
                {
                    "id": "vip",
                    "priority": 5,
                    "when": {"any": [
                        {"cond": {"path": "user.spend", "op": "gt", "value": 1000}},
                        {"cond": {"path": "user.referral", "op": "isNotNull"}}
                    ]},
                    "then": [{"type": "tag", "params": {"name": "vip"}}]
                }
            ]
        });
        assert_eq!(serde_json::to_value(&rs).unwrap(), expected);
    }

    #[test]
    fn json_wire_format_empty_ruleset() {
        // Go: `{"name":"","rules":null}` for the zero value — the Rust
        // port keeps `name`/`rules` mandatory but renders empty slices
        // as `[]` (serde has no nil-slice notion).
        let rs = RuleSet::default();
        assert_eq!(
            serde_json::to_value(&rs).unwrap(),
            json!({"name": "", "rules": []})
        );
    }

    #[test]
    fn json_wire_format_minimal_rule() {
        let rule = Rule::new("r1", Logic::default());
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            json!({"id": "r1", "when": {}, "then": []})
        );
    }

    #[test]
    fn yaml_round_trip() {
        let rs = RuleSet::from_yaml(README_YAML).unwrap();
        let again = RuleSet::from_yaml(&rs.to_yaml().unwrap()).unwrap();
        assert_eq!(rs, again);
    }

    #[test]
    fn json_round_trip() {
        let rs = RuleSet::from_yaml(README_YAML).unwrap();
        let text = serde_json::to_string(&rs).unwrap();
        let again: RuleSet = serde_json::from_str(&text).unwrap();
        assert_eq!(rs, again);
    }

    #[test]
    fn op_string_round_trip() {
        for op in Op::ALL {
            let s = op.as_str().to_owned();
            assert_eq!(Op::from(s.as_str()), op);
            // serde round trip through JSON
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(json, format!("{s:?}"));
            let back: Op = serde_json::from_str(&json).unwrap();
            assert_eq!(back, op);
        }
    }

    #[test]
    fn new_operators_round_trip_camel_case() {
        // The four pyfly-parity operators round-trip through their
        // canonical camelCase wire spelling.
        for (op, wire) in [
            (Op::Between, "between"),
            (Op::NotContains, "notContains"),
            (Op::Exists, "exists"),
            (Op::IsEmpty, "isEmpty"),
        ] {
            assert_eq!(op.as_str(), wire);
            assert_eq!(Op::from(wire), op);
            let json = serde_json::to_string(&op).unwrap();
            assert_eq!(json, format!("{wire:?}"));
            assert_eq!(serde_json::from_str::<Op>(&json).unwrap(), op);
        }
    }

    #[test]
    fn op_accepts_pyfly_snake_case_aliases() {
        // pyfly authors rule documents with snake_case operator
        // spellings; the Rust parser accepts them and normalises to the
        // canonical Op variant (and thus camelCase on re-serialize).
        assert_eq!(Op::from("not_in"), Op::NotIn);
        assert_eq!(Op::from("starts_with"), Op::StartsWith);
        assert_eq!(Op::from("ends_with"), Op::EndsWith);
        assert_eq!(Op::from("is_null"), Op::IsNull);
        assert_eq!(Op::from("is_not_null"), Op::IsNotNull);
        assert_eq!(Op::from("not_contains"), Op::NotContains);
        assert_eq!(Op::from("is_empty"), Op::IsEmpty);
        // pyfly spells regex `regex`; Go/Rust spell it `matches`.
        assert_eq!(Op::from("regex"), Op::Matches);
        // A pyfly snake_case condition parses through the YAML DSL.
        let rs = RuleSet::from_yaml(
            "name: x\nrules:\n  - id: r\n    when:\n      cond: { path: s, op: not_contains, value: bad }\n",
        )
        .unwrap();
        assert_eq!(rs.rules[0].when.cond.as_ref().unwrap().op, Op::NotContains);
    }

    #[test]
    fn rule_otherwise_and_enabled_wire_format() {
        // Default (enabled, no otherwise) keeps the Go-parity byte
        // stream: neither `enabled` nor `otherwise` appears.
        let plain = Rule::new("r1", Logic::default());
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            json!({"id": "r1", "when": {}, "then": []})
        );
        // A disabled rule with an otherwise branch surfaces both keys.
        let rich = Rule::new("r2", Logic::cond("x", Op::Eq, json!(1)))
            .with_enabled(false)
            .with_otherwise(Action::new("set").with_param("target", "y"));
        let v = serde_json::to_value(&rich).unwrap();
        assert_eq!(v["enabled"], json!(false));
        assert_eq!(v["otherwise"][0]["type"], json!("set"));
    }

    #[test]
    fn rule_otherwise_and_enabled_parse_from_yaml() {
        let rs = RuleSet::from_yaml(
            r#"
name: x
rules:
  - id: gated
    enabled: false
    when:
      cond: { path: tier, op: eq, value: gold }
    then:
      - type: set
        params: { target: a }
    otherwise:
      - type: set
        params: { target: b }
"#,
        )
        .unwrap();
        let rule = &rs.rules[0];
        assert!(!rule.enabled);
        assert_eq!(rule.then.len(), 1);
        assert_eq!(rule.otherwise.len(), 1);
        assert_eq!(rule.otherwise[0].params["target"], json!("b"));
    }

    #[test]
    fn rule_enabled_defaults_to_true_when_absent() {
        let rs = RuleSet::from_yaml("name: x\nrules:\n  - id: r\n").unwrap();
        assert!(rs.rules[0].enabled);
        assert!(rs.rules[0].otherwise.is_empty());
    }

    #[test]
    fn between_operand_round_trips() {
        let rs = RuleSet::from_yaml(
            "name: x\nrules:\n  - id: r\n    when:\n      cond: { path: age, op: between, value: [18, 65] }\n",
        )
        .unwrap();
        let cond = rs.rules[0].when.cond.as_ref().unwrap();
        assert_eq!(cond.op, Op::Between);
        assert_eq!(cond.value, json!([18, 65]));
    }

    #[test]
    fn op_unknown_string_parses_to_other() {
        // Op is an open string type in Go: unknown spellings survive
        // parsing and are only rejected at evaluation time.
        let op: Op = serde_json::from_str("\"fuzzy\"").unwrap();
        assert_eq!(op, Op::Other("fuzzy".into()));
        assert_eq!(op.to_string(), "fuzzy");
        assert_eq!(serde_json::to_string(&op).unwrap(), "\"fuzzy\"");
    }

    /// Regression: Go's plain `Version string` field accepts an
    /// explicit null from both `encoding/json` and yaml.v3 and leaves
    /// `Version == ""` — the Rust parser must not reject such
    /// documents.
    #[test]
    fn version_null_is_accepted_as_empty_string() {
        // JSON `"version": null`
        let rs: RuleSet = serde_json::from_str(r#"{"name":"x","version":null,"rules":[]}"#)
            .expect("JSON null version must parse");
        assert_eq!(rs.version, "");
        // YAML `version: null`
        let rs = RuleSet::from_yaml("name: x\nversion: null\nrules: []\n")
            .expect("YAML null version must parse");
        assert_eq!(rs.version, "");
        // bare `version:` (implicit YAML null)
        let rs = RuleSet::from_yaml("name: x\nversion:\nrules: []\n")
            .expect("bare version key must parse");
        assert_eq!(rs.version, "");
    }

    #[test]
    fn rule_without_when_parses_to_empty_logic() {
        let rs = RuleSet::from_yaml("name: x\nrules:\n  - id: always\n").unwrap();
        assert_eq!(rs.rules[0].when, Logic::default());
        assert!(rs.rules[0].then.is_empty());
    }

    #[test]
    fn invalid_yaml_is_rejected() {
        let err = RuleSet::from_yaml("rules: {not-a-list: true}").unwrap_err();
        assert!(err.to_string().starts_with("ruleengine: invalid rule DSL:"));
    }
}
