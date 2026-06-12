//! Named-ruleset management and action-executing evaluation — the
//! pyfly-parity service layer.
//!
//! This module ports pyfly's `RuleSetRepository` SPI, its
//! `InMemoryRuleSetRepository` adapter, and the `RuleEngineService` facade.
//! The Rust [`RuleEngineService`] wires a [`RuleSetRepository`] (persistence)
//! to an [`Evaluator`] (the Go-parity [`AstEvaluator`] by default) and an
//! [`ActionRegistry`] (the pyfly-parity action handlers), so a caller can:
//!
//! 1. **register** a [`RuleSet`] under its [`RuleSet::name`], then
//! 2. **evaluate it by name** against a fact — running the matched rules'
//!    actions over the (mutable) fact and returning the verdict, the final
//!    fact state, the executed actions, and any per-action error.
//!
//! ```rust
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! use firefly_rule_engine::{Action, Logic, Op, Rule, RuleEngineService, RuleSet};
//! use serde_json::json;
//!
//! let rs = RuleSet::new("orders").with_rule(
//!     Rule::new("vip", Logic::cond("amount", Op::Gte, json!(1000.0)))
//!         .with_action(Action::new("set").with_param("target", "tier").with_param("value", "vip")),
//! );
//!
//! let service = RuleEngineService::in_memory();
//! service.register(rs).await;
//!
//! let fact = json!({"amount": 1500}).as_object().unwrap().clone();
//! let outcome = service.evaluate_by_name("orders", &fact).await.unwrap();
//! assert_eq!(outcome.verdict.matched, ["vip"]);
//! assert_eq!(outcome.facts["tier"], json!("vip"));
//! # });
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::actions::ActionRegistry;
use crate::core::{AstEvaluator, EvalError, EvaluationMode};
use crate::interfaces::{Evaluator, Fact, Verdict};
use crate::models::RuleSet;

/// Persistence SPI for named [`RuleSet`]s — the Rust counterpart of
/// pyfly's `RuleSetRepository` protocol.
///
/// Rulesets are keyed by their [`RuleSet::name`] (the Rust port has no
/// separate `id` field; `name` is the stable identifier). The trait is
/// object-safe so [`RuleEngineService`] can hold an `Arc<dyn
/// RuleSetRepository>`, and `async` per the framework convention for
/// outbound ports.
#[async_trait]
pub trait RuleSetRepository: Send + Sync {
    /// Persists `ruleset`, replacing any existing ruleset with the same
    /// [`RuleSet::name`].
    async fn save(&self, ruleset: RuleSet);

    /// Returns the ruleset registered under `name`, or `None` when absent.
    async fn get(&self, name: &str) -> Option<RuleSet>;

    /// Returns every persisted ruleset, in unspecified order.
    async fn list(&self) -> Vec<RuleSet>;

    /// Removes the ruleset registered under `name`, returning `true` when a
    /// ruleset was present (and removed), `false` otherwise.
    async fn delete(&self, name: &str) -> bool;
}

/// In-memory [`RuleSetRepository`] adapter — the Rust counterpart of
/// pyfly's `InMemoryRuleSetRepository`.
///
/// Backed by an [`RwLock`]-guarded map so it is `Send + Sync` and safe to
/// share behind an [`Arc`]. Intended for tests, single-process services,
/// and as the default backing store for [`RuleEngineService::in_memory`].
#[derive(Debug, Default)]
pub struct MemoryRuleSetRepository {
    store: RwLock<HashMap<String, RuleSet>>,
}

impl MemoryRuleSetRepository {
    /// Builds an empty in-memory repository.
    pub fn new() -> Self {
        MemoryRuleSetRepository::default()
    }
}

#[async_trait]
impl RuleSetRepository for MemoryRuleSetRepository {
    async fn save(&self, ruleset: RuleSet) {
        self.store
            .write()
            .await
            .insert(ruleset.name.clone(), ruleset);
    }

    async fn get(&self, name: &str) -> Option<RuleSet> {
        self.store.read().await.get(name).cloned()
    }

    async fn list(&self) -> Vec<RuleSet> {
        self.store.read().await.values().cloned().collect()
    }

    async fn delete(&self, name: &str) -> bool {
        self.store.write().await.remove(name).is_some()
    }
}

/// Error returned by [`RuleEngineService::evaluate_by_name`].
#[derive(Debug, PartialEq, Error)]
pub enum ServiceError {
    /// No ruleset was registered under the requested name.
    #[error("ruleset {0:?} not found in repository")]
    RuleSetNotFound(String),
    /// Evaluation of the loaded ruleset failed (unknown operator, bad
    /// regex, non-numeric comparison).
    #[error(transparent)]
    Eval(#[from] EvalError),
}

/// The result of an action-executing evaluation — the Rust counterpart of
/// pyfly's `list[EvaluationResult]`, collapsed into a single value that
/// carries the verdict, the post-execution fact state, the executed
/// actions, and any per-action error.
///
/// `verdict` is the Go-parity [`Verdict`] (matched rule ids + the matched
/// actions, untouched); `facts` is the input fact after every matched
/// action has been applied; `actions_executed` lists the actions that ran
/// without error, in firing order; `error` is the `"; "`-joined
/// `"<type>: <message>"` failures, or `None` when all actions succeeded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvaluationOutcome {
    /// The pure verdict (matched rule ids + matched actions).
    pub verdict: Verdict,
    /// The fact context after every matched action has been applied.
    pub facts: Fact,
    /// The actions that executed without error, in firing order.
    pub actions_executed: Vec<crate::models::Action>,
    /// `"; "`-joined per-action failures, or `None` when all succeeded.
    pub error: Option<String>,
}

/// Facade wiring a [`RuleSetRepository`], an [`Evaluator`], and an
/// [`ActionRegistry`] — the Rust counterpart of pyfly's `RuleEngineService`.
///
/// Construct it with [`RuleEngineService::in_memory`] (default
/// [`AstEvaluator`] + builtin action handlers + a fresh
/// [`MemoryRuleSetRepository`]) or with [`RuleEngineService::new`] to inject
/// a custom repository, evaluator, and registry (e.g. a registry with extra
/// `call` / `http` handlers).
#[derive(Clone)]
pub struct RuleEngineService {
    repository: Arc<dyn RuleSetRepository>,
    evaluator: Arc<dyn Evaluator>,
    registry: Arc<ActionRegistry>,
    mode: EvaluationMode,
}

impl RuleEngineService {
    /// Wires the given repository, evaluator, and action registry under
    /// [`EvaluationMode::All`] (every enabled rule is evaluated).
    pub fn new(
        repository: Arc<dyn RuleSetRepository>,
        evaluator: Arc<dyn Evaluator>,
        registry: Arc<ActionRegistry>,
    ) -> Self {
        RuleEngineService {
            repository,
            evaluator,
            registry,
            mode: EvaluationMode::All,
        }
    }

    /// Sets the [`EvaluationMode`], builder-style.
    ///
    /// [`EvaluationMode::FirstMatch`] makes [`evaluate`](Self::evaluate)
    /// / [`evaluate_by_name`](Self::evaluate_by_name) stop after the
    /// first matching rule (pyfly parity); the default is
    /// [`EvaluationMode::All`].
    #[must_use]
    pub fn with_mode(mut self, mode: EvaluationMode) -> Self {
        self.mode = mode;
        self
    }

    /// Returns the service's [`EvaluationMode`].
    pub fn mode(&self) -> EvaluationMode {
        self.mode
    }

    /// Builds a service backed by a fresh [`MemoryRuleSetRepository`], the
    /// default [`AstEvaluator`], and the builtin [`ActionRegistry`]
    /// (`set` / `increment` / `log`).
    pub fn in_memory() -> Self {
        RuleEngineService::new(
            Arc::new(MemoryRuleSetRepository::new()),
            Arc::new(AstEvaluator::new()),
            Arc::new(ActionRegistry::default()),
        )
    }

    /// Builds a service backed by a fresh [`MemoryRuleSetRepository`], the
    /// default [`AstEvaluator`], and a caller-supplied [`ActionRegistry`].
    pub fn in_memory_with_registry(registry: ActionRegistry) -> Self {
        RuleEngineService::new(
            Arc::new(MemoryRuleSetRepository::new()),
            Arc::new(AstEvaluator::new()),
            Arc::new(registry),
        )
    }

    /// Registers `ruleset` in the repository under its [`RuleSet::name`],
    /// replacing any ruleset already present under that name. This is the
    /// pyfly `save_ruleset` operation under its task-mandated name.
    pub async fn register(&self, ruleset: RuleSet) {
        self.repository.save(ruleset).await;
    }

    /// Returns the ruleset registered under `name`, or `None` when absent.
    pub async fn get(&self, name: &str) -> Option<RuleSet> {
        self.repository.get(name).await
    }

    /// Returns every registered ruleset.
    pub async fn list(&self) -> Vec<RuleSet> {
        self.repository.list().await
    }

    /// Removes the ruleset registered under `name`, returning `true` when a
    /// ruleset was present.
    pub async fn delete(&self, name: &str) -> bool {
        self.repository.delete(name).await
    }

    /// Evaluates `ruleset` against `fact`, then runs the matched actions
    /// over a clone of `fact`, returning the combined [`EvaluationOutcome`].
    ///
    /// The input `fact` is left untouched; the mutated copy is returned in
    /// [`EvaluationOutcome::facts`].
    pub async fn evaluate(
        &self,
        ruleset: &RuleSet,
        fact: &Fact,
    ) -> Result<EvaluationOutcome, EvalError> {
        let verdict = self
            .evaluator
            .evaluate_with_mode(ruleset, fact, self.mode)
            .await?;
        let mut facts = fact.clone();
        let outcome = self.registry.execute(&verdict.actions, &mut facts);
        Ok(EvaluationOutcome {
            verdict,
            facts,
            actions_executed: outcome.executed,
            error: outcome.error,
        })
    }

    /// Loads the ruleset registered under `name` and evaluates it against
    /// `fact` (executing the matched actions).
    ///
    /// Returns [`ServiceError::RuleSetNotFound`] when no ruleset is
    /// registered under `name`, or [`ServiceError::Eval`] when evaluation
    /// of the loaded ruleset fails.
    pub async fn evaluate_by_name(
        &self,
        name: &str,
        fact: &Fact,
    ) -> Result<EvaluationOutcome, ServiceError> {
        let ruleset = self
            .repository
            .get(name)
            .await
            .ok_or_else(|| ServiceError::RuleSetNotFound(name.to_owned()))?;
        Ok(self.evaluate(&ruleset, fact).await?)
    }
}

impl std::fmt::Debug for RuleEngineService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleEngineService")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionError;
    use crate::core::EvaluationMode;
    use crate::models::{Action, Logic, Op, Rule};
    use serde_json::json;

    fn fact(v: serde_json::Value) -> Fact {
        v.as_object().expect("fact must be an object").clone()
    }

    fn simple_ruleset(name: &str) -> RuleSet {
        RuleSet::new(name).with_rule(
            Rule::new("r1", Logic::cond("active", Op::Eq, json!(true))).with_action(
                Action::new("set")
                    .with_param("target", "result")
                    .with_param("value", "matched"),
            ),
        )
    }

    // ----- repository (ports pyfly test_repository_round_trip) ------------

    #[tokio::test]
    async fn repository_round_trip() {
        let repo = MemoryRuleSetRepository::new();
        repo.save(RuleSet::new("x")).await;
        assert!(repo.get("x").await.is_some());
        assert_eq!(repo.list().await.len(), 1);
        assert!(repo.delete("x").await);
        assert!(!repo.delete("x").await);
        assert!(repo.get("x").await.is_none());
    }

    #[tokio::test]
    async fn repository_save_replaces_existing() {
        let repo = MemoryRuleSetRepository::new();
        repo.save(RuleSet::new("x").with_version("1")).await;
        repo.save(RuleSet::new("x").with_version("2")).await;
        assert_eq!(repo.list().await.len(), 1);
        assert_eq!(repo.get("x").await.unwrap().version, "2");
    }

    // ----- evaluate_by_name (ports pyfly TestEvaluateByName) --------------

    #[tokio::test]
    async fn round_trip_register_and_evaluate() {
        let service = RuleEngineService::in_memory();
        service.register(simple_ruleset("test-rs")).await;
        let outcome = service
            .evaluate_by_name("test-rs", &fact(json!({"active": true})))
            .await
            .unwrap();
        assert_eq!(outcome.verdict.matched, ["r1"]);
        assert!(outcome.error.is_none());
        assert_eq!(outcome.facts["result"], json!("matched"));
    }

    #[tokio::test]
    async fn evaluate_by_name_no_match_runs_no_actions() {
        // The Go-parity verdict only carries `then` actions, so a
        // non-match fires nothing and the fact is unchanged.
        let service = RuleEngineService::in_memory();
        service.register(simple_ruleset("test-rs")).await;
        let outcome = service
            .evaluate_by_name("test-rs", &fact(json!({"active": false})))
            .await
            .unwrap();
        assert!(outcome.verdict.matched.is_empty());
        assert!(!outcome.facts.contains_key("result"));
    }

    #[tokio::test]
    async fn evaluate_by_name_not_found_is_an_error() {
        let service = RuleEngineService::in_memory();
        let err = service
            .evaluate_by_name("does-not-exist", &Fact::new())
            .await
            .unwrap_err();
        assert_eq!(err, ServiceError::RuleSetNotFound("does-not-exist".into()));
        assert!(err.to_string().contains("does-not-exist"));
    }

    #[tokio::test]
    async fn evaluate_does_not_mutate_input_fact() {
        let service = RuleEngineService::in_memory();
        let rs = simple_ruleset("rs");
        let input = fact(json!({"active": true}));
        let outcome = service.evaluate(&rs, &input).await.unwrap();
        assert!(outcome.facts.contains_key("result"));
        assert!(!input.contains_key("result"), "input must be untouched");
    }

    // ----- error propagation ---------------------------------------------

    #[tokio::test]
    async fn evaluate_records_unregistered_action_error() {
        let service = RuleEngineService::in_memory();
        let rs = RuleSet::new("err-rs").with_rule(
            Rule::new("bad", Logic::default()).with_action(Action::new("nonexistent_action")),
        );
        let outcome = service.evaluate(&rs, &Fact::new()).await.unwrap();
        let error = outcome.error.expect("error must be recorded");
        assert!(error.contains("nonexistent_action"), "error: {error}");
    }

    #[tokio::test]
    async fn evaluate_propagates_evaluation_error() {
        let service = RuleEngineService::in_memory();
        let rs = RuleSet::new("e").with_rule(Rule::new(
            "r",
            Logic::cond("a", Op::Other("fuzzy".into()), json!(1)),
        ));
        service.register(rs).await;
        let err = service
            .evaluate_by_name("e", &fact(json!({"a": 1})))
            .await
            .unwrap_err();
        assert!(matches!(err, ServiceError::Eval(_)));
    }

    // ----- custom registry ------------------------------------------------

    #[tokio::test]
    async fn service_uses_injected_action_registry() {
        let registry =
            ActionRegistry::default().with_handler("call", |action: &Action, facts: &mut Fact| {
                let target = action
                    .params
                    .get("target")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ActionError::Custom("call missing target".into()))?;
                facts.insert("called".into(), json!(target));
                Ok(())
            });
        let service = RuleEngineService::in_memory_with_registry(registry);
        let rs = RuleSet::new("rs").with_rule(
            Rule::new("r", Logic::default())
                .with_action(Action::new("call").with_param("target", "svc")),
        );
        let outcome = service.evaluate(&rs, &Fact::new()).await.unwrap();
        assert!(outcome.error.is_none());
        assert_eq!(outcome.facts["called"], json!("svc"));
    }

    // ----- EvaluationMode (ports pyfly test_modes.py through the service) --

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
    async fn all_mode_fires_actions_for_all_matching_rules() {
        let service = RuleEngineService::in_memory(); // ALL is the default
        assert_eq!(service.mode(), EvaluationMode::All);
        let outcome = service
            .evaluate(&modes_ruleset(), &fact(json!({"tier": "gold"})))
            .await
            .unwrap();
        assert_eq!(outcome.verdict.matched, ["high", "low"]);
        assert_eq!(outcome.facts["high_ran"], json!(true));
        assert_eq!(outcome.facts["low_ran"], json!(true));
    }

    #[tokio::test]
    async fn first_match_mode_lower_priority_actions_do_not_fire() {
        let service = RuleEngineService::in_memory().with_mode(EvaluationMode::FirstMatch);
        let outcome = service
            .evaluate(&modes_ruleset(), &fact(json!({"tier": "gold"})))
            .await
            .unwrap();
        assert_eq!(outcome.verdict.matched, ["high"]);
        assert_eq!(outcome.facts["high_ran"], json!(true));
        assert!(
            !outcome.facts.contains_key("low_ran"),
            "low-priority rule must NOT have executed"
        );
    }

    #[tokio::test]
    async fn first_match_mode_returns_all_when_no_rule_matches() {
        let service = RuleEngineService::in_memory().with_mode(EvaluationMode::FirstMatch);
        let outcome = service
            .evaluate(&modes_ruleset(), &fact(json!({"tier": "bronze"})))
            .await
            .unwrap();
        assert!(outcome.verdict.matched.is_empty());
        assert!(!outcome.facts.contains_key("high_ran"));
        assert!(!outcome.facts.contains_key("low_ran"));
    }

    // ----- otherwise branch through the service ---------------------------

    #[tokio::test]
    async fn otherwise_actions_execute_when_when_is_false() {
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
        assert_eq!(outcome.actions_executed.len(), 1);
    }

    #[tokio::test]
    async fn disabled_rule_fires_nothing_through_service() {
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

    // ----- passthrough -----------------------------------------------------

    #[tokio::test]
    async fn list_and_get_passthrough() {
        let service = RuleEngineService::in_memory();
        service.register(simple_ruleset("a")).await;
        service.register(simple_ruleset("b")).await;
        let names: std::collections::BTreeSet<String> =
            service.list().await.into_iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            ["a".to_owned(), "b".to_owned()].into_iter().collect()
        );
        assert!(service.get("a").await.is_some());
        assert!(service.get("nope").await.is_none());
    }
}
