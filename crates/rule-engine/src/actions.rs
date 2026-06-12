//! Action execution — the pyfly-parity layer on top of the Go-parity
//! [`Verdict`](crate::Verdict).
//!
//! The Go-parity [`AstEvaluator`](crate::core::AstEvaluator) is a *pure*
//! engine: it returns the matched [`Action`]s in a [`Verdict`] but never
//! runs them. pyfly's `RuleEvaluator`, by contrast, owns a pluggable
//! **action-handler registry** that mutates an evaluation context as each
//! matched action fires. This module ports that registry so a Rust caller
//! can take a `Verdict`'s `actions` and apply them over a mutable [`Fact`].
//!
//! ## The SPI
//!
//! [`ActionHandler`] is the action-execution SPI — the Rust counterpart of
//! pyfly's `ActionHandler` `__call__` protocol. A handler receives the full
//! [`Action`] (so it can inspect `action_type` and every entry of `params`)
//! and the mutable evaluation context, and either mutates the context or
//! raises an [`ActionError`]. Any closure of the right shape *is* an
//! `ActionHandler` (see the blanket impl), mirroring pyfly's "a plain
//! function, a lambda, or a `__call__` object all qualify" semantics.
//!
//! ## Builtins
//!
//! [`ActionRegistry::with_builtins`] (and [`ActionRegistry::default`])
//! seed three handlers, keyed by the action's `type`:
//!
//! * `set` — writes `params["value"]` into the dot-path `params["target"]`.
//! * `increment` — adds `params["value"]` to the current value at
//!   `params["target"]`, mirroring pyfly's `current + (action.value or 1)`
//!   with `current = read(...) or 0`: a falsy current (absent / `null` /
//!   `0` / `false` / …) reads as `0`, and a falsy `value` operand
//!   (including an explicit `0`) reads as `1`.
//! * `log` — a side-effect-only handler that never mutates the context
//!   (matching pyfly's logger-only `log` action).
//!
//! Custom handlers registered through [`ActionRegistry::register`] are
//! **additive** and may override a builtin under the same key. An action
//! whose `type` is not in the registry fails with
//! [`ActionError::Unsupported`], matching pyfly's loud-failure semantics
//! (audit #215).
//!
//! ## Isolation
//!
//! [`ActionRegistry::execute`] runs a list of actions over a shared
//! context, **isolating** each one: a failing action records its error and
//! the remaining actions still run, exactly like pyfly's isolate-and-continue
//! (audit #216). The returned [`ActionOutcome`] reports the executed actions
//! and the combined error string (or `None`).
//!
//! ```rust
//! use firefly_rule_engine::{Action, ActionRegistry, Fact};
//! use serde_json::json;
//!
//! let registry = ActionRegistry::default();
//! let mut facts: Fact = json!({"count": 4}).as_object().unwrap().clone();
//! let actions = vec![
//!     Action::new("set").with_param("target", "tier").with_param("value", "gold"),
//!     Action::new("increment").with_param("target", "count"),
//! ];
//! let outcome = registry.execute(&actions, &mut facts);
//! assert!(outcome.error.is_none());
//! assert_eq!(facts["tier"], json!("gold"));
//! assert_eq!(facts["count"], json!(5));
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::interfaces::Fact;
use crate::models::Action;

/// Error raised while executing a single rule [`Action`].
///
/// The message spellings are stable: they appear verbatim in the
/// `"<type>: <error>"` segments of [`ActionOutcome::error`] and in the
/// `error` field of the pyfly-style evaluation result, so log lines and
/// assertions stay recognisable.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActionError {
    /// The action's `type` has no registered handler. Mirrors pyfly's
    /// `NotImplementedError` (audit #215): a typo or an unsupported action
    /// surfaces loudly instead of silently doing nothing.
    #[error("unsupported action type '{0}'; register a handler for it")]
    Unsupported(String),
    /// A required parameter (e.g. `target` for `set` / `increment`) was
    /// missing from the action's `params`.
    #[error("{action_type} action missing '{param}'")]
    MissingParam {
        /// The action `type` whose parameter was absent.
        action_type: String,
        /// The name of the missing parameter.
        param: String,
    },
    /// `increment` was applied to a non-numeric current value or a
    /// non-numeric operand.
    #[error("increment: non-numeric value")]
    NonNumericIncrement,
    /// Free-form failure raised by a custom handler.
    #[error("{0}")]
    Custom(String),
}

/// SPI for executing a single rule [`Action`] over a mutable context —
/// the Rust counterpart of pyfly's `ActionHandler` `__call__` protocol.
///
/// A handler receives the full [`Action`] (so it can read `action_type`
/// and every `params` entry) and the mutable [`Fact`] context, and either
/// mutates the context or returns an [`ActionError`]. Implementors must be
/// `Send + Sync` so a registry can be shared across threads behind an
/// [`Arc`].
///
/// Any closure `Fn(&Action, &mut Fact) -> Result<(), ActionError>` already
/// implements this trait (see the blanket impl below), so a plain function
/// or a `move` closure can be registered directly — matching pyfly, where a
/// plain function and a `__call__` object both satisfy the protocol.
pub trait ActionHandler: Send + Sync {
    /// Applies `action` to `facts`, mutating the context in place.
    ///
    /// Returns [`ActionError`] when the action cannot be executed (missing
    /// parameter, type mismatch, …). The caller
    /// ([`ActionRegistry::execute`]) isolates the failure and continues
    /// with the remaining actions.
    fn apply(&self, action: &Action, facts: &mut Fact) -> Result<(), ActionError>;
}

impl<F> ActionHandler for F
where
    F: Fn(&Action, &mut Fact) -> Result<(), ActionError> + Send + Sync,
{
    fn apply(&self, action: &Action, facts: &mut Fact) -> Result<(), ActionError> {
        self(action, facts)
    }
}

/// Builtin `set` handler: writes `params["value"]` into the dot-path
/// `params["target"]`, creating intermediate objects as needed.
///
/// A missing `target` raises [`ActionError::MissingParam`]; a missing
/// `value` writes JSON `null`, matching pyfly (`action.value` defaults to
/// `None`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SetHandler;

impl ActionHandler for SetHandler {
    fn apply(&self, action: &Action, facts: &mut Fact) -> Result<(), ActionError> {
        let target = require_target(action, "set")?;
        let value = action.params.get("value").cloned().unwrap_or(Value::Null);
        write_path(facts, target, value);
        Ok(())
    }
}

/// Builtin `increment` handler: adds `params["value"]` to the current value
/// at the dot-path `params["target"]`, mirroring pyfly's
/// `current + (action.value or 1)` with `current = read(...) or 0`.
///
/// pyfly applies Python's `or` to both operands, so any *falsy* value is
/// coerced before the addition: an absent / `null` / falsy current value
/// reads as `0`, and an absent / `null` / falsy `value` operand (including an
/// explicit `0`, `false`, `""`, `[]`, or `{}`) reads as `1`. A JSON `true`
/// survives the `or` (it is truthy) and, because Python `bool` is a subclass
/// of `int`, participates in the addition as `1` (`false` as `0`).
///
/// Integer + integer arithmetic stays integral; any float operand promotes
/// the result to a float, matching pyfly's Python `int`/`float` addition. A
/// missing `target` raises [`ActionError::MissingParam`]; a current value or
/// operand that is neither numeric nor boolean (a non-empty string, list, or
/// object) raises [`ActionError::NonNumericIncrement`].
#[derive(Debug, Clone, Copy, Default)]
pub struct IncrementHandler;

impl ActionHandler for IncrementHandler {
    fn apply(&self, action: &Action, facts: &mut Fact) -> Result<(), ActionError> {
        let target = require_target(action, "increment")?;
        // pyfly: `current = self._read(action.target, ctx) or 0`.
        let current = read_path(facts, target).cloned().unwrap_or(Value::Null);
        let current = py_or(current, || Value::from(0));
        // pyfly: `action.value or 1` — an explicit falsy `value` (0, false,
        // "", …) is coerced to 1 exactly like the absent default.
        let by = py_or(
            action.params.get("value").cloned().unwrap_or(Value::Null),
            || Value::from(1),
        );
        let sum = add_numeric(&current, &by)?;
        write_path(facts, target, sum);
        Ok(())
    }
}

/// Builtin `log` handler: a side-effect-only handler that never mutates the
/// context, matching pyfly's logger-only `log` action (it emits a log line
/// for `value` / `target` and returns).
///
/// The handler is a no-op with respect to the [`Fact`] context so that the
/// wire-observable behaviour matches pyfly exactly; callers that want an
/// observable audit trail can register a custom handler under `"log"`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogHandler;

impl ActionHandler for LogHandler {
    fn apply(&self, _action: &Action, _facts: &mut Fact) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Result of running a list of actions over a context with
/// [`ActionRegistry::execute`].
///
/// `executed` holds the actions that ran without error, in firing order;
/// `error` is the `"; "`-joined `"<type>: <message>"` list of failures, or
/// `None` when every action succeeded — the same shape pyfly's
/// `EvaluationResult.error` carries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionOutcome {
    /// The actions that executed without error, in firing order.
    pub executed: Vec<Action>,
    /// `"; "`-joined `"<type>: <message>"` failures, or `None` when all
    /// actions succeeded.
    pub error: Option<String>,
}

/// A pluggable registry of [`ActionHandler`]s keyed by action `type` — the
/// Rust counterpart of pyfly's `RuleEvaluator` action-handler registry.
///
/// [`ActionRegistry::default`] / [`ActionRegistry::with_builtins`] seed the
/// `set`, `increment`, and `log` builtins. [`ActionRegistry::register`]
/// adds custom handlers (additive; may override a builtin), and
/// [`ActionRegistry::execute`] applies a list of actions over a shared
/// context with per-action isolation.
#[derive(Clone)]
pub struct ActionRegistry {
    handlers: HashMap<String, Arc<dyn ActionHandler>>,
}

impl std::fmt::Debug for ActionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<&String> = self.handlers.keys().collect();
        keys.sort();
        f.debug_struct("ActionRegistry")
            .field("handlers", &keys)
            .finish()
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        ActionRegistry::with_builtins()
    }
}

impl ActionRegistry {
    /// Builds a registry with the three builtins (`set`, `increment`,
    /// `log`) registered.
    pub fn with_builtins() -> Self {
        let mut registry = ActionRegistry {
            handlers: HashMap::new(),
        };
        registry.register("set", SetHandler);
        registry.register("increment", IncrementHandler);
        registry.register("log", LogHandler);
        registry
    }

    /// Builds an empty registry with no handlers — every action will fail
    /// with [`ActionError::Unsupported`] until one is registered.
    pub fn empty() -> Self {
        ActionRegistry {
            handlers: HashMap::new(),
        }
    }

    /// Registers `handler` under the action `type` string `action_type`.
    ///
    /// Registration is additive and **overrides** any handler already
    /// present under the same key, matching pyfly's
    /// `action_handlers` constructor merge (a custom `"set"` shadows the
    /// builtin). Returns `&mut self` for chaining.
    pub fn register(
        &mut self,
        action_type: impl Into<String>,
        handler: impl ActionHandler + 'static,
    ) -> &mut Self {
        self.handlers.insert(action_type.into(), Arc::new(handler));
        self
    }

    /// Builder-style [`register`](ActionRegistry::register) — consumes and
    /// returns `self` so handlers can be chained at construction.
    #[must_use]
    pub fn with_handler(
        mut self,
        action_type: impl Into<String>,
        handler: impl ActionHandler + 'static,
    ) -> Self {
        self.register(action_type, handler);
        self
    }

    /// Returns `true` if a handler is registered for `action_type`.
    pub fn contains(&self, action_type: &str) -> bool {
        self.handlers.contains_key(action_type)
    }

    /// Applies a single `action` to `facts`.
    ///
    /// Returns [`ActionError::Unsupported`] when no handler is registered
    /// for the action's `type`, otherwise the handler's result.
    pub fn apply(&self, action: &Action, facts: &mut Fact) -> Result<(), ActionError> {
        match self.handlers.get(&action.action_type) {
            Some(handler) => handler.apply(action, facts),
            None => Err(ActionError::Unsupported(action.action_type.clone())),
        }
    }

    /// Executes `actions` in order over the shared `facts` context, with
    /// per-action isolation.
    ///
    /// A failing action records its error and the remaining actions still
    /// run (audit #216). The returned [`ActionOutcome`] lists the actions
    /// that succeeded and the combined error string (or `None`).
    pub fn execute(&self, actions: &[Action], facts: &mut Fact) -> ActionOutcome {
        let mut executed = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for action in actions {
            match self.apply(action, facts) {
                Ok(()) => executed.push(action.clone()),
                Err(e) => errors.push(format!("{}: {e}", action.action_type)),
            }
        }
        ActionOutcome {
            executed,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
        }
    }
}

/// Reads the `target` parameter of `action` as a string, raising
/// [`ActionError::MissingParam`] when it is absent or not a string.
fn require_target<'a>(action: &'a Action, action_type: &str) -> Result<&'a str, ActionError> {
    action
        .params
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| ActionError::MissingParam {
            action_type: action_type.to_owned(),
            param: "target".to_owned(),
        })
}

/// Resolves a dot-separated path against a [`Fact`], returning the value at
/// the leaf or `None` when any segment is absent or traverses a non-object.
fn read_path<'a>(facts: &'a Fact, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut current = facts.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

/// Writes `value` at the dot-separated `path`, creating intermediate JSON
/// objects as needed and replacing a non-object intermediate with a fresh
/// object — the [`Fact`] counterpart of pyfly's `RuleEvaluator._write`.
fn write_path(facts: &mut Fact, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts
        .split_last()
        .expect("split('.') yields at least one segment");
    // Descend into (creating as needed) the parent objects.
    let mut current: &mut Map<String, Value> = facts;
    for part in parents {
        let entry = current
            .entry((*part).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry
            .as_object_mut()
            .expect("entry was just ensured to be an object");
    }
    current.insert((*last).to_owned(), value);
}

/// Mirrors Python's `value or default`: returns `default()` when `value` is
/// *falsy* (`null`, `0`, `0.0`, `false`, `""`, `[]`, or `{}`), and `value`
/// itself otherwise. A JSON `true` is truthy and passes through unchanged.
fn py_or(value: Value, default: impl FnOnce() -> Value) -> Value {
    if py_truthy(&value) {
        value
    } else {
        default()
    }
}

/// Python truthiness for a JSON [`Value`]: `null`, `false`, numeric zero, the
/// empty string, the empty array, and the empty object are *falsy*; every
/// other value is truthy.
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Adds two JSON operands, keeping integer arithmetic integral and promoting
/// to `f64` when either operand is a float — mirroring Python's `int`/`float`
/// addition that pyfly's `increment` relies on. Because Python `bool` is a
/// subclass of `int`, a JSON `true`/`false` operand participates as `1`/`0`
/// (pyfly's `True + 1 == 2`). Any operand that is neither numeric nor boolean
/// (a non-empty string, list, or object) raises
/// [`ActionError::NonNumericIncrement`].
fn add_numeric(current: &Value, by: &Value) -> Result<Value, ActionError> {
    let (ai, af) = numeric_operand(current)?;
    let (bi, bf) = numeric_operand(by)?;
    match (ai, bi) {
        (Some(a), Some(b)) => Ok(Value::from(a + b)),
        _ => Ok(Value::from(af + bf)),
    }
}

/// Decomposes a numeric-or-boolean operand into `(integral, float)` views.
///
/// Integers and booleans (Python `bool` ⊂ `int`) yield `(Some(i64), f64)` so
/// integer arithmetic stays integral; floats yield `(None, f64)` to force
/// float promotion. A non-empty string, list, or object — which Python would
/// reject with a `TypeError` — raises [`ActionError::NonNumericIncrement`].
fn numeric_operand(value: &Value) -> Result<(Option<i64>, f64), ActionError> {
    match value {
        Value::Bool(b) => {
            let i = i64::from(*b);
            Ok((Some(i), i as f64))
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok((Some(i), i as f64))
            } else {
                let f = n.as_f64().ok_or(ActionError::NonNumericIncrement)?;
                Ok((None, f))
            }
        }
        _ => Err(ActionError::NonNumericIncrement),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact(v: Value) -> Fact {
        v.as_object().expect("fact must be an object").clone()
    }

    // ----- builtins -------------------------------------------------------

    #[test]
    fn set_writes_simple_target() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("set")
            .with_param("target", "x")
            .with_param("value", 42);
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["x"], json!(42));
    }

    #[test]
    fn set_writes_nested_dot_path_creating_objects() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("set")
            .with_param("target", "flags.high_value")
            .with_param("value", true);
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts, fact(json!({"flags": {"high_value": true}})));
    }

    #[test]
    fn set_missing_target_is_an_error() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("set").with_param("value", 1);
        let err = registry.apply(&action, &mut facts).unwrap_err();
        assert_eq!(err.to_string(), "set action missing 'target'");
    }

    #[test]
    fn set_missing_value_writes_null() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("set").with_param("target", "x");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["x"], Value::Null);
    }

    #[test]
    fn increment_defaults_to_one_from_absent() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("increment").with_param("target", "count");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["count"], json!(1));
    }

    #[test]
    fn increment_adds_explicit_value_to_current() {
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"count": 4}));
        let action = Action::new("increment")
            .with_param("target", "count")
            .with_param("value", 10);
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["count"], json!(14));
    }

    #[test]
    fn increment_promotes_to_float_with_float_operand() {
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"score": 1}));
        let action = Action::new("increment")
            .with_param("target", "score")
            .with_param("value", 0.5);
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["score"], json!(1.5));
    }

    #[test]
    fn increment_non_numeric_is_an_error() {
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"count": "nope"}));
        let action = Action::new("increment").with_param("target", "count");
        let err = registry.apply(&action, &mut facts).unwrap_err();
        assert_eq!(err, ActionError::NonNumericIncrement);
    }

    #[test]
    fn increment_with_explicit_zero_value_bumps_by_one() {
        // Bug 1 regression / pyfly parity: pyfly evaluates `action.value or 1`,
        // so an explicit falsy `value: 0` is coerced to 1. With current=5,
        // pyfly yields 5 + 1 = 6, not 5 + 0 = 5.
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"count": 5}));
        let action = Action::new("increment")
            .with_param("target", "count")
            .with_param("value", 0);
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["count"], json!(6));
    }

    #[test]
    fn increment_over_boolean_current_treats_true_as_one() {
        // Bug 2 regression / pyfly parity: a JSON boolean current value passes
        // `read(...) or 0` (True is truthy) and `True + 1 == 2` because Python
        // `bool` is a subclass of `int`. Rust must yield 2, not a
        // NonNumericIncrement error.
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"flag": true}));
        let action = Action::new("increment").with_param("target", "flag");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["flag"], json!(2));
    }

    #[test]
    fn increment_over_false_current_reads_as_zero() {
        // pyfly parity: `False or 0` is `0`, then `0 + 1 == 1`.
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"flag": false}));
        let action = Action::new("increment").with_param("target", "flag");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["flag"], json!(1));
    }

    #[test]
    fn log_is_a_noop_on_context() {
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"x": 1}));
        let action = Action::new("log").with_param("value", "fired");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts, fact(json!({"x": 1})));
    }

    // ----- registry semantics --------------------------------------------

    #[test]
    fn unsupported_type_is_an_error() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let action = Action::new("calculate");
        let err = registry.apply(&action, &mut facts).unwrap_err();
        assert_eq!(err, ActionError::Unsupported("calculate".into()));
    }

    #[test]
    fn closure_satisfies_action_handler() {
        // pyfly parity: a plain callable is a valid handler.
        let registry =
            ActionRegistry::empty().with_handler("call", |action: &Action, facts: &mut Fact| {
                facts.insert("fired".into(), json!(action.params.get("target")));
                Ok(())
            });
        let mut facts = Fact::new();
        let action = Action::new("call").with_param("target", "audit");
        registry.apply(&action, &mut facts).unwrap();
        assert_eq!(facts["fired"], json!("audit"));
    }

    #[test]
    fn custom_handler_overrides_builtin() {
        // pyfly parity: a custom "set" shadows the builtin entirely.
        let registry = ActionRegistry::default()
            .with_handler("set", |_action: &Action, _facts: &mut Fact| Ok(()));
        let mut facts = Fact::new();
        let action = Action::new("set")
            .with_param("target", "x")
            .with_param("value", 99);
        registry.apply(&action, &mut facts).unwrap();
        assert!(
            !facts.contains_key("x"),
            "override should suppress the write"
        );
    }

    #[test]
    fn custom_handler_is_additive_to_builtins() {
        let registry =
            ActionRegistry::default().with_handler("noop", |_a: &Action, _f: &mut Fact| Ok(()));
        let mut facts = Fact::new();
        let outcome = registry.execute(
            &[
                Action::new("set")
                    .with_param("target", "x")
                    .with_param("value", 42),
                Action::new("noop"),
            ],
            &mut facts,
        );
        assert_eq!(facts["x"], json!(42));
        assert!(outcome.error.is_none());
        assert_eq!(outcome.executed.len(), 2);
    }

    // ----- isolation ------------------------------------------------------

    #[test]
    fn execute_isolates_failing_action_and_continues() {
        let registry = ActionRegistry::default();
        let mut facts = Fact::new();
        let outcome = registry.execute(
            &[
                Action::new("unknown_xyz").with_param("target", "irrelevant"),
                Action::new("set")
                    .with_param("target", "ok")
                    .with_param("value", true),
            ],
            &mut facts,
        );
        assert_eq!(facts["ok"], json!(true), "sibling set should still run");
        let error = outcome.error.expect("error must be recorded");
        assert!(error.contains("unknown_xyz"), "error: {error}");
        assert_eq!(
            outcome
                .executed
                .iter()
                .map(|a| a.action_type.as_str())
                .collect::<Vec<_>>(),
            ["set"]
        );
    }

    #[test]
    fn execute_all_success_has_no_error() {
        let registry = ActionRegistry::default();
        let mut facts = fact(json!({"count": 0}));
        let outcome = registry.execute(
            &[
                Action::new("increment").with_param("target", "count"),
                Action::new("increment")
                    .with_param("target", "count")
                    .with_param("value", 5),
            ],
            &mut facts,
        );
        assert!(outcome.error.is_none());
        assert_eq!(facts["count"], json!(6));
    }
}
