//! Default evaluator — the Rust counterpart of the Go
//! `ruleengine/core` package.
//!
//! [`AstEvaluator`] walks the [`Logic`] tree recursively: `cond` leaves
//! resolve their dot-separated path against the fact and apply the
//! operator; `all` / `any` / `not` combine sub-results; an empty logic
//! block evaluates **true** (a rule with no `when` always fires).
//! Rules fire in descending priority order, ties broken by document
//! order, and the merged [`Verdict`] carries the matched ids plus the
//! concatenated action list.

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::interfaces::{Evaluator, Fact, Verdict};
use crate::models::{Condition, Logic, Op, Rule, RuleSet};

/// Errors produced while evaluating a rule set.
///
/// The message spellings mirror the Go module (`ruleengine: unknown
/// op: …`, `matches: bad regex: …`, `compare gt: non-numeric (…)`,
/// each wrapped as `rule "<id>": …`), so log lines stay recognisable
/// across runtimes.
#[derive(Debug, Error, PartialEq)]
pub enum EvalError {
    /// A [`Condition`] used an [`Op`] the evaluator does not implement
    /// — Go's `ErrUnknownOp`.
    #[error("ruleengine: unknown op: {0}")]
    UnknownOp(String),
    /// The `matches` operator was given an invalid regular expression.
    #[error("matches: bad regex: {0}")]
    BadRegex(String),
    /// The `between` operator was given an operand that is not a
    /// two-element list `[lo, hi]`.
    #[error("between: value must be a 2-element list, got {0}")]
    BadBetween(String),
    /// A numeric comparison (`lt`/`lte`/`gt`/`gte`) was applied to a
    /// non-numeric operand. `left`/`right` are JSON type names.
    #[error("compare {op}: non-numeric ({left} vs {right})")]
    NonNumericCompare {
        /// The comparison operator that failed.
        op: Op,
        /// JSON type name of the fact-side operand.
        left: &'static str,
        /// JSON type name of the condition-side operand.
        right: &'static str,
    },
    /// Wraps any of the above with the id of the offending rule,
    /// mirroring Go's `fmt.Errorf("rule %q: %w", r.ID, err)`.
    #[error("rule {id:?}: {source}")]
    Rule {
        /// Id of the rule whose logic failed to evaluate.
        id: String,
        /// The underlying evaluation error.
        #[source]
        source: Box<EvalError>,
    },
}

/// Controls how many rules of a [`RuleSet`] are evaluated — the Rust
/// counterpart of pyfly's `EvaluationMode`.
///
/// Both modes walk rules in **descending priority order** (ties broken
/// by document order) and honour the [`Rule::enabled`] flag (a disabled
/// rule is skipped entirely).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvaluationMode {
    /// Evaluate **every** enabled rule (the default, Go-parity
    /// behaviour). All matching rules contribute their `then` actions
    /// and all non-matching rules contribute their `otherwise` actions.
    #[default]
    All,
    /// Evaluate rules in priority order and stop immediately after the
    /// **first matching** rule. Non-matching rules encountered before
    /// the first match are still evaluated (and their `otherwise`
    /// actions still fire); rules after the first match are never
    /// evaluated. Mirrors pyfly's `EvaluationMode.FIRST_MATCH`.
    FirstMatch,
}

/// `AstEvaluator` is the default, stateless rule-engine implementation
/// — the counterpart of Go's `core.Evaluator` (`core.New()`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AstEvaluator;

impl AstEvaluator {
    /// Returns a stateless evaluator.
    pub fn new() -> Self {
        AstEvaluator
    }

    /// Synchronous evaluation — the engine is pure CPU, so callers
    /// that are not inside an async context can use this directly. The
    /// [`Evaluator`] trait implementation delegates here.
    ///
    /// Uses [`EvaluationMode::All`]: every enabled rule is evaluated; a
    /// matched rule contributes its `then` actions and a non-matched
    /// rule its `otherwise` actions to the merged [`Verdict`]. Disabled
    /// rules ([`Rule::enabled`] = `false`) are skipped entirely.
    pub fn evaluate_sync(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError> {
        self.evaluate_with_mode(set, fact, EvaluationMode::All)
    }

    /// Evaluates `set` against `fact` under the given [`EvaluationMode`].
    ///
    /// In [`EvaluationMode::All`] every enabled rule is evaluated; in
    /// [`EvaluationMode::FirstMatch`] evaluation stops after the first
    /// matching rule. In both modes, matched rules contribute `then`
    /// actions, non-matched rules contribute `otherwise` actions, and
    /// disabled rules are skipped — the Rust counterpart of pyfly's
    /// `RuleSetEvaluator.evaluate`.
    pub fn evaluate_with_mode(
        &self,
        set: &RuleSet,
        fact: &Fact,
        mode: EvaluationMode,
    ) -> Result<Verdict, EvalError> {
        let mut rules: Vec<&Rule> = set.rules.iter().collect();
        // Stable sort: descending priority, ties keep document order.
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));

        let mut verdict = Verdict::default();
        for rule in rules {
            // pyfly parity: a disabled rule short-circuits to non-matched
            // and fires neither `then` nor `otherwise`.
            if !rule.enabled {
                continue;
            }
            let ok = self.eval(&rule.when, fact).map_err(|e| EvalError::Rule {
                id: rule.id.clone(),
                source: Box::new(e),
            })?;
            if ok {
                verdict.matched.push(rule.id.clone());
                verdict.actions.extend(rule.then.iter().cloned());
                if mode == EvaluationMode::FirstMatch {
                    break;
                }
            } else {
                // pyfly fires the else-branch when `when` is false.
                verdict.actions.extend(rule.otherwise.iter().cloned());
            }
        }
        Ok(verdict)
    }

    fn eval(&self, logic: &Logic, fact: &Fact) -> Result<bool, EvalError> {
        // Branch order matches the Go switch: cond, all, any, not.
        if let Some(cond) = &logic.cond {
            return self.eval_cond(cond, fact);
        }
        if !logic.all.is_empty() {
            for sub in &logic.all {
                if !self.eval(sub, fact)? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if !logic.any.is_empty() {
            for sub in &logic.any {
                if self.eval(sub, fact)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        if let Some(inner) = &logic.not {
            return Ok(!self.eval(inner, fact)?);
        }
        // Empty logic block evaluates true (a rule with no When always fires).
        Ok(true)
    }

    fn eval_cond(&self, cond: &Condition, fact: &Fact) -> Result<bool, EvalError> {
        let found = lookup(fact, &cond.path);
        match &cond.op {
            Op::IsNull => Ok(is_null(found)),
            Op::IsNotNull => Ok(!is_null(found)),
            Op::Eq => Ok(deep_eq(found, &cond.value)),
            Op::Ne => Ok(!deep_eq(found, &cond.value)),
            Op::Lt | Op::Lte | Op::Gt | Op::Gte => compare_num(found, &cond.value, &cond.op),
            Op::In => Ok(in_list(&cond.value, found)),
            Op::NotIn => Ok(!in_list(&cond.value, found)),
            Op::Contains => Ok(contains(found, &cond.value)),
            Op::NotContains => {
                // pyfly parity: a null/absent fact matches neither
                // `contains` nor `notContains` (both short-circuit to
                // false), so this is *not* a plain `!contains`.
                Ok(!is_null(found) && !contains(found, &cond.value))
            }
            Op::StartsWith => Ok(to_text(found).starts_with(&value_text(&cond.value))),
            Op::EndsWith => Ok(to_text(found).ends_with(&value_text(&cond.value))),
            Op::Matches => {
                let re = regex::Regex::new(&value_text(&cond.value))
                    .map_err(|e| EvalError::BadRegex(e.to_string()))?;
                Ok(re.is_match(&to_text(found)))
            }
            Op::Between => between(found, &cond.value),
            Op::Exists => Ok(!is_null(found)),
            Op::IsEmpty => Ok(is_empty(found)),
            Op::Other(name) => Err(EvalError::UnknownOp(name.clone())),
        }
    }
}

#[async_trait]
impl Evaluator for AstEvaluator {
    async fn evaluate(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError> {
        self.evaluate_sync(set, fact)
    }

    async fn evaluate_with_mode(
        &self,
        set: &RuleSet,
        fact: &Fact,
        mode: EvaluationMode,
    ) -> Result<Verdict, EvalError> {
        AstEvaluator::evaluate_with_mode(self, set, fact, mode)
    }
}

/// Resolves a dot-separated path against the fact by descending nested
/// objects. Missing keys or traversal through a non-object yield
/// `None` (Go returns `nil`).
fn lookup<'a>(fact: &'a Fact, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut current = fact.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

/// Both an absent path and an explicit JSON `null` count as null —
/// in Go both surface as an untyped `nil`.
fn is_null(v: Option<&Value>) -> bool {
    v.is_none_or(Value::is_null)
}

/// Deep equality between the looked-up fact value (absent ⇒ null) and
/// the condition operand — Go's `reflect.DeepEqual` as it behaves on
/// the reference runtime's data. In Go the fact always arrives through
/// `encoding/json`, which decodes **every** fact number as `float64`,
/// while yaml.v3 keeps an integer rule operand (`value: 18`) as `int`.
/// `reflect.DeepEqual(float64, int)` is `false` regardless of
/// magnitude, so an integer operand never equals a fact number and a
/// float operand (`value: 18.0`) equals any fact number of the same
/// magnitude — see [`go_deep_eq`].
fn deep_eq(v: Option<&Value>, operand: &Value) -> bool {
    go_deep_eq(v.unwrap_or(&Value::Null), operand)
}

/// `reflect.DeepEqual` parity between a fact-side value (whose numbers
/// Go's `encoding/json` decodes as `float64`) and an operand-side
/// value (whose numbers keep yaml.v3's int/float split): two numbers
/// are equal only when the operand is a float of the same magnitude;
/// arrays and objects recurse with the same asymmetry; all other
/// values compare structurally.
fn go_deep_eq(fact: &Value, operand: &Value) -> bool {
    match (fact, operand) {
        (Value::Number(f), Value::Number(o)) => o.is_f64() && f.as_f64() == o.as_f64(),
        (Value::Array(f), Value::Array(o)) => {
            f.len() == o.len() && f.iter().zip(o).all(|(fv, ov)| go_deep_eq(fv, ov))
        }
        (Value::Object(f), Value::Object(o)) => {
            f.len() == o.len()
                && f.iter()
                    .all(|(k, fv)| o.get(k).is_some_and(|ov| go_deep_eq(fv, ov)))
        }
        _ => fact == operand,
    }
}

/// Coerces a value to text the way Go's `toString` does with
/// `fmt.Sprintf("%v", …)`: null ⇒ empty string, strings verbatim,
/// scalars via their display form (floats via Go's `%v` float64
/// rendering — see [`go_float_text`]), composites as compact JSON.
fn to_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if n.is_f64() => go_float_text(f),
            _ => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

/// Renders an `f64` exactly the way Go's `fmt.Sprintf("%v", …)` prints
/// a `float64` (`strconv.FormatFloat(f, 'g', -1, 64)`): the shortest
/// round-trip digits in fixed notation when the decimal exponent is in
/// `[-4, 6)` (`1500.0` ⇒ `"1500"`, `0.0001` ⇒ `"0.0001"`) and in
/// scientific notation otherwise, with a signed, at-least-two-digit
/// exponent (`1e6` ⇒ `"1e+06"`, `0.00001` ⇒ `"1e-05"`).
fn go_float_text(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return (if f > 0.0 { "+Inf" } else { "-Inf" }).to_owned();
    }
    if f == 0.0 {
        return (if f.is_sign_negative() { "-0" } else { "0" }).to_owned();
    }
    // `{:e}` yields the shortest round-trip digits as `d[.ddd…]e<exp>`,
    // the same digit string Go's shortest 'g' conversion produces.
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').expect("{:e} always has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    if !(-4..6).contains(&exp) {
        let exp_sign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}e{exp_sign}{:02}", exp.abs());
    }
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let point = exp + 1; // decimal-point position within `digits`
    if point <= 0 {
        format!(
            "{sign}0.{}{digits}",
            "0".repeat(point.unsigned_abs() as usize)
        )
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    }
}

fn value_text(v: &Value) -> String {
    to_text(Some(v))
}

/// JSON type name used in [`EvalError::NonNumericCompare`] (Go prints
/// the Go type via `%T`).
fn json_type_name(v: Option<&Value>) -> &'static str {
    match v {
        None | Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// Numeric coercion — only JSON numbers qualify, exactly like Go's
/// `toFloat` (strings and bools are not coerced).
fn to_f64(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

fn compare_num(a: Option<&Value>, b: &Value, op: &Op) -> Result<bool, EvalError> {
    let (Some(af), Some(bf)) = (to_f64(a), to_f64(Some(b))) else {
        return Err(EvalError::NonNumericCompare {
            op: op.clone(),
            left: json_type_name(a),
            right: json_type_name(Some(b)),
        });
    };
    Ok(match op {
        Op::Lt => af < bf,
        Op::Lte => af <= bf,
        Op::Gt => af > bf,
        Op::Gte => af >= bf,
        // Unreachable: compare_num is only called for the four range ops.
        _ => return Err(EvalError::UnknownOp(op.to_string())),
    })
}

/// `in` / `notIn`: true when the condition operand is a list containing
/// the looked-up value (absent ⇒ null), by [`go_deep_eq`] — the fact
/// side is the needle, the operand side the list elements.
fn in_list(operand: &Value, v: Option<&Value>) -> bool {
    match operand {
        Value::Array(items) => {
            let needle = v.unwrap_or(&Value::Null);
            items.iter().any(|x| go_deep_eq(needle, x))
        }
        _ => false,
    }
}

/// `contains`: substring test when the fact value is a string,
/// [`go_deep_eq`] element test when it is a list (fact-side elements
/// vs the operand needle), false otherwise.
fn contains(haystack: Option<&Value>, needle: &Value) -> bool {
    match haystack {
        Some(Value::String(s)) => s.contains(&value_text(needle)),
        Some(Value::Array(items)) => items.iter().any(|x| go_deep_eq(x, needle)),
        _ => false,
    }
}

/// `between`: true when `lo <= fact <= hi`, where `operand` is a
/// two-element list `[lo, hi]` — pyfly's `between`.
///
/// A null/absent fact never matches (returns `Ok(false)`), mirroring
/// pyfly's `if actual is None: return False`. Numeric facts and bounds
/// compare numerically (matching the `lt`/`gt` family); string facts
/// and bounds compare lexically (Python's `<=` over `str`). An operand
/// that is not a two-element list raises [`EvalError::BadBetween`], and
/// a fact/bound type that cannot be ordered raises
/// [`EvalError::NonNumericCompare`].
fn between(fact: Option<&Value>, operand: &Value) -> Result<bool, EvalError> {
    let bounds = match operand {
        Value::Array(items) if items.len() == 2 => items,
        other => return Err(EvalError::BadBetween(other.to_string())),
    };
    // pyfly: a null/absent fact is never within range.
    if is_null(fact) {
        return Ok(false);
    }
    let lo = &bounds[0];
    let hi = &bounds[1];
    // Numeric path (consistent with the lt/gt family): coerce all three
    // operands to f64 when they are JSON numbers.
    if let (Some(f), Some(l), Some(h)) = (to_f64(fact), to_f64(Some(lo)), to_f64(Some(hi))) {
        return Ok(l <= f && f <= h);
    }
    // String path: Python compares str with `<=` lexically.
    if let (Some(Value::String(f)), Value::String(l), Value::String(h)) = (fact, lo, hi) {
        return Ok(l.as_str() <= f.as_str() && f.as_str() <= h.as_str());
    }
    Err(EvalError::NonNumericCompare {
        op: Op::Between,
        left: json_type_name(fact),
        right: json_type_name(Some(lo)),
    })
}

/// `isEmpty`: true when the fact is null/absent, the empty string, the
/// empty list, or the empty object — pyfly's `is_empty`. A numeric `0`
/// or boolean `false` is **not** empty (they are present, non-collection
/// values), matching pyfly's `actual == "" or actual == [] or actual ==
/// {}` (which is false for `0`/`False`).
fn is_empty(fact: Option<&Value>) -> bool {
    match fact {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Action;
    use serde_json::json;
    use std::sync::Arc;

    fn fact(v: serde_json::Value) -> Fact {
        v.as_object().expect("fact must be a JSON object").clone()
    }

    /// Port of Go `TestEvaluatorAndOrNot`.
    #[test]
    fn evaluator_and_or_not() {
        let rs = RuleSet::default()
            .with_rule(
                Rule::new(
                    "premium",
                    Logic::all(vec![
                        Logic::cond("user.age", Op::Gte, json!(18.0)),
                        Logic::cond("user.country", Op::In, json!(["ES", "FR"])),
                    ]),
                )
                .with_priority(10)
                .with_action(Action::new("tag").with_param("name", "premium")),
            )
            .with_rule(
                Rule::new(
                    "vip",
                    Logic::any(vec![
                        Logic::cond("user.spend", Op::Gt, json!(1000.0)),
                        Logic::cond("user.referral", Op::IsNotNull, Value::Null),
                    ]),
                )
                .with_priority(5)
                .with_action(Action::new("tag").with_param("name", "vip")),
            );
        let fact = fact(json!({
            "user": {"age": 30, "country": "ES", "spend": 500, "referral": "abc"}
        }));
        let v = AstEvaluator::new().evaluate_sync(&rs, &fact).unwrap();
        assert_eq!(v.matched, ["premium", "vip"]);
        assert_eq!(v.actions.len(), 2);
        assert_eq!(v.actions[0].params["name"], json!("premium"));
        assert_eq!(v.actions[1].params["name"], json!("vip"));
    }

    /// Port of Go `TestEvaluatorRegexAndStrings`.
    #[test]
    fn evaluator_regex_and_strings() {
        let rs = RuleSet::default().with_rule(Rule::new(
            "email-corp",
            Logic::cond("email", Op::Matches, json!(r"^.+@corp\.com$")),
        ));
        let f = fact(json!({"email": "alice@corp.com"}));
        let v = AstEvaluator::new().evaluate_sync(&rs, &f).unwrap();
        assert_eq!(v.matched, ["email-corp"]);
    }

    /// Port of Go `TestEvaluatorNot`.
    #[test]
    fn evaluator_not() {
        let rs = RuleSet::default().with_rule(Rule::new(
            "not-premium",
            Logic::not(Logic::cond("tier", Op::Eq, json!("premium"))),
        ));
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"tier": "basic"})))
            .unwrap();
        assert_eq!(v.matched, ["not-premium"]);
    }

    #[test]
    fn priority_ordering_with_document_order_ties() {
        let always = || Rule::new("", Logic::default());
        let mut rs = RuleSet::default();
        for (id, priority) in [("a", 0), ("b", 5), ("c", 5), ("d", 10)] {
            let mut r = always();
            r.id = id.into();
            r.priority = priority;
            rs.rules.push(r);
        }
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &Fact::new())
            .unwrap();
        assert_eq!(v.matched, ["d", "b", "c", "a"]);
    }

    #[test]
    fn empty_when_always_fires() {
        let rs = RuleSet::default()
            .with_rule(Rule::new("always", Logic::default()).with_action(Action::new("noop")));
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &Fact::new())
            .unwrap();
        assert_eq!(v.matched, ["always"]);
        assert_eq!(v.actions.len(), 1);
    }

    #[test]
    fn unknown_op_rejected_at_evaluation_time() {
        let rs = RuleSet::default().with_rule(Rule::new(
            "x",
            Logic::cond("a", Op::Other("fuzzy".into()), json!(1)),
        ));
        let err = AstEvaluator::new()
            .evaluate_sync(&rs, &Fact::new())
            .unwrap_err();
        assert_eq!(
            err,
            EvalError::Rule {
                id: "x".into(),
                source: Box::new(EvalError::UnknownOp("fuzzy".into())),
            }
        );
        assert_eq!(err.to_string(), "rule \"x\": ruleengine: unknown op: fuzzy");
    }

    #[test]
    fn range_operators_and_fall_through() {
        let f = fact(json!({"age": 18}));
        let cases = [
            (Op::Gte, json!(18), true),
            (Op::Gt, json!(18), false),
            (Op::Lte, json!(18), true),
            (Op::Lt, json!(18), false),
            (Op::Gt, json!(17.5), true),
            (Op::Lt, json!(18.5), true),
            // int fact vs float operand — numeric coercion bridges them
            (Op::Gte, json!(18.0), true),
        ];
        for (op, value, want) in cases {
            let rs = RuleSet::default().with_rule(Rule::new(
                "r",
                Logic::cond("age", op.clone(), value.clone()),
            ));
            let v = AstEvaluator::new().evaluate_sync(&rs, &f).unwrap();
            assert_eq!(v.matched.len() == 1, want, "op={op} value={value}");
        }
    }

    #[test]
    fn eq_and_ne_use_deep_equality() {
        let f = fact(json!({"n": 30, "tags": ["a", "b"]}));
        let eval = AstEvaluator::new();
        let run = |op: Op, path: &str, value: serde_json::Value| {
            let rs = RuleSet::default().with_rule(Rule::new("r", Logic::cond(path, op, value)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        // reflect.DeepEqual parity: Go decodes every JSON fact number
        // as float64, so the integer operand a YAML `value: 30`
        // produces never matches…
        assert!(!run(Op::Eq, "n", json!(30)));
        assert!(run(Op::Ne, "n", json!(30)));
        // …while a float operand of the same magnitude does.
        assert!(run(Op::Eq, "n", json!(30.0)));
        assert!(!run(Op::Ne, "n", json!(30.0)));
        assert!(run(Op::Eq, "tags", json!(["a", "b"])));
        // absent path equals an absent (null) operand
        assert!(run(Op::Eq, "missing", Value::Null));
    }

    /// Regression: identical wire bytes must produce the Go verdict.
    /// Go decodes every JSON fact number as `float64` while yaml.v3
    /// keeps `value: 18` as `int`, and `reflect.DeepEqual(float64,
    /// int)` is false — so an integer YAML operand never `eq`-matches
    /// a JSON fact, `ne` always holds, and `in`/`notIn`/list-`contains`
    /// behave accordingly. A float operand (`18.0`) bridges the gap.
    #[test]
    fn go_parity_yaml_int_operand_vs_json_fact() {
        let eval = AstEvaluator::new();
        let f: Fact = serde_json::from_str(r#"{"age": 18, "tags": [7, 8]}"#).unwrap();
        let run = |cond: &str| {
            let rs = RuleSet::from_yaml(&format!(
                "name: x\nrules:\n  - id: r\n    when:\n      cond: {cond}\n"
            ))
            .unwrap();
            eval.evaluate_sync(&rs, &f).unwrap().matched == ["r"]
        };
        // eq / ne with an integer YAML operand: Go never matches.
        assert!(!run("{ path: age, op: eq, value: 18 }"));
        assert!(run("{ path: age, op: ne, value: 18 }"));
        // …but a float YAML operand of the same magnitude matches.
        assert!(run("{ path: age, op: eq, value: 18.0 }"));
        assert!(!run("{ path: age, op: ne, value: 18.0 }"));
        // in / notIn follow the same DeepEqual asymmetry per element.
        assert!(!run("{ path: age, op: in, value: [18, 21] }"));
        assert!(run("{ path: age, op: in, value: [18.0, 21] }"));
        assert!(run("{ path: age, op: notIn, value: [18, 21] }"));
        // list-contains: fact-side elements are floats in Go too.
        assert!(!run("{ path: tags, op: contains, value: 7 }"));
        assert!(run("{ path: tags, op: contains, value: 7.0 }"));
    }

    /// Regression: a whole-number float fact must stringify as Go's
    /// `%v` does (`1500.0` ⇒ `"1500"`), so the string-coercing ops
    /// (`startsWith` / `endsWith` / `matches` / string-`contains`)
    /// reach the same verdict on identical wire bytes.
    #[test]
    fn go_parity_whole_number_float_string_coercion() {
        let eval = AstEvaluator::new();
        let f: Fact = serde_json::from_str(r#"{"amount": 1500.0}"#).unwrap();
        let run = |op: Op, value: serde_json::Value| {
            let rs = RuleSet::default().with_rule(Rule::new("r", Logic::cond("amount", op, value)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        assert!(run(Op::EndsWith, json!("00")));
        assert!(run(Op::StartsWith, json!("15")));
        assert!(run(Op::Matches, json!(r"^\d+$")));
        assert!(!run(Op::EndsWith, json!("00.0")));
        // string haystack `contains` coerces the needle the same way
        let f2 = fact(json!({"msg": "total 1500 EUR"}));
        let rs = RuleSet::default().with_rule(Rule::new(
            "r",
            Logic::cond("msg", Op::Contains, json!(1500.0)),
        ));
        assert_eq!(eval.evaluate_sync(&rs, &f2).unwrap().matched, ["r"]);
    }

    /// [`go_float_text`] against outputs captured from a Go probe
    /// running `fmt.Sprintf("%v", f)` on the same values.
    #[test]
    fn go_float_text_matches_go_sprintf_v() {
        let cases: [(f64, &str); 20] = [
            (1500.0, "1500"),
            (18.5, "18.5"),
            (0.5, "0.5"),
            (100000.0, "100000"),
            (999999.0, "999999"),
            (1e6, "1e+06"),
            (1234567.0, "1.234567e+06"),
            (13.5e6, "1.35e+07"),
            (1e20, "1e+20"),
            (1e21, "1e+21"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (123456789.0, "1.23456789e+08"),
            (-1500.0, "-1500"),
            (0.0, "0"),
            (-0.0, "-0"),
            (9007199254740992.0, "9.007199254740992e+15"),
            (1e300, "1e+300"),
            (2.5e-10, "2.5e-10"),
            (12345.6789, "12345.6789"),
        ];
        for (f, want) in cases {
            assert_eq!(go_float_text(f), want, "input: {f:?}");
        }
    }

    #[test]
    fn in_and_not_in() {
        let f = fact(json!({"country": "ES"}));
        let eval = AstEvaluator::new();
        let run = |op: Op, value: serde_json::Value| {
            let rs =
                RuleSet::default().with_rule(Rule::new("r", Logic::cond("country", op, value)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        assert!(run(Op::In, json!(["ES", "FR"])));
        assert!(!run(Op::In, json!(["DE", "IT"])));
        assert!(run(Op::NotIn, json!(["DE", "IT"])));
        assert!(!run(Op::NotIn, json!(["ES"])));
        // non-list operand never matches `in`
        assert!(!run(Op::In, json!("ES")));
    }

    #[test]
    fn contains_on_strings_and_lists() {
        let f = fact(json!({"msg": "hello world", "tags": ["vip", "beta"], "n": 7}));
        let eval = AstEvaluator::new();
        let run = |path: &str, value: serde_json::Value| {
            let rs = RuleSet::default()
                .with_rule(Rule::new("r", Logic::cond(path, Op::Contains, value)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        assert!(run("msg", json!("world")));
        assert!(!run("msg", json!("mars")));
        assert!(run("tags", json!("vip")));
        assert!(!run("tags", json!("gold")));
        // non-string, non-list haystack is never contained-in
        assert!(!run("n", json!(7)));
    }

    #[test]
    fn starts_with_and_ends_with() {
        let f = fact(json!({"sku": "ES-1234-X", "amount": 1500}));
        let eval = AstEvaluator::new();
        let run = |path: &str, op: Op, value: serde_json::Value| {
            let rs = RuleSet::default().with_rule(Rule::new("r", Logic::cond(path, op, value)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        assert!(run("sku", Op::StartsWith, json!("ES-")));
        assert!(run("sku", Op::EndsWith, json!("-X")));
        assert!(!run("sku", Op::StartsWith, json!("FR-")));
        // Go stringifies non-strings with %v before comparing
        assert!(run("amount", Op::StartsWith, json!("15")));
        assert!(run("amount", Op::EndsWith, json!("00")));
    }

    #[test]
    fn is_null_and_is_not_null() {
        let f = fact(json!({"a": null, "b": "x", "nested": {"deep": 1}}));
        let eval = AstEvaluator::new();
        let run = |path: &str, op: Op| {
            let rs =
                RuleSet::default().with_rule(Rule::new("r", Logic::cond(path, op, Value::Null)));
            eval.evaluate_sync(&rs, &f).unwrap().matched.len() == 1
        };
        assert!(run("a", Op::IsNull)); // explicit null
        assert!(run("missing", Op::IsNull)); // absent key
        assert!(run("b.inner", Op::IsNull)); // traversal through non-object
        assert!(run("b", Op::IsNotNull));
        assert!(run("nested.deep", Op::IsNotNull));
        assert!(!run("a", Op::IsNotNull));
    }

    /// Helper: evaluate a single-condition rule and report whether it
    /// matched. Ports pyfly's `_eval(cond, ctx)` from
    /// `tests/rule_engine/test_operators.py`.
    fn op_matches(path: &str, op: Op, value: serde_json::Value, ctx: serde_json::Value) -> bool {
        let rs = RuleSet::default().with_rule(Rule::new("t", Logic::cond(path, op, value)));
        AstEvaluator::new()
            .evaluate_sync(&rs, &fact(ctx))
            .unwrap()
            .matched
            .len()
            == 1
    }

    /// Port of pyfly `TestBetween` (test_operators.py). Note: the Rust
    /// port coerces JSON numbers numerically (a `[1, 10]` int operand
    /// works against an int/float fact alike — unlike `eq`, which uses
    /// DeepEqual).
    #[test]
    fn between_operator() {
        assert!(op_matches(
            "x",
            Op::Between,
            json!([1, 10]),
            json!({"x": 5})
        ));
        assert!(op_matches(
            "x",
            Op::Between,
            json!([1, 10]),
            json!({"x": 1})
        )); // lower bound
        assert!(op_matches(
            "x",
            Op::Between,
            json!([1, 10]),
            json!({"x": 10})
        )); // upper bound
        assert!(!op_matches(
            "x",
            Op::Between,
            json!([5, 10]),
            json!({"x": 4})
        )); // below
        assert!(!op_matches(
            "x",
            Op::Between,
            json!([5, 10]),
            json!({"x": 11})
        )); // above
            // None/missing field is false (never crashes).
        assert!(!op_matches(
            "missing",
            Op::Between,
            json!([1, 10]),
            json!({})
        ));
        // Float fact and float bounds also work.
        assert!(op_matches(
            "x",
            Op::Between,
            json!([1.0, 10.0]),
            json!({"x": 5.5})
        ));
        // String range (Python-style lexical comparison).
        assert!(op_matches(
            "s",
            Op::Between,
            json!(["a", "m"]),
            json!({"s": "f"})
        ));
        assert!(!op_matches(
            "s",
            Op::Between,
            json!(["a", "m"]),
            json!({"s": "z"})
        ));
    }

    #[test]
    fn between_bad_operand_is_an_error() {
        // A non-2-element operand fails loudly at evaluation time.
        for bad in [json!(5), json!([1, 2, 3]), json!("nope")] {
            let rs =
                RuleSet::default().with_rule(Rule::new("r", Logic::cond("x", Op::Between, bad)));
            let err = AstEvaluator::new()
                .evaluate_sync(&rs, &fact(json!({"x": 5})))
                .unwrap_err();
            assert!(err.to_string().contains("between"), "message: {err}");
        }
    }

    /// Port of pyfly `TestNotContains` (test_operators.py).
    #[test]
    fn not_contains_operator() {
        // substring absent → true; present → false
        assert!(op_matches(
            "s",
            Op::NotContains,
            json!("bad"),
            json!({"s": "good text"})
        ));
        assert!(!op_matches(
            "s",
            Op::NotContains,
            json!("bad"),
            json!({"s": "this is bad"})
        ));
        // list member absent → true; present → false
        assert!(op_matches(
            "tags",
            Op::NotContains,
            json!("blocked"),
            json!({"tags": ["active", "vip"]})
        ));
        assert!(!op_matches(
            "tags",
            Op::NotContains,
            json!("blocked"),
            json!({"tags": ["active", "blocked"]})
        ));
        // None/missing field is false (pyfly: neither contains nor
        // not_contains holds for a null fact).
        assert!(!op_matches(
            "missing",
            Op::NotContains,
            json!("x"),
            json!({})
        ));
    }

    /// Port of pyfly `TestExists` (test_operators.py).
    #[test]
    fn exists_operator() {
        assert!(op_matches(
            "name",
            Op::Exists,
            Value::Null,
            json!({"name": "Alice"})
        ));
        assert!(!op_matches("name", Op::Exists, Value::Null, json!({}))); // absent
        assert!(!op_matches(
            "name",
            Op::Exists,
            Value::Null,
            json!({"name": null})
        )); // explicit null
            // Falsy-but-present value still exists; the operand is ignored.
        assert!(op_matches(
            "x",
            Op::Exists,
            json!("anything"),
            json!({"x": 0})
        ));
    }

    /// Port of pyfly `TestIsEmpty` (test_operators.py).
    #[test]
    fn is_empty_operator() {
        // Empty variants → true (null, "", [], {}); absent → true.
        for v in [json!(null), json!(""), json!([]), json!({})] {
            assert!(
                op_matches("x", Op::IsEmpty, Value::Null, json!({ "x": v.clone() })),
                "value {v} should be empty"
            );
        }
        assert!(op_matches("x", Op::IsEmpty, Value::Null, json!({}))); // absent → null → empty
                                                                       // Non-empty variants → false (incl. `0` and `false`, which are
                                                                       // present non-collection values).
        for v in [
            json!("hello"),
            json!([1]),
            json!({"a": 1}),
            json!(0),
            json!(false),
        ] {
            assert!(
                !op_matches("x", Op::IsEmpty, Value::Null, json!({ "x": v.clone() })),
                "value {v} should NOT be empty"
            );
        }
    }

    #[test]
    fn non_numeric_compare_is_an_error() {
        let rs =
            RuleSet::default().with_rule(Rule::new("r", Logic::cond("name", Op::Gt, json!(10))));
        let err = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"name": "alice"})))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "rule \"r\": compare gt: non-numeric (string vs number)"
        );
    }

    #[test]
    fn missing_path_numeric_compare_is_an_error() {
        // Go parity: toFloat(nil) fails, and errors propagate out of
        // any/all instead of being treated as a non-match.
        let rs = RuleSet::default().with_rule(Rule::new(
            "r",
            Logic::any(vec![
                Logic::cond("user.spend", Op::Gt, json!(1000.0)),
                Logic::cond("user.referral", Op::IsNotNull, Value::Null),
            ]),
        ));
        let err = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"user": {"referral": "abc"}})))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "rule \"r\": compare gt: non-numeric (null vs number)"
        );
    }

    #[test]
    fn bad_regex_is_an_error() {
        let rs = RuleSet::default().with_rule(Rule::new(
            "r",
            Logic::cond("email", Op::Matches, json!("(")),
        ));
        let err = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"email": "x"})))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with("rule \"r\": matches: bad regex:"),
            "message: {msg}"
        );
    }

    #[test]
    fn nested_all_any_not_composition() {
        // (age >= 18 AND NOT (country in [US])) OR vip == true
        let logic = Logic::any(vec![
            Logic::all(vec![
                Logic::cond("age", Op::Gte, json!(18)),
                Logic::not(Logic::cond("country", Op::In, json!(["US"]))),
            ]),
            Logic::cond("vip", Op::Eq, json!(true)),
        ]);
        let rs = RuleSet::default().with_rule(Rule::new("r", logic));
        let eval = AstEvaluator::new();
        let hit =
            |f: serde_json::Value| eval.evaluate_sync(&rs, &fact(f)).unwrap().matched.len() == 1;
        assert!(hit(json!({"age": 30, "country": "ES", "vip": false})));
        assert!(!hit(json!({"age": 30, "country": "US", "vip": false})));
        assert!(hit(json!({"age": 10, "country": "US", "vip": true})));
        assert!(!hit(json!({"age": 10, "country": "US", "vip": false})));
    }

    #[test]
    fn yaml_dsl_end_to_end() {
        // The README document, parsed and evaluated in one go.
        let rs = RuleSet::from_yaml(
            r#"
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
"#,
        )
        .unwrap();
        let f = fact(json!({
            "user": {"age": 30, "country": "ES", "spend": 500, "referral": "abc"}
        }));
        let v = AstEvaluator::new().evaluate_sync(&rs, &f).unwrap();
        assert_eq!(v.matched, ["premium", "vip"]);
    }

    #[tokio::test]
    async fn evaluator_trait_is_object_safe() {
        let eval: Arc<dyn Evaluator> = Arc::new(AstEvaluator::new());
        let rs =
            RuleSet::default().with_rule(Rule::new("r", Logic::cond("ok", Op::Eq, json!(true))));
        let v = eval
            .evaluate(&rs, &fact(json!({"ok": true})))
            .await
            .unwrap();
        assert_eq!(v.matched, ["r"]);
    }

    #[test]
    fn evaluator_trait_usable_without_tokio() {
        // The default engine never awaits, so a lightweight executor
        // is sufficient — proves the trait does not require a runtime.
        let eval: Arc<dyn Evaluator> = Arc::new(AstEvaluator::new());
        let rs = RuleSet::default().with_rule(Rule::new("r", Logic::default()));
        let v = futures::executor::block_on(eval.evaluate(&rs, &Fact::new())).unwrap();
        assert_eq!(v.matched, ["r"]);
    }

    // ----- EvaluationMode (ports pyfly test_modes.py) ---------------------

    /// Two rules that both match a `tier == gold` fact; high priority
    /// fires first.
    fn modes_ruleset() -> RuleSet {
        RuleSet::default()
            .with_rule(
                Rule::new("high", Logic::cond("tier", Op::Eq, json!("gold")))
                    .with_priority(10)
                    .with_action(
                        Action::new("set")
                            .with_param("target", "high_ran")
                            .with_param("value", true),
                    ),
            )
            .with_rule(
                Rule::new("low", Logic::cond("tier", Op::Eq, json!("gold")))
                    .with_priority(1)
                    .with_action(
                        Action::new("set")
                            .with_param("target", "low_ran")
                            .with_param("value", true),
                    ),
            )
    }

    #[test]
    fn all_mode_evaluates_every_matching_rule() {
        let v = AstEvaluator::new()
            .evaluate_with_mode(
                &modes_ruleset(),
                &fact(json!({"tier": "gold"})),
                EvaluationMode::All,
            )
            .unwrap();
        assert_eq!(v.matched, ["high", "low"]);
        assert_eq!(v.actions.len(), 2);
    }

    #[test]
    fn all_is_the_default_mode() {
        // evaluate_sync == evaluate_with_mode(ALL).
        let f = fact(json!({"tier": "gold"}));
        let a = AstEvaluator::new()
            .evaluate_sync(&modes_ruleset(), &f)
            .unwrap();
        let b = AstEvaluator::new()
            .evaluate_with_mode(&modes_ruleset(), &f, EvaluationMode::default())
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.matched, ["high", "low"]);
    }

    #[test]
    fn first_match_stops_after_first_matching_rule() {
        let v = AstEvaluator::new()
            .evaluate_with_mode(
                &modes_ruleset(),
                &fact(json!({"tier": "gold"})),
                EvaluationMode::FirstMatch,
            )
            .unwrap();
        // Only the high-priority rule fires; low is never reached.
        assert_eq!(v.matched, ["high"]);
        assert_eq!(v.actions.len(), 1);
        assert_eq!(v.actions[0].params["target"], json!("high_ran"));
    }

    #[test]
    fn first_match_continues_past_non_matching_rules() {
        // high does NOT match (platinum) → evaluation continues; low
        // matches → stops.
        let rs = RuleSet::default()
            .with_rule(
                Rule::new("high", Logic::cond("tier", Op::Eq, json!("platinum"))).with_priority(10),
            )
            .with_rule(
                Rule::new("low", Logic::cond("tier", Op::Eq, json!("gold"))).with_priority(1),
            );
        let v = AstEvaluator::new()
            .evaluate_with_mode(
                &rs,
                &fact(json!({"tier": "gold"})),
                EvaluationMode::FirstMatch,
            )
            .unwrap();
        assert_eq!(v.matched, ["low"]);
    }

    #[test]
    fn first_match_no_rule_matches_evaluates_all() {
        let v = AstEvaluator::new()
            .evaluate_with_mode(
                &modes_ruleset(),
                &fact(json!({"tier": "bronze"})),
                EvaluationMode::FirstMatch,
            )
            .unwrap();
        assert!(v.matched.is_empty());
        assert!(v.actions.is_empty());
    }

    // ----- enabled / otherwise (pyfly Rule.enabled / Rule.otherwise) -------

    #[test]
    fn disabled_rule_is_skipped_entirely() {
        let rs = RuleSet::default().with_rule(
            Rule::new("off", Logic::default())
                .with_enabled(false)
                .with_action(
                    Action::new("set")
                        .with_param("target", "x")
                        .with_param("value", 1),
                )
                .with_otherwise(
                    Action::new("set")
                        .with_param("target", "y")
                        .with_param("value", 2),
                ),
        );
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &Fact::new())
            .unwrap();
        // A disabled rule never matches and fires neither then nor otherwise.
        assert!(v.matched.is_empty());
        assert!(v.actions.is_empty());
    }

    #[test]
    fn otherwise_actions_fire_when_when_is_false() {
        let rs = RuleSet::default().with_rule(
            Rule::new("r", Logic::cond("tier", Op::Eq, json!("gold")))
                .with_action(Action::new("set").with_param("target", "then_ran"))
                .with_otherwise(Action::new("set").with_param("target", "else_ran")),
        );
        // when=false (tier != gold) → otherwise fires, rule is NOT matched.
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"tier": "bronze"})))
            .unwrap();
        assert!(v.matched.is_empty(), "non-match must not be in matched");
        assert_eq!(v.actions.len(), 1);
        assert_eq!(v.actions[0].params["target"], json!("else_ran"));
    }

    #[test]
    fn then_actions_fire_when_when_is_true_not_otherwise() {
        let rs = RuleSet::default().with_rule(
            Rule::new("r", Logic::cond("tier", Op::Eq, json!("gold")))
                .with_action(Action::new("set").with_param("target", "then_ran"))
                .with_otherwise(Action::new("set").with_param("target", "else_ran")),
        );
        let v = AstEvaluator::new()
            .evaluate_sync(&rs, &fact(json!({"tier": "gold"})))
            .unwrap();
        assert_eq!(v.matched, ["r"]);
        assert_eq!(v.actions.len(), 1);
        assert_eq!(v.actions[0].params["target"], json!("then_ran"));
    }

    #[test]
    fn send_sync_bounds() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AstEvaluator>();
        assert_send_sync::<RuleSet>();
        assert_send_sync::<Verdict>();
        assert_send_sync::<EvalError>();
    }
}
