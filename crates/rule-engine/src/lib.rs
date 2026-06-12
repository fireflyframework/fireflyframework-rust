//! # firefly-rule-engine
//!
//! The framework's **declarative business-rule engine** — the Rust port
//! of the Go `ruleengine` module (Java original:
//! `firefly-common-rule-engine`, .NET:
//! `FireflyFramework.RuleEngine.*`).
//!
//! Rules are authored as YAML documents (or programmatically via
//! [`models`]), parsed into an AST, and evaluated by a recursive walker
//! that resolves fact-paths against a JSON-object fact.
//!
//! Sub-modules mirror the Go package split:
//!
//! * [`models`] — AST: [`Rule`], [`RuleSet`], [`Logic`], [`Condition`],
//!   [`Action`], [`Op`].
//! * [`interfaces`] — port: [`Evaluator`], [`Verdict`], [`Fact`].
//! * [`core`] — [`AstEvaluator`], the default [`Evaluator`].
//! * [`web`] — REST admin router ([`rule_engine_router`]).
//! * [`sdk`] — typed admin client ([`RuleEngineClient`]).
//!
//! ## Rule shape
//!
//! ```yaml
//! name: vip-tagging
//! version: 1
//! rules:
//!   - id: premium
//!     priority: 10
//!     when:
//!       all:
//!         - cond: { path: user.age,     op: gte, value: 18 }
//!         - cond: { path: user.country, op: in,  value: [ES, FR] }
//!     then:
//!       - type: tag
//!         params: { name: premium }
//! ```
//!
//! ## Operators
//!
//! `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `in`, `notIn`, `contains`,
//! `startsWith`, `endsWith`, `matches` (regex), `isNull`, `isNotNull`.
//!
//! Rules fire in **descending priority order**; ties broken by document
//! order. The [`Verdict`] returns the matched rule ids and the merged
//! action list.
//!
//! ## Quick start
//!
//! ```rust
//! use firefly_rule_engine::{Action, AstEvaluator, Logic, Op, Rule, RuleSet};
//! use serde_json::json;
//!
//! let rs = RuleSet::new("orders").with_rule(
//!     Rule::new("high-value", Logic::cond("amount", Op::Gt, json!(1000.0)))
//!         .with_action(Action::new("review").with_param("queue", "manual")),
//! );
//!
//! let fact = json!({"amount": 1500}).as_object().unwrap().clone();
//! let verdict = AstEvaluator::new().evaluate_sync(&rs, &fact).unwrap();
//! assert_eq!(verdict.matched, ["high-value"]);
//! assert_eq!(verdict.actions[0].action_type, "review");
//! ```
//!
//! Or straight from the YAML DSL:
//!
//! ```rust
//! use firefly_rule_engine::{AstEvaluator, RuleSet};
//!
//! let rs = RuleSet::from_yaml(
//!     "name: demo\nrules:\n  - id: es\n    when:\n      cond: { path: country, op: eq, value: ES }\n",
//! )
//! .unwrap();
//! let fact = serde_json::json!({"country": "ES"}).as_object().unwrap().clone();
//! let verdict = AstEvaluator::new().evaluate_sync(&rs, &fact).unwrap();
//! assert_eq!(verdict.matched, ["es"]);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod core;
pub mod interfaces;
pub mod models;
pub mod sdk;
pub mod web;

pub use crate::core::{AstEvaluator, EvalError};
pub use crate::interfaces::{Evaluator, Fact, Verdict};
pub use crate::models::{Action, Condition, DslError, Logic, Op, Rule, RuleSet};
pub use crate::sdk::{HttpTransport, ReqwestTransport, RuleEngineClient, SdkError};
pub use crate::web::{
    rule_engine_router, rule_engine_router_with, ErrorBody, EvaluateRequest, EvaluateYamlRequest,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
