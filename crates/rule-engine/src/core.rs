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
    pub fn evaluate_sync(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError> {
        let mut rules: Vec<&Rule> = set.rules.iter().collect();
        // Stable sort: descending priority, ties keep document order.
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));

        let mut verdict = Verdict::default();
        for rule in rules {
            let ok = self.eval(&rule.when, fact).map_err(|e| EvalError::Rule {
                id: rule.id.clone(),
                source: Box::new(e),
            })?;
            if ok {
                verdict.matched.push(rule.id.clone());
                verdict.actions.extend(rule.then.iter().cloned());
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
            Op::StartsWith => Ok(to_text(found).starts_with(&value_text(&cond.value))),
            Op::EndsWith => Ok(to_text(found).ends_with(&value_text(&cond.value))),
            Op::Matches => {
                let re = regex::Regex::new(&value_text(&cond.value))
                    .map_err(|e| EvalError::BadRegex(e.to_string()))?;
                Ok(re.is_match(&to_text(found)))
            }
            Op::Other(name) => Err(EvalError::UnknownOp(name.clone())),
        }
    }
}

#[async_trait]
impl Evaluator for AstEvaluator {
    async fn evaluate(&self, set: &RuleSet, fact: &Fact) -> Result<Verdict, EvalError> {
        self.evaluate_sync(set, fact)
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
/// the condition operand — Go's `reflect.DeepEqual`. As in Go, an
/// integer never equals a float of the same magnitude (`30 != 30.0`).
fn deep_eq(v: Option<&Value>, operand: &Value) -> bool {
    v.unwrap_or(&Value::Null) == operand
}

/// Coerces a value to text the way Go's `toString` does with
/// `fmt.Sprintf("%v", …)`: null ⇒ empty string, strings verbatim,
/// scalars via their display form, composites as compact JSON.
fn to_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
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
/// the looked-up value (absent ⇒ null), by deep equality.
fn in_list(operand: &Value, v: Option<&Value>) -> bool {
    match operand {
        Value::Array(items) => {
            let needle = v.unwrap_or(&Value::Null);
            items.iter().any(|x| x == needle)
        }
        _ => false,
    }
}

/// `contains`: substring test when the fact value is a string,
/// deep-equality element test when it is a list, false otherwise.
fn contains(haystack: Option<&Value>, needle: &Value) -> bool {
    match haystack {
        Some(Value::String(s)) => s.contains(&value_text(needle)),
        Some(Value::Array(items)) => items.iter().any(|x| x == needle),
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
        assert!(run(Op::Eq, "n", json!(30)));
        // reflect.DeepEqual parity: int 30 != float 30.0
        assert!(!run(Op::Eq, "n", json!(30.0)));
        assert!(run(Op::Ne, "n", json!(30.0)));
        assert!(run(Op::Eq, "tags", json!(["a", "b"])));
        // absent path equals an absent (null) operand
        assert!(run(Op::Eq, "missing", Value::Null));
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

    #[test]
    fn send_sync_bounds() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AstEvaluator>();
        assert_send_sync::<RuleSet>();
        assert_send_sync::<Verdict>();
        assert_send_sync::<EvalError>();
    }
}
