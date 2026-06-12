//! Static validation for a [`RuleSet`] — the Rust counterpart of
//! pyfly's `rule_engine.validation` module.
//!
//! The Go-parity AST rejects only *malformed DSL* at parse time (a
//! [`DslError`](crate::DslError)); it never lints the **shape** of an
//! otherwise-parseable rule set. pyfly adds a static linter that returns
//! a list of human-readable issues a consumer can run before deploying a
//! rule set. This module ports that linter, adapted to the Rust AST.
//!
//! Use [`validate_ruleset`] for the issue list, or [`RuleSetValidator`]
//! for the object-oriented interface that also provides
//! [`RuleSetValidator::assert_valid`] (raises
//! [`RuleValidationError`]).
//!
//! ## Checks
//!
//! Mirroring pyfly (adapted to the Rust [`Logic`] tree), the validator
//! reports:
//!
//! * **Duplicate rule ids** — two rules sharing an `id`.
//! * **Unknown operator** — a [`Condition`] whose [`Op`] is
//!   [`Op::Other`] (an operator the evaluator does not implement).
//! * **Malformed `between`** — a `between` condition whose operand is
//!   not a two-element JSON array `[lo, hi]`.
//! * **Ambiguous logic node** — a [`Logic`] node that populates more
//!   than one of `cond` / `all` / `any` / `not` at once (the evaluator
//!   would silently pick `cond` → `all` → `any` → `not` and ignore the
//!   rest), the Rust analog of pyfly's malformed-compound check.
//! * **Missing action target** — a `set` / `increment` action without a
//!   `target` parameter.
//! * **Unknown action type** — an action whose `type` is not one of the
//!   known types (`set` / `increment` / `log` / `call` / `calculate`).
//!
//! ```rust
//! use firefly_rule_engine::{Logic, Rule, RuleSet};
//! use firefly_rule_engine::validation::{validate_ruleset, RuleSetValidator};
//!
//! let rs = RuleSet::new("orders")
//!     .with_rule(Rule::new("dup", Logic::default()))
//!     .with_rule(Rule::new("dup", Logic::default()));
//! let issues = validate_ruleset(&rs);
//! assert!(issues.iter().any(|i| i.contains("duplicate") && i.contains("dup")));
//! assert_eq!(RuleSetValidator::check(&rs).len(), issues.len());
//! ```

use std::collections::HashSet;

use thiserror::Error;

use crate::models::{Action, Logic, Op, RuleSet};

/// The action `type`s the validator recognises — the Rust counterpart
/// of pyfly's `_KNOWN_ACTION_TYPES`. `set` / `increment` / `log` are
/// builtins; `call` / `calculate` are documented extension points a
/// consumer wires via a custom handler.
const KNOWN_ACTION_TYPES: [&str; 5] = ["set", "increment", "log", "call", "calculate"];

/// The action `type`s that require a `target` parameter — pyfly's
/// `_TARGET_REQUIRED`.
const TARGET_REQUIRED: [&str; 2] = ["set", "increment"];

/// Raised by [`RuleSetValidator::assert_valid`] when a rule set has one
/// or more validation issues — the Rust counterpart of pyfly's
/// `RuleValidationError`.
///
/// Carries the offending rule set's [`RuleSet::name`] and the full list
/// of human-readable issues; the [`Display`](std::fmt::Display) form
/// joins them with `"; "`, matching pyfly's message shape.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("RuleSet {ruleset_name:?} has {} validation issue(s): {}", .issues.len(), .issues.join("; "))]
pub struct RuleValidationError {
    /// [`RuleSet::name`] of the rule set that failed validation.
    pub ruleset_name: String,
    /// The human-readable validation issues, in discovery order.
    pub issues: Vec<String>,
}

/// Returns a list of human-readable validation issues for `ruleset` —
/// the Rust counterpart of pyfly's `validate_ruleset`.
///
/// An empty list means the rule set is valid. See the [module
/// docs](crate::validation) for the full list of checks.
pub fn validate_ruleset(ruleset: &RuleSet) -> Vec<String> {
    RuleSetValidator::check(ruleset)
}

/// Object-oriented wrapper around [`validate_ruleset`] — the Rust
/// counterpart of pyfly's `RuleSetValidator`.
#[derive(Debug, Clone, Copy)]
pub struct RuleSetValidator;

impl RuleSetValidator {
    /// Returns the validation issues for `ruleset`; an empty list means
    /// the rule set is valid.
    pub fn check(ruleset: &RuleSet) -> Vec<String> {
        let mut issues = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();

        for rule in &ruleset.rules {
            if !seen.insert(rule.id.as_str()) {
                issues.push(format!("duplicate rule id {:?}", rule.id));
            }
            check_logic(&rule.when, &rule.id, &mut issues);
            for action in &rule.then {
                check_action(action, &rule.id, "then", &mut issues);
            }
            for action in &rule.otherwise {
                check_action(action, &rule.id, "otherwise", &mut issues);
            }
        }
        issues
    }

    /// Raises [`RuleValidationError`] if `ruleset` has any validation
    /// issues; returns `Ok(())` otherwise — pyfly's `assert_valid`.
    pub fn assert_valid(ruleset: &RuleSet) -> Result<(), RuleValidationError> {
        let issues = RuleSetValidator::check(ruleset);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(RuleValidationError {
                ruleset_name: ruleset.name.clone(),
                issues,
            })
        }
    }
}

/// Recursively validates a [`Logic`] node.
///
/// Empty logic (the "always fires" node) is valid. A node that sets
/// more than one branch is ambiguous and reported. Each populated
/// branch is recursed into, and every leaf [`Condition`] has its
/// operator and `between` operand checked.
fn check_logic(logic: &Logic, rule_id: &str, issues: &mut Vec<String>) {
    let branches = usize::from(logic.cond.is_some())
        + usize::from(!logic.all.is_empty())
        + usize::from(!logic.any.is_empty())
        + usize::from(logic.not.is_some());
    if branches > 1 {
        issues.push(format!(
            "rule {rule_id:?}: ambiguous logic node sets {branches} branches; \
             exactly one of cond/all/any/not is expected"
        ));
    }

    if let Some(cond) = &logic.cond {
        check_condition_op(&cond.op, &cond.value, rule_id, issues);
    }
    for sub in &logic.all {
        check_logic(sub, rule_id, issues);
    }
    for sub in &logic.any {
        check_logic(sub, rule_id, issues);
    }
    if let Some(inner) = &logic.not {
        check_logic(inner, rule_id, issues);
    }
}

/// Validates a single leaf condition's operator (and the `between`
/// operand shape).
fn check_condition_op(op: &Op, value: &serde_json::Value, rule_id: &str, issues: &mut Vec<String>) {
    if let Op::Other(name) = op {
        issues.push(format!("rule {rule_id:?}: unknown operator {name:?}"));
        return;
    }
    if *op == Op::Between {
        let ok = matches!(value, serde_json::Value::Array(items) if items.len() == 2);
        if !ok {
            issues.push(format!(
                "rule {rule_id:?}: 'between' value must be a 2-element list, got {value}"
            ));
        }
    }
}

/// Validates a single action: known type and required `target`.
fn check_action(action: &Action, rule_id: &str, branch: &str, issues: &mut Vec<String>) {
    let action_type = action.action_type.as_str();
    if !KNOWN_ACTION_TYPES.contains(&action_type) {
        issues.push(format!(
            "rule {rule_id:?} ({branch}): unknown action type {action_type:?}"
        ));
    }
    if TARGET_REQUIRED.contains(&action_type) {
        let has_target = action
            .params
            .get("target")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if !has_target {
            issues.push(format!(
                "rule {rule_id:?} ({branch}): '{action_type}' action missing 'target'"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Condition, Rule};
    use serde_json::json;

    fn rule(id: &str) -> Rule {
        Rule::new(id, Logic::default())
    }

    // ----- valid case (ports pyfly TestValidatorValidRuleset) -------------

    #[test]
    fn empty_issues_for_valid_ruleset() {
        let rs = RuleSet::new("rs")
            .with_rule(
                Rule::new("r1", Logic::cond("x", Op::Gt, json!(5))).with_action(
                    Action::new("set")
                        .with_param("target", "y")
                        .with_param("value", 1),
                ),
            )
            .with_rule(Rule::new(
                "r2",
                Logic::cond("x", Op::Between, json!([1, 10])),
            ));
        assert_eq!(validate_ruleset(&rs), Vec::<String>::new());
    }

    #[test]
    fn assert_valid_does_not_raise_for_valid() {
        let rs = RuleSet::new("rs").with_rule(rule("r1"));
        RuleSetValidator::assert_valid(&rs).expect("valid ruleset must not raise");
    }

    // ----- invalid cases (ports pyfly TestValidatorInvalidCases) ----------

    #[test]
    fn duplicate_rule_ids() {
        let rs = RuleSet::new("rs")
            .with_rule(rule("dup"))
            .with_rule(rule("dup"));
        let issues = validate_ruleset(&rs);
        assert!(
            issues
                .iter()
                .any(|i| i.contains("duplicate") && i.contains("dup")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn unknown_operator() {
        let rs = RuleSet::new("rs").with_rule(Rule::new(
            "r1",
            Logic::cond("x", Op::Other("fuzzy_match".into()), json!("abc")),
        ));
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("fuzzy_match")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn missing_target_on_set() {
        let rs = RuleSet::new("rs").with_rule(
            Rule::new("r1", Logic::default())
                .with_action(Action::new("set").with_param("value", 1)),
        );
        let issues = validate_ruleset(&rs);
        assert!(
            issues
                .iter()
                .any(|i| i.contains("target") && i.contains("set")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn missing_target_on_increment() {
        let rs = RuleSet::new("rs")
            .with_rule(Rule::new("r1", Logic::default()).with_action(Action::new("increment")));
        let issues = validate_ruleset(&rs);
        assert!(
            issues
                .iter()
                .any(|i| i.contains("target") && i.contains("increment")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn bad_between_value_not_two_elements() {
        let rs = RuleSet::new("rs").with_rule(Rule::new(
            "r1",
            Logic::cond("x", Op::Between, json!([1, 2, 3])),
        ));
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("between")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn bad_between_value_scalar() {
        let rs =
            RuleSet::new("rs").with_rule(Rule::new("r1", Logic::cond("x", Op::Between, json!(5))));
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("between")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn unknown_action_type() {
        let rs = RuleSet::new("rs").with_rule(
            Rule::new("r1", Logic::default())
                .with_action(Action::new("teleport").with_param("target", "x")),
        );
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("teleport")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn ambiguous_logic_node_is_reported() {
        // The Rust analog of pyfly's "compound op with no children" /
        // "not with two children": a Logic node that sets more than one
        // branch at once.
        let logic = Logic {
            all: vec![Logic::cond("a", Op::Eq, json!(1))],
            any: vec![Logic::cond("b", Op::Eq, json!(2))],
            ..Logic::default()
        };
        let rs = RuleSet::new("rs").with_rule(Rule::new("r1", logic));
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("ambiguous")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn empty_logic_is_valid() {
        // A bare "always fires" rule (empty when) is valid — it is the
        // Rust analog of pyfly's `Rule(id="r1")` with no condition.
        let rs = RuleSet::new("rs").with_rule(rule("only"));
        assert_eq!(validate_ruleset(&rs), Vec::<String>::new());
    }

    #[test]
    fn nested_unknown_operator_in_compound_is_reported() {
        let logic = Logic::all(vec![
            Logic::cond("a", Op::Eq, json!(1)),
            Logic::not(Logic::cond("b", Op::Other("weird".into()), json!(2))),
        ]);
        let rs = RuleSet::new("rs").with_rule(Rule::new("r1", logic));
        let issues = validate_ruleset(&rs);
        assert!(
            issues.iter().any(|i| i.contains("weird")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn otherwise_branch_actions_are_validated() {
        let rs = RuleSet::new("rs").with_rule(
            Rule::new("r1", Logic::default()).with_otherwise(Action::new("set")), // missing target
        );
        let issues = validate_ruleset(&rs);
        assert!(
            issues
                .iter()
                .any(|i| i.contains("otherwise") && i.contains("target")),
            "issues: {issues:?}"
        );
    }

    #[test]
    fn multiple_issues_all_reported() {
        let rs = RuleSet::new("rs").with_rule(rule("dup")).with_rule(
            Rule::new(
                "dup",
                Logic::cond("x", Op::Other("bad_op".into()), json!(null)),
            )
            .with_action(Action::new("set")),
        );
        let issues = validate_ruleset(&rs);
        // duplicate + unknown op + missing target
        assert!(issues.len() >= 3, "issues: {issues:?}");
    }

    #[test]
    fn assert_valid_raises_rule_validation_error() {
        let rs = RuleSet::new("rs")
            .with_rule(rule("dup"))
            .with_rule(rule("dup"));
        let err = RuleSetValidator::assert_valid(&rs).unwrap_err();
        assert_eq!(err.ruleset_name, "rs");
        assert!(!err.issues.is_empty());
        assert!(err.to_string().contains("dup"), "display: {err}");
    }

    #[test]
    fn from_dict_style_condition_validates() {
        // A Condition built directly (the Rust analog of
        // Condition.from_dict) is validated the same way.
        let cond = Condition::new("x", Op::Between, json!([1, 10]));
        let rs = RuleSet::new("rs").with_rule(Rule::new(
            "r1",
            Logic {
                cond: Some(cond),
                ..Logic::default()
            },
        ));
        assert_eq!(validate_ruleset(&rs), Vec::<String>::new());
    }
}
