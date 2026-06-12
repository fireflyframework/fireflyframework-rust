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

//! End-to-end pyfly-parity tests exercised through the crate's **public**
//! surface only.
//!
//! These port the behaviour contract of pyfly's
//! `tests/rule_engine/{test_operators,test_modes,test_loading_and_validation}.py`
//! onto the Rust public API: the four added leaf operators (`between`,
//! `notContains`, `exists`, `isEmpty`), the [`EvaluationMode`] (ALL vs
//! FIRST_MATCH), the `otherwise` / `enabled` rule fields, and the
//! [`validate_ruleset`] linter.

use firefly_rule_engine::{
    validate_ruleset, Action, AstEvaluator, EvaluationMode, Fact, Logic, Op, Rule,
    RuleEngineService, RuleSet, RuleSetValidator,
};
use serde_json::{json, Value};

fn fact(v: Value) -> Fact {
    v.as_object().expect("fact must be a JSON object").clone()
}

/// Evaluate a one-condition rule via the public `AstEvaluator` and report
/// whether it matched — the public-API analog of pyfly's `_eval`.
fn op_matches(path: &str, op: Op, value: Value, ctx: Value) -> bool {
    let rs = RuleSet::default().with_rule(Rule::new("t", Logic::cond(path, op, value)));
    AstEvaluator::new()
        .evaluate_sync(&rs, &fact(ctx))
        .unwrap()
        .matched
        == ["t"]
}

// ---------------------------------------------------------------------------
// Operators (ports test_operators.py)
// ---------------------------------------------------------------------------

#[test]
fn between_within_and_outside_range() {
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
    ));
    assert!(op_matches(
        "x",
        Op::Between,
        json!([1, 10]),
        json!({"x": 10})
    ));
    assert!(!op_matches(
        "x",
        Op::Between,
        json!([5, 10]),
        json!({"x": 4})
    ));
    assert!(!op_matches(
        "x",
        Op::Between,
        json!([5, 10]),
        json!({"x": 11})
    ));
    assert!(!op_matches(
        "missing",
        Op::Between,
        json!([1, 10]),
        json!({})
    ));
}

#[test]
fn not_contains_substring_and_list() {
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
    assert!(!op_matches(
        "missing",
        Op::NotContains,
        json!("x"),
        json!({})
    ));
}

#[test]
fn exists_present_absent_and_null() {
    assert!(op_matches(
        "name",
        Op::Exists,
        Value::Null,
        json!({"name": "Alice"})
    ));
    assert!(!op_matches("name", Op::Exists, Value::Null, json!({})));
    assert!(!op_matches(
        "name",
        Op::Exists,
        Value::Null,
        json!({"name": null})
    ));
    assert!(op_matches("x", Op::Exists, Value::Null, json!({"x": 0})));
}

#[test]
fn is_empty_variants() {
    for v in [json!(null), json!(""), json!([]), json!({})] {
        assert!(op_matches("x", Op::IsEmpty, Value::Null, json!({ "x": v })));
    }
    assert!(op_matches("x", Op::IsEmpty, Value::Null, json!({})));
    for v in [
        json!("hello"),
        json!([1]),
        json!({"a": 1}),
        json!(0),
        json!(false),
    ] {
        assert!(!op_matches(
            "x",
            Op::IsEmpty,
            Value::Null,
            json!({ "x": v })
        ));
    }
}

#[test]
fn new_operators_via_yaml_snake_case() {
    // pyfly authors operators in snake_case; the DSL parses them.
    let rs = RuleSet::from_yaml(
        "name: x\nrules:\n  - id: r\n    when:\n      cond: { path: tags, op: not_contains, value: blocked }\n",
    )
    .unwrap();
    let v = AstEvaluator::new()
        .evaluate_sync(&rs, &fact(json!({"tags": ["a", "b"]})))
        .unwrap();
    assert_eq!(v.matched, ["r"]);
}

// ---------------------------------------------------------------------------
// EvaluationMode (ports test_modes.py)
// ---------------------------------------------------------------------------

fn modes_ruleset() -> RuleSet {
    RuleSet::new("rs")
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

#[tokio::test]
async fn all_mode_fires_every_matching_rule() {
    let service = RuleEngineService::in_memory();
    let outcome = service
        .evaluate(&modes_ruleset(), &fact(json!({"tier": "gold"})))
        .await
        .unwrap();
    assert_eq!(outcome.verdict.matched, ["high", "low"]);
    assert_eq!(outcome.facts["high_ran"], json!(true));
    assert_eq!(outcome.facts["low_ran"], json!(true));
}

#[tokio::test]
async fn first_match_mode_stops_after_first_match() {
    let service = RuleEngineService::in_memory().with_mode(EvaluationMode::FirstMatch);
    let outcome = service
        .evaluate(&modes_ruleset(), &fact(json!({"tier": "gold"})))
        .await
        .unwrap();
    assert_eq!(outcome.verdict.matched, ["high"]);
    assert!(!outcome.facts.contains_key("low_ran"));
}

// ---------------------------------------------------------------------------
// otherwise / enabled
// ---------------------------------------------------------------------------

#[tokio::test]
async fn otherwise_fires_on_non_match() {
    let service = RuleEngineService::in_memory();
    let rs = RuleSet::new("rs").with_rule(
        Rule::new("r", Logic::cond("tier", Op::Eq, json!("gold")))
            .with_action(
                Action::new("set")
                    .with_param("target", "result")
                    .with_param("value", "then"),
            )
            .with_otherwise(
                Action::new("set")
                    .with_param("target", "result")
                    .with_param("value", "else"),
            ),
    );
    let outcome = service
        .evaluate(&rs, &fact(json!({"tier": "bronze"})))
        .await
        .unwrap();
    assert!(outcome.verdict.matched.is_empty());
    assert_eq!(outcome.facts["result"], json!("else"));
}

#[tokio::test]
async fn disabled_rule_is_a_no_op() {
    let service = RuleEngineService::in_memory();
    let rs = RuleSet::new("rs").with_rule(
        Rule::new("off", Logic::default())
            .with_enabled(false)
            .with_action(
                Action::new("set")
                    .with_param("target", "x")
                    .with_param("value", 1),
            ),
    );
    let outcome = service.evaluate(&rs, &Fact::new()).await.unwrap();
    assert!(outcome.verdict.matched.is_empty());
    assert!(!outcome.facts.contains_key("x"));
}

// ---------------------------------------------------------------------------
// validation (ports test_loading_and_validation.py)
// ---------------------------------------------------------------------------

#[test]
fn valid_ruleset_has_no_issues() {
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
    assert!(validate_ruleset(&rs).is_empty());
    RuleSetValidator::assert_valid(&rs).expect("valid ruleset must not raise");
}

#[test]
fn invalid_ruleset_reports_all_issues() {
    let rs = RuleSet::new("rs")
        .with_rule(Rule::new("dup", Logic::default()))
        .with_rule(
            Rule::new(
                "dup",
                Logic::cond("x", Op::Other("bad_op".into()), json!(null)),
            )
            .with_action(Action::new("teleport"))
            .with_action(Action::new("set")),
        );
    let issues = validate_ruleset(&rs);
    assert!(issues
        .iter()
        .any(|i| i.contains("duplicate") && i.contains("dup")));
    assert!(issues.iter().any(|i| i.contains("bad_op")));
    assert!(issues.iter().any(|i| i.contains("teleport")));
    assert!(issues
        .iter()
        .any(|i| i.contains("target") && i.contains("set")));
    let err = RuleSetValidator::assert_valid(&rs).unwrap_err();
    assert_eq!(err.ruleset_name, "rs");
    assert!(err.to_string().contains("dup"));
}

#[test]
fn between_malformed_operand_is_a_validation_issue() {
    let rs = RuleSet::new("rs").with_rule(Rule::new(
        "r1",
        Logic::cond("x", Op::Between, json!([1, 2, 3])),
    ));
    assert!(validate_ruleset(&rs).iter().any(|i| i.contains("between")));
}
