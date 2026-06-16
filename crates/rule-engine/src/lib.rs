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
//! * [`actions`] — pyfly-parity action execution: [`ActionHandler`] SPI,
//!   the `set`/`increment`/`log` builtins, and [`ActionRegistry`].
//! * [`service`] — pyfly-parity named-ruleset management:
//!   [`RuleSetRepository`], [`MemoryRuleSetRepository`], and
//!   [`RuleEngineService`] (`register` / `evaluate_by_name`, with an
//!   [`EvaluationMode`]).
//! * [`validation`] — pyfly-parity static linter:
//!   [`validate_ruleset`] / [`RuleSetValidator`].
//! * [`web`] — REST admin router ([`rule_engine_router`]) and the
//!   named-ruleset service router ([`rule_engine_service_router`]).
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
//! Go-parity set: `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `in`, `notIn`,
//! `contains`, `startsWith`, `endsWith`, `matches` (regex), `isNull`,
//! `isNotNull`.
//!
//! pyfly-parity additions: `between` (inclusive `[lo, hi]` range),
//! `notContains` (inverse of `contains`), `exists` (present and
//! non-null), `isEmpty` (null/absent, empty string, empty list, or
//! empty object). [`Op::from`] also accepts pyfly's snake_case spellings
//! (`not_in`, `starts_with`, `is_null`, `not_contains`, `is_empty`,
//! `regex` → `matches`, …), so rule documents authored against the
//! pyfly DSL parse unchanged.
//!
//! Rules fire in **descending priority order**; ties broken by document
//! order. The [`Verdict`] returns the matched rule ids and the merged
//! action list. A rule may carry an `otherwise` branch (else-actions
//! fired when `when` is false) and an `enabled` flag (a disabled rule is
//! skipped entirely). [`EvaluationMode::FirstMatch`] stops evaluation
//! after the first matching rule.
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

pub mod actions;
pub mod core;
pub mod interfaces;
pub mod models;
pub mod sdk;
pub mod service;
pub mod validation;
pub mod web;

pub use crate::actions::{
    ActionError, ActionHandler, ActionOutcome, ActionRegistry, IncrementHandler, LogHandler,
    SetHandler,
};
pub use crate::core::{AstEvaluator, EvalError, EvaluationMode};
pub use crate::interfaces::{Evaluator, Fact, Verdict};
pub use crate::models::{Action, Condition, DslError, Logic, Op, Rule, RuleSet};
pub use crate::sdk::{HttpTransport, ReqwestTransport, RuleEngineClient, SdkError};
pub use crate::service::{
    EvaluationOutcome, MemoryRuleSetRepository, RuleEngineMetrics, RuleEngineService,
    RuleSetRepository, ServiceError,
};
pub use crate::validation::{validate_ruleset, RuleSetValidator, RuleValidationError};
pub use crate::web::{
    rule_engine_router, rule_engine_router_with, rule_engine_service_router,
    rule_engine_service_router_with, ErrorBody, EvaluateByNameRequest, EvaluateByNameResponse,
    EvaluateRequest, EvaluateYamlRequest, RegisteredResponse, RuleSetNamesResponse,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.19";
