//! Inter-step data passing — the runtime blackboard threaded through every
//! step of a saga / workflow / TCC run.
//!
//! The Rust spelling of pyfly's runtime `ExecutionContext` argument
//! injection (`pyfly.transactional.core.context` +
//! `core.argument.ArgumentResolver`). pyfly steps declare typed inputs via
//! `Annotated[T, Input(...)]` / `FromStep("id")` / `Variable(...)` /
//! `Header(...)` markers resolved from the live context. Rust closures are
//! zero-arg, so instead of decorator-driven injection the engines hand each
//! step an `&StepContext` it reads explicitly:
//!
//! ```
//! use firefly_orchestration::StepContext;
//! use serde_json::json;
//!
//! let ctx = StepContext::new();
//! // A prior step published its result under its own name.
//! ctx.set_result("reserve", json!({"reservation_id": "R-1"}));
//! // A later step consumes it (pyfly's `Annotated[dict, FromStep("reserve")]`).
//! let reservation = ctx.result("reserve").unwrap();
//! assert_eq!(reservation["reservation_id"], "R-1");
//! // And the field accessor mirrors pyfly's `FromStep("reserve", field=...)`.
//! assert_eq!(ctx.result_field("reserve", "reservation_id").unwrap(), json!("R-1"));
//! ```
//!
//! The context is `Send + Sync` and cheaply `clone`-able (an `Arc` inside),
//! so engines can hand the same handle to concurrent workflow-wave nodes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

#[derive(Debug, Default)]
struct Inner {
    /// Per-step JSON results — pyfly's `ctx.get_step_result(step_id)`.
    results: BTreeMap<String, Value>,
    /// Mutable user variables — pyfly's `@SetVariable` / `@Variable`.
    variables: BTreeMap<String, Value>,
    /// Free-form headers (HTTP / message-envelope keys) — pyfly's
    /// `ctx.headers` consumed by `@Header` / `@Headers`.
    headers: BTreeMap<String, String>,
    /// The original engine input payload — pyfly's `ctx.input` consumed by
    /// `@Input`.
    input: Value,
    /// Correlation id of the run — pyfly's `@CorrelationId`.
    correlation_id: String,
}

/// A typed, thread-safe blackboard threaded through a saga / workflow / TCC
/// run so a step can consume the outputs of prior steps.
///
/// Mirrors pyfly's runtime `ExecutionContext` injection surface: step
/// results ([`Self::set_result`] / [`Self::result`]), mutable variables
/// ([`Self::set_variable`] / [`Self::variable`]), headers, the engine input
/// and the correlation id. Every accessor returns owned clones so callers
/// never hold the internal lock.
#[derive(Debug, Clone, Default)]
pub struct StepContext {
    inner: Arc<Mutex<Inner>>,
}

impl StepContext {
    /// Creates an empty context with an empty input and correlation id.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a context seeded with the engine input payload — pyfly's
    /// `ExecutionContext(input=...)`.
    pub fn with_input(input: Value) -> Self {
        let ctx = Self::new();
        ctx.locked().input = input;
        ctx
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .expect("firefly/orchestration: step-context lock poisoned")
    }

    // -- correlation id -----------------------------------------------------

    /// Sets the run's correlation id — pyfly's `ctx.correlation_id`.
    pub fn set_correlation_id(&self, id: impl Into<String>) {
        self.locked().correlation_id = id.into();
    }

    /// The run's correlation id (empty when unset) — pyfly's
    /// `@CorrelationId`.
    pub fn correlation_id(&self) -> String {
        self.locked().correlation_id.clone()
    }

    // -- input --------------------------------------------------------------

    /// Replaces the engine input payload.
    pub fn set_input(&self, input: Value) {
        self.locked().input = input;
    }

    /// The engine input payload — pyfly's `@Input()`.
    pub fn input(&self) -> Value {
        self.locked().input.clone()
    }

    /// A single field of the input payload — pyfly's `@Input(field=...)`.
    /// Returns `None` when the input is not a JSON object or the field is
    /// absent.
    pub fn input_field(&self, field: &str) -> Option<Value> {
        read_field(&self.locked().input, field)
    }

    // -- step results -------------------------------------------------------

    /// Publishes a step's JSON result under its step name — the engine's
    /// analogue of pyfly's `ctx.record_step_success(step_id, result, ...)`.
    pub fn set_result(&self, step: impl Into<String>, result: Value) {
        self.locked().results.insert(step.into(), result);
    }

    /// The result a prior step published, if any — pyfly's
    /// `@FromStep("step")` / `ctx.get_step_result(step)`.
    pub fn result(&self, step: &str) -> Option<Value> {
        self.locked().results.get(step).cloned()
    }

    /// A single field of a prior step's result — pyfly's
    /// `@FromStep("step", field=...)`. Returns `None` when the step has no
    /// result, the result is not a JSON object, or the field is absent.
    pub fn result_field(&self, step: &str, field: &str) -> Option<Value> {
        self.locked()
            .results
            .get(step)
            .and_then(|v| read_field(v, field))
    }

    /// `true` when `step` published a result.
    pub fn has_result(&self, step: &str) -> bool {
        self.locked().results.contains_key(step)
    }

    /// Every published step result, keyed by step name — pyfly's
    /// `{sid: rec.result for ...}` condition namespace.
    pub fn results(&self) -> BTreeMap<String, Value> {
        self.locked().results.clone()
    }

    // -- variables ----------------------------------------------------------

    /// Sets a mutable context variable — pyfly's `@SetVariable` /
    /// `ctx.set_variable(key, value)`.
    pub fn set_variable(&self, key: impl Into<String>, value: Value) {
        self.locked().variables.insert(key.into(), value);
    }

    /// A context variable, if set — pyfly's `@Variable(name)` /
    /// `ctx.get_variable(key)`.
    pub fn variable(&self, key: &str) -> Option<Value> {
        self.locked().variables.get(key).cloned()
    }

    /// Every context variable — pyfly's `@Variables` /
    /// `ctx.get_all_variables()`.
    pub fn variables(&self) -> BTreeMap<String, Value> {
        self.locked().variables.clone()
    }

    // -- headers ------------------------------------------------------------

    /// Sets a header — a key of pyfly's `ctx.headers`.
    pub fn set_header(&self, name: impl Into<String>, value: impl Into<String>) {
        self.locked().headers.insert(name.into(), value.into());
    }

    /// A header value, if present — pyfly's `@Header(name)` /
    /// `ctx.get_header(name)`.
    pub fn header(&self, name: &str) -> Option<String> {
        self.locked().headers.get(name).cloned()
    }

    /// Every header — pyfly's `@Headers` / `dict(ctx.headers)`.
    pub fn headers(&self) -> BTreeMap<String, String> {
        self.locked().headers.clone()
    }

    // -- durable snapshot ---------------------------------------------------

    /// Serializes the context to a JSON snapshot suitable for persisting in
    /// an [`ExecutionState`](crate::ExecutionState) payload — the unit a
    /// [`PersistenceProvider`](crate::PersistenceProvider) round-trips for
    /// durable suspend/resume across restart. Mirrors the shape of pyfly's
    /// `ExecutionContext.to_dict()` results / variables / headers / input.
    pub fn to_snapshot(&self) -> Value {
        let inner = self.locked();
        serde_json::json!({
            "correlation_id": inner.correlation_id,
            "input": inner.input,
            "results": Value::Object(inner.results.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            "variables": Value::Object(inner.variables.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
            "headers": Value::Object(
                inner
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            ),
        })
    }

    /// Rebuilds a context from a [`Self::to_snapshot`] payload — pyfly's
    /// `ExecutionContext.from_dict()`. Unknown / missing fields default to
    /// empty, so a partial snapshot still restores cleanly.
    pub fn from_snapshot(snapshot: &Value) -> Self {
        let ctx = Self::new();
        let mut inner = ctx.locked();
        if let Some(cid) = snapshot.get("correlation_id").and_then(Value::as_str) {
            inner.correlation_id = cid.to_string();
        }
        if let Some(input) = snapshot.get("input") {
            inner.input = input.clone();
        }
        if let Some(map) = snapshot.get("results").and_then(Value::as_object) {
            inner.results = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        if let Some(map) = snapshot.get("variables").and_then(Value::as_object) {
            inner.variables = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        if let Some(map) = snapshot.get("headers").and_then(Value::as_object) {
            inner.headers = map
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();
        }
        drop(inner);
        ctx
    }
}

/// Reads a single field out of a JSON object, mirroring pyfly's
/// `_read_field`: `None` for non-objects or absent fields.
fn read_field(value: &Value, field: &str) -> Option<Value> {
    value.as_object().and_then(|m| m.get(field).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Port of pyfly test_argument.py FromStep resolution: a later step reads
    // the result a prior step published under its name.
    #[test]
    fn from_step_reads_prior_step_result() {
        let ctx = StepContext::new();
        ctx.set_result("reserve", json!({"reservation_id": "R-1"}));
        assert_eq!(ctx.result("reserve").unwrap()["reservation_id"], "R-1");
        assert_eq!(
            ctx.result_field("reserve", "reservation_id").unwrap(),
            json!("R-1")
        );
        assert!(ctx.has_result("reserve"));
        assert!(!ctx.has_result("charge"));
        assert!(ctx.result("charge").is_none());
    }

    // Port of pyfly Input / Input(field=...) resolution.
    #[test]
    fn input_and_input_field() {
        let ctx = StepContext::with_input(json!({"amount": 100, "currency": "EUR"}));
        assert_eq!(ctx.input()["amount"], 100);
        assert_eq!(ctx.input_field("currency").unwrap(), json!("EUR"));
        assert!(ctx.input_field("missing").is_none());
    }

    // Port of pyfly @SetVariable / @Variable / @Variables round-trip.
    #[test]
    fn variables_round_trip() {
        let ctx = StepContext::new();
        ctx.set_variable("token", json!("abc"));
        ctx.set_variable("count", json!(3));
        assert_eq!(ctx.variable("token").unwrap(), json!("abc"));
        assert!(ctx.variable("nope").is_none());
        let all = ctx.variables();
        assert_eq!(all.len(), 2);
        assert_eq!(all["count"], json!(3));
    }

    // Port of pyfly @Header / @Headers / @CorrelationId resolution.
    #[test]
    fn headers_and_correlation_id() {
        let ctx = StepContext::new();
        ctx.set_correlation_id("cid-7");
        ctx.set_header("x-tenant", "acme");
        assert_eq!(ctx.correlation_id(), "cid-7");
        assert_eq!(ctx.header("x-tenant").unwrap(), "acme");
        assert!(ctx.header("x-missing").is_none());
        assert_eq!(ctx.headers().len(), 1);
    }

    // Result field on a non-object value returns None (pyfly _read_field).
    #[test]
    fn result_field_on_non_object_is_none() {
        let ctx = StepContext::new();
        ctx.set_result("plain", json!("just-a-string"));
        assert!(ctx.result_field("plain", "anything").is_none());
        assert_eq!(ctx.result("plain").unwrap(), json!("just-a-string"));
    }

    // The context is shared: a clone observes writes through the original.
    #[test]
    fn clones_share_state() {
        let ctx = StepContext::new();
        let clone = ctx.clone();
        ctx.set_result("a", json!(1));
        clone.set_result("b", json!(2));
        assert_eq!(ctx.results().len(), 2);
        assert_eq!(clone.result("a").unwrap(), json!(1));
    }

    // Durable snapshot round-trip (pyfly ExecutionContext.to_dict/from_dict):
    // results, variables, headers, input and correlation id all survive.
    #[test]
    fn snapshot_round_trip() {
        let ctx = StepContext::with_input(json!({"order": "O-9"}));
        ctx.set_correlation_id("cid-42");
        ctx.set_result("reserve", json!({"id": "R-1"}));
        ctx.set_variable("token", json!("abc"));
        ctx.set_header("x-tenant", "acme");

        let snapshot = ctx.to_snapshot();
        let restored = StepContext::from_snapshot(&snapshot);

        assert_eq!(restored.correlation_id(), "cid-42");
        assert_eq!(restored.input(), json!({"order": "O-9"}));
        assert_eq!(restored.result("reserve").unwrap(), json!({"id": "R-1"}));
        assert_eq!(restored.variable("token").unwrap(), json!("abc"));
        assert_eq!(restored.header("x-tenant").unwrap(), "acme");
    }

    // A partial snapshot still restores cleanly (missing fields default).
    #[test]
    fn from_partial_snapshot_defaults_missing() {
        let restored = StepContext::from_snapshot(&json!({"correlation_id": "only-cid"}));
        assert_eq!(restored.correlation_id(), "only-cid");
        assert!(restored.results().is_empty());
        assert!(restored.variables().is_empty());
    }
}
