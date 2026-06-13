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

//! Saga composition — orchestrate several sagas as a DAG.
//!
//! The Rust spelling of pyfly's `pyfly.transactional.saga.composition`
//! subpackage: run multiple registered sagas in dependency order, wiring each
//! saga's output into downstream sagas' input, running same-layer sagas
//! concurrently, and compensating all completed sagas in reverse on a
//! failure.
//!
//! | pyfly symbol             | Rust spelling                              |
//! |--------------------------|--------------------------------------------|
//! | `SagaDataFlow`           | [`SagaDataFlow`]                           |
//! | `CompositionEntry`       | [`CompositionEntry`]                       |
//! | `SagaComposition`        | [`SagaComposition`]                        |
//! | `SagaCompositionBuilder` | [`SagaCompositionBuilder`]                 |
//! | `CompositionContext`     | [`CompositionContext`]                     |
//! | `DataFlowManager`        | [`DataFlowManager`]                        |
//! | `CompositionValidator`   | [`CompositionValidator`]                   |
//! | `CompensationManager`    | folded into [`SagaCompositor`] rollback    |
//! | `SagaCompositor`         | [`SagaCompositor`]                         |
//!
//! Each [`SagaCompositor`] entry pairs a named [`Saga`] with its dependency
//! edges and data-flow wiring. A saga's output is read from the
//! [`StepContext`] it ran with (its step results). Because a Rust [`Saga`]
//! only compensates its own steps when its own execute fails, the compositor
//! takes an optional per-saga *undo* closure that rolls a completed saga back
//! when a *downstream* saga fails — the cross-saga compensation pyfly's
//! `CompensationManager` performs by re-running each saga's built-in
//! compensation.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_orchestration::{Saga, SagaCompositionBuilder, SagaCompositor, Step};
//! use serde_json::json;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! // Two sagas: reserve then charge, charge depends on reserve.
//! let reserve = Saga::new("reserve").step(Step::with_context("hold", |ctx| async move {
//!     ctx.set_result("hold", json!({"reservation_id": "R-1"}));
//!     Ok(())
//! }));
//! let charge = Saga::new("charge").step(Step::with_context("bill", |ctx| async move {
//!     // The reservation id flowed in from the upstream saga's input.
//!     assert_eq!(ctx.input_field("reservation_id").unwrap(), json!("R-1"));
//!     Ok(())
//! }));
//!
//! let composition = SagaCompositionBuilder::new("checkout")
//!     .saga("reserve").add()
//!     .saga("charge")
//!         .depends_on(["reserve"])
//!         // wire reserve's "hold" step result into charge's input.
//!         .data_flow("reserve", Some("hold"), None)
//!         .add()
//!     .build()
//!     .expect("valid composition");
//!
//! let compositor = SagaCompositor::new()
//!     .register("reserve", Arc::new(reserve))
//!     .register("charge", Arc::new(charge));
//!
//! let ctx = compositor.execute(&composition, json!({})).await;
//! assert!(ctx.is_success());
//! assert_eq!(ctx.completed(), ["reserve", "charge"]);
//! # });
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::observability::{NoOpOrchestrationEvents, OrchestrationEvents};
use crate::saga::Outcome;
use crate::step_context::StepContext;
use crate::{ActionFuture, BoxError, Saga, SagaStatus};

/// Maps output from one saga to the input of another — pyfly's
/// `SagaDataFlow`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SagaDataFlow {
    /// Name of the saga whose result provides the data.
    pub source_saga: String,
    /// Specific step within the source saga whose result is used; `None`
    /// uses the source saga's whole result map.
    pub source_step: Option<String>,
    /// Key under which the resolved value is placed in the target saga's
    /// input; `None` merges the value directly (it must be a JSON object).
    pub target_key: Option<String>,
}

/// A saga within a [`SagaComposition`] — pyfly's `CompositionEntry`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompositionEntry {
    /// Registered name of the saga.
    pub saga_name: String,
    /// Names of entries that must complete before this one.
    pub depends_on: Vec<String>,
    /// How upstream output wires into this saga's input.
    pub data_flows: Vec<SagaDataFlow>,
}

/// An immutable multi-saga composition definition — pyfly's
/// `SagaComposition`. Entries are keyed by saga name.
#[derive(Clone, Debug, Default)]
pub struct SagaComposition {
    /// The composition name.
    pub name: String,
    /// Entries keyed by saga name.
    pub entries: BTreeMap<String, CompositionEntry>,
}

impl SagaComposition {
    /// The dependency graph as `name -> dependencies`.
    fn deps(&self) -> BTreeMap<String, Vec<String>> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.depends_on.clone()))
            .collect()
    }
}

/// Errors produced building or running a [`SagaComposition`].
#[derive(Debug, Error)]
pub enum CompositionError {
    /// The composition has no entries.
    #[error("composition {composition:?}: must contain at least one saga entry")]
    Empty {
        /// The composition name.
        composition: String,
    },
    /// A `depends_on` reference points at a non-existent entry.
    #[error("composition {composition:?}: entry {entry:?} depends on unknown {dependency:?}")]
    UnknownDependency {
        /// The composition name.
        composition: String,
        /// The entry declaring the dependency.
        entry: String,
        /// The unknown dependency name.
        dependency: String,
    },
    /// A data-flow `source_saga` references a non-existent entry.
    #[error("composition {composition:?}: entry {entry:?} data flow references unknown source {source_saga:?}")]
    UnknownDataFlowSource {
        /// The composition name.
        composition: String,
        /// The entry declaring the data flow.
        entry: String,
        /// The unknown source-saga name.
        source_saga: String,
    },
    /// The dependency graph contains a cycle.
    #[error("composition {composition:?}: dependency cycle detected")]
    Cycle {
        /// The composition name.
        composition: String,
    },
}

/// Validates a [`SagaComposition`] for structural correctness — pyfly's
/// `CompositionValidator`.
pub struct CompositionValidator;

impl CompositionValidator {
    /// Validates the composition: every `depends_on` and data-flow source
    /// resolves to a real entry, and the dependency graph is acyclic.
    pub fn validate(composition: &SagaComposition) -> Result<(), CompositionError> {
        if composition.entries.is_empty() {
            return Err(CompositionError::Empty {
                composition: composition.name.clone(),
            });
        }
        for (name, entry) in &composition.entries {
            for dep in &entry.depends_on {
                if !composition.entries.contains_key(dep) {
                    return Err(CompositionError::UnknownDependency {
                        composition: composition.name.clone(),
                        entry: name.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
            for flow in &entry.data_flows {
                if !composition.entries.contains_key(&flow.source_saga) {
                    return Err(CompositionError::UnknownDataFlowSource {
                        composition: composition.name.clone(),
                        entry: name.clone(),
                        source_saga: flow.source_saga.clone(),
                    });
                }
            }
        }
        // Cycle detection via layered topological ordering.
        compute_layers(&composition.deps()).map_err(|_| CompositionError::Cycle {
            composition: composition.name.clone(),
        })?;
        Ok(())
    }
}

/// Computes DAG execution layers from a `node -> dependencies` map — the
/// Rust spelling of pyfly's `SagaTopology.compute_layers`. Each returned
/// layer is a set of names with no unresolved dependencies; same-layer
/// entries run concurrently. Returns `Err` on a cycle.
fn compute_layers(deps: &BTreeMap<String, Vec<String>>) -> Result<Vec<Vec<String>>, ()> {
    let mut remaining: BTreeMap<String, Vec<String>> = deps.clone();
    let mut done: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut layers = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|(_, d)| d.iter().all(|dep| done.contains(dep)))
            .map(|(name, _)| name.clone())
            .collect();
        if ready.is_empty() {
            return Err(()); // cycle or dangling dependency
        }
        for name in &ready {
            remaining.remove(name);
            done.insert(name.clone());
        }
        layers.push(ready);
    }
    Ok(layers)
}

/// Fluent builder producing a validated [`SagaComposition`] — pyfly's
/// `SagaCompositionBuilder`.
pub struct SagaCompositionBuilder {
    name: String,
    entries: BTreeMap<String, CompositionEntry>,
}

/// Builder for a single [`CompositionEntry`] — pyfly's `_EntryBuilder`,
/// created by [`SagaCompositionBuilder::saga`] and finalised by
/// [`EntryBuilder::add`].
pub struct EntryBuilder {
    parent: SagaCompositionBuilder,
    entry: CompositionEntry,
}

impl SagaCompositionBuilder {
    /// Begins a composition named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            entries: BTreeMap::new(),
        }
    }

    /// Begins defining an entry for the named saga.
    pub fn saga(self, saga_name: impl Into<String>) -> EntryBuilder {
        let saga_name = saga_name.into();
        EntryBuilder {
            parent: self,
            entry: CompositionEntry {
                saga_name,
                depends_on: Vec::new(),
                data_flows: Vec::new(),
            },
        }
    }

    /// Builds and validates the [`SagaComposition`].
    pub fn build(self) -> Result<SagaComposition, CompositionError> {
        let composition = SagaComposition {
            name: self.name,
            entries: self.entries,
        };
        CompositionValidator::validate(&composition)?;
        Ok(composition)
    }
}

impl EntryBuilder {
    /// Declares dependencies for this entry.
    #[must_use]
    pub fn depends_on<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.entry.depends_on = names.into_iter().map(Into::into).collect();
        self
    }

    /// Declares a data-flow mapping from an upstream saga's `source_step`
    /// (or whole result when `None`) into this saga's input under
    /// `target_key` (or merged directly when `None`).
    #[must_use]
    pub fn data_flow(
        mut self,
        source_saga: impl Into<String>,
        source_step: Option<&str>,
        target_key: Option<&str>,
    ) -> Self {
        self.entry.data_flows.push(SagaDataFlow {
            source_saga: source_saga.into(),
            source_step: source_step.map(str::to_string),
            target_key: target_key.map(str::to_string),
        });
        self
    }

    /// Finalises this entry and returns to the parent builder.
    pub fn add(mut self) -> SagaCompositionBuilder {
        self.parent
            .entries
            .insert(self.entry.saga_name.clone(), self.entry);
        self.parent
    }
}

/// Mutable state accompanying a composition run — pyfly's
/// `CompositionContext`. The compositor populates it as sagas complete; the
/// [`DataFlowManager`] reads from it to resolve downstream input.
#[derive(Debug, Default)]
pub struct CompositionContext {
    /// Unique id for this composition run.
    pub correlation_id: String,
    /// The composition name.
    pub composition_name: String,
    /// Each completed saga's terminal [`Outcome`], keyed by saga name.
    pub saga_results: BTreeMap<String, Outcome>,
    /// Each completed saga's step-result map (the `StepContext` blackboard),
    /// keyed by saga name — the source of cross-saga data flow.
    pub saga_step_results: BTreeMap<String, BTreeMap<String, Value>>,
    /// The resolved input each saga ran with, keyed by saga name.
    pub saga_inputs: BTreeMap<String, Value>,
    /// Names of sagas that completed successfully, in completion order.
    pub completed_sagas: Vec<String>,
    /// Names of sagas that were compensated after a failure, in
    /// compensation order.
    pub compensated_sagas: Vec<String>,
    /// Rendered error that failed the composition, if any.
    pub error: Option<String>,
}

impl CompositionContext {
    /// Whether the composition completed without error.
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Names of sagas that completed successfully, in completion order.
    pub fn completed(&self) -> &[String] {
        &self.completed_sagas
    }

    /// Names of sagas compensated after a failure, in compensation order.
    pub fn compensated(&self) -> &[String] {
        &self.compensated_sagas
    }
}

/// Resolves a saga's input from its data-flow declarations — pyfly's
/// `DataFlowManager`.
pub struct DataFlowManager;

impl DataFlowManager {
    /// Builds the input for `entry` by layering its data-flow values onto a
    /// copy of `initial_input` (which must be a JSON object to be used as
    /// the base; otherwise a fresh object is used) — pyfly's
    /// `resolve_input`.
    pub fn resolve_input(
        entry: &CompositionEntry,
        ctx: &CompositionContext,
        initial_input: &Value,
    ) -> Value {
        if entry.data_flows.is_empty() {
            return initial_input.clone();
        }
        let mut resolved = match initial_input {
            Value::Object(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        for flow in &entry.data_flows {
            let value = match &flow.source_step {
                Some(step) => ctx
                    .saga_step_results
                    .get(&flow.source_saga)
                    .and_then(|results| results.get(step).cloned()),
                None => ctx.saga_step_results.get(&flow.source_saga).map(|results| {
                    Value::Object(
                        results
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    )
                }),
            };
            let Some(value) = value else { continue };
            match (&flow.target_key, &value) {
                (Some(key), _) => {
                    resolved.insert(key.clone(), value);
                }
                (None, Value::Object(obj)) => {
                    for (k, v) in obj {
                        resolved.insert(k.clone(), v.clone());
                    }
                }
                (None, _) => {
                    resolved.insert(flow.source_saga.clone(), value);
                }
            }
        }
        Value::Object(resolved)
    }
}

/// The undo closure rolling back a completed saga when a downstream saga
/// fails — the Rust spelling of pyfly's `CompensationManager` re-running a
/// saga's built-in compensation. Receives the saga's [`StepContext`] (with
/// its step results) so the rollback can read what the forward run produced.
type UndoFn = Arc<dyn Fn(StepContext) -> ActionFuture + Send + Sync>;

struct Registered {
    saga: Arc<Saga>,
    undo: Option<UndoFn>,
}

/// Executes a [`SagaComposition`] as a DAG — pyfly's `SagaCompositor`.
///
/// Sagas in the same dependency layer run concurrently; on any saga failure
/// the compositor rolls back the completed sagas in reverse completion order
/// using their registered undo closures.
#[derive(Default)]
pub struct SagaCompositor {
    sagas: BTreeMap<String, Registered>,
}

impl SagaCompositor {
    /// Returns an empty compositor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a named saga (no cross-saga undo).
    #[must_use]
    pub fn register(mut self, name: impl Into<String>, saga: Arc<Saga>) -> Self {
        self.sagas
            .insert(name.into(), Registered { saga, undo: None });
        self
    }

    /// Registers a named saga plus an undo closure rolling it back when a
    /// downstream saga fails — the cross-saga compensation hook.
    #[must_use]
    pub fn register_with_undo<F, Fut>(
        mut self,
        name: impl Into<String>,
        saga: Arc<Saga>,
        undo: F,
    ) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        let undo: UndoFn = Arc::new(move |ctx| Box::pin(undo(ctx)));
        self.sagas.insert(
            name.into(),
            Registered {
                saga,
                undo: Some(undo),
            },
        );
        self
    }

    /// Executes `composition`, seeding root sagas with `initial_input` and
    /// firing lifecycle hooks via the [`NoOpOrchestrationEvents`] listener.
    pub async fn execute(
        &self,
        composition: &SagaComposition,
        initial_input: Value,
    ) -> CompositionContext {
        self.execute_with_listener(composition, initial_input, &NoOpOrchestrationEvents)
            .await
    }

    /// Executes `composition` firing lifecycle hooks on `listener` (each
    /// saga also fires its own per-step hooks).
    pub async fn execute_with_listener(
        &self,
        composition: &SagaComposition,
        initial_input: Value,
        listener: &dyn OrchestrationEvents,
    ) -> CompositionContext {
        let mut ctx = CompositionContext {
            correlation_id: firefly_kernel::new_correlation_id(),
            composition_name: composition.name.clone(),
            ..Default::default()
        };
        // compute_layers cannot fail for a validated composition.
        let layers = match compute_layers(&composition.deps()) {
            Ok(layers) => layers,
            Err(()) => {
                ctx.error = Some(format!(
                    "composition {:?}: dependency cycle detected",
                    composition.name
                ));
                return ctx;
            }
        };

        'outer: for layer in layers {
            // Resolve input + run each saga in the layer concurrently.
            let mut futures = Vec::with_capacity(layer.len());
            for saga_name in &layer {
                let entry = &composition.entries[saga_name];
                let input = DataFlowManager::resolve_input(entry, &ctx, &initial_input);
                ctx.saga_inputs.insert(saga_name.clone(), input.clone());
                let registered = self.sagas.get(saga_name);
                let saga_name = saga_name.clone();
                let cid = ctx.correlation_id.clone();
                futures.push(async move {
                    let step_ctx = StepContext::with_input(input);
                    step_ctx.set_correlation_id(cid);
                    let result = match registered {
                        Some(reg) => reg
                            .saga
                            .run_with_context(&step_ctx)
                            .await
                            .unwrap_or_else(|failure| failure.outcome().clone()),
                        None => Outcome {
                            saga: saga_name.clone(),
                            status: SagaStatus::Failed,
                            steps_executed: Vec::new(),
                            steps_rolled: Vec::new(),
                            error: Some(format!("saga {saga_name:?} not registered")),
                            started_at: chrono::Utc::now(),
                            finished_at: chrono::Utc::now(),
                        },
                    };
                    (saga_name, result, step_ctx.results())
                });
            }
            let results = futures::future::join_all(futures).await;

            for (saga_name, outcome, step_results) in results {
                let succeeded = outcome.status == SagaStatus::Completed;
                ctx.saga_step_results
                    .insert(saga_name.clone(), step_results.into_iter().collect());
                ctx.saga_results.insert(saga_name.clone(), outcome.clone());
                ctx.completed_sagas.push(saga_name.clone());
                if !succeeded {
                    ctx.error = Some(format!(
                        "saga {:?} failed in composition {:?}: {}",
                        saga_name,
                        composition.name,
                        outcome.error.as_deref().unwrap_or("unknown"),
                    ));
                    break 'outer;
                }
            }
        }

        if ctx.error.is_some() {
            // The failing saga is the last in `completed_sagas`; roll back the
            // sagas that completed *successfully* before it, newest-first.
            let to_compensate: Vec<String> = ctx
                .completed_sagas
                .iter()
                .filter(|name| {
                    ctx.saga_results
                        .get(*name)
                        .map(|o| o.status == SagaStatus::Completed)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            listener
                .on_compensation_started(&composition.name, &ctx.correlation_id)
                .await;
            for saga_name in to_compensate.iter().rev() {
                if let Some(reg) = self.sagas.get(saga_name) {
                    if let Some(undo) = &reg.undo {
                        let step_ctx = StepContext::new();
                        if let Some(results) = ctx.saga_step_results.get(saga_name) {
                            for (step, value) in results {
                                step_ctx.set_result(step, value.clone());
                            }
                        }
                        let err = undo(step_ctx).await.err().map(|e| e.to_string());
                        listener
                            .on_step_compensated(
                                &composition.name,
                                &ctx.correlation_id,
                                saga_name,
                                err.as_deref(),
                            )
                            .await;
                    }
                }
                ctx.compensated_sagas.push(saga_name.clone());
            }
        }

        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Step;
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn validator_rejects_empty() {
        let composition = SagaComposition {
            name: "c".into(),
            entries: BTreeMap::new(),
        };
        assert!(matches!(
            CompositionValidator::validate(&composition),
            Err(CompositionError::Empty { .. })
        ));
    }

    #[test]
    fn validator_rejects_unknown_dependency() {
        let err = SagaCompositionBuilder::new("c")
            .saga("a")
            .depends_on(["ghost"])
            .add()
            .build()
            .expect_err("ghost dep");
        assert!(matches!(err, CompositionError::UnknownDependency { .. }));
    }

    #[test]
    fn validator_rejects_unknown_data_flow_source() {
        let err = SagaCompositionBuilder::new("c")
            .saga("a")
            .data_flow("ghost", None, None)
            .add()
            .build()
            .expect_err("ghost source");
        assert!(matches!(
            err,
            CompositionError::UnknownDataFlowSource { .. }
        ));
    }

    #[test]
    fn validator_rejects_cycle() {
        let err = SagaCompositionBuilder::new("c")
            .saga("a")
            .depends_on(["b"])
            .add()
            .saga("b")
            .depends_on(["a"])
            .add()
            .build()
            .expect_err("cycle");
        assert!(matches!(err, CompositionError::Cycle { .. }));
    }

    #[tokio::test]
    async fn runs_sagas_in_dependency_order_with_data_flow() {
        let reserve = Saga::new("reserve").step(Step::with_context("hold", |ctx| async move {
            ctx.set_result("hold", json!({"reservation_id": "R-1"}));
            Ok(())
        }));
        let seen = Arc::new(Mutex::new(None));
        let seen2 = seen.clone();
        let charge = Saga::new("charge").step(Step::with_context("bill", move |ctx| {
            let seen = seen2.clone();
            async move {
                *seen.lock().unwrap() = ctx.input_field("reservation_id");
                Ok(())
            }
        }));

        let composition = SagaCompositionBuilder::new("checkout")
            .saga("reserve")
            .add()
            .saga("charge")
            .depends_on(["reserve"])
            .data_flow("reserve", Some("hold"), None)
            .add()
            .build()
            .expect("valid");

        let compositor = SagaCompositor::new()
            .register("reserve", Arc::new(reserve))
            .register("charge", Arc::new(charge));
        let ctx = compositor.execute(&composition, json!({})).await;
        assert!(ctx.is_success());
        assert_eq!(ctx.completed(), ["reserve", "charge"]);
        assert_eq!(*seen.lock().unwrap(), Some(json!("R-1")));
    }

    #[tokio::test]
    async fn compensates_completed_sagas_in_reverse_on_failure() {
        let undone = Arc::new(Mutex::new(Vec::<String>::new()));
        let undo_a = undone.clone();
        let undo_b = undone.clone();

        let a = Saga::new("a").step(Step::new("a1", || async { Ok(()) }));
        let b = Saga::new("b").step(Step::new("b1", || async { Ok(()) }));
        // c fails.
        let c = Saga::new("c").step(Step::new("c1", || async { Err("boom".into()) }));

        let composition = SagaCompositionBuilder::new("chain")
            .saga("a")
            .add()
            .saga("b")
            .depends_on(["a"])
            .add()
            .saga("c")
            .depends_on(["b"])
            .add()
            .build()
            .expect("valid");

        let compositor = SagaCompositor::new()
            .register_with_undo("a", Arc::new(a), move |_ctx| {
                let undone = undo_a.clone();
                async move {
                    undone.lock().unwrap().push("a".to_string());
                    Ok(())
                }
            })
            .register_with_undo("b", Arc::new(b), move |_ctx| {
                let undone = undo_b.clone();
                async move {
                    undone.lock().unwrap().push("b".to_string());
                    Ok(())
                }
            })
            .register("c", Arc::new(c));

        let ctx = compositor.execute(&composition, json!({})).await;
        assert!(!ctx.is_success());
        assert!(ctx.error.as_deref().unwrap().contains("c"));
        // a and b completed; c failed → roll back b then a (reverse order).
        assert_eq!(*undone.lock().unwrap(), ["b", "a"]);
        assert_eq!(ctx.compensated(), ["b", "a"]);
    }

    #[tokio::test]
    async fn same_layer_sagas_run_concurrently() {
        use std::time::Duration;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mk = |name: &str, barrier: Arc<tokio::sync::Barrier>| {
            Saga::new(name).step(Step::new("wait", move || {
                let barrier = barrier.clone();
                async move {
                    barrier.wait().await;
                    Ok(())
                }
            }))
        };
        let composition = SagaCompositionBuilder::new("parallel")
            .saga("a")
            .add()
            .saga("b")
            .add()
            .build()
            .expect("valid");
        let compositor = SagaCompositor::new()
            .register("a", Arc::new(mk("a", barrier.clone())))
            .register("b", Arc::new(mk("b", barrier.clone())));
        // Sequential execution would deadlock on the barrier; a timeout
        // guards the assertion.
        let ctx = tokio::time::timeout(
            Duration::from_millis(200),
            compositor.execute(&composition, json!({})),
        )
        .await
        .expect("same-layer sagas must run concurrently");
        assert!(ctx.is_success());
    }
}
