//! Definition registry — named saga / workflow / TCC definitions plus the
//! listing accessors the admin crate renders.
//!
//! pyfly spreads this across `SagaRegistry`, `WorkflowRegistry`, and
//! `TccRegistry` (populated by class decorators); the Rust port collapses
//! them into one [`OrchestrationRegistry`] populated by explicit
//! `register_*` calls, since definitions are built with the engine
//! builders rather than discovered from annotations.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::model::ExecutionPattern;
use crate::{Saga, Tcc, Workflow};

/// Summary of one registered definition — the row the admin dashboard and
/// `GET /orchestration/definitions` expose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefinitionInfo {
    /// The definition name.
    pub name: String,
    /// Which engine the definition drives.
    pub pattern: ExecutionPattern,
    /// Step / node / participant names, in declaration order.
    pub steps: Vec<String>,
}

#[derive(Default)]
struct Definitions {
    sagas: BTreeMap<String, Arc<Saga>>,
    workflows: BTreeMap<String, Arc<Workflow>>,
    tccs: BTreeMap<String, Arc<Tcc>>,
}

/// Holds every registered orchestration definition, keyed by name.
///
/// Registering under an existing name replaces the previous definition,
/// matching pyfly's last-decorator-wins registry semantics.
#[derive(Default)]
pub struct OrchestrationRegistry {
    inner: RwLock<Definitions>,
}

impl std::fmt::Debug for OrchestrationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.read();
        f.debug_struct("OrchestrationRegistry")
            .field("sagas", &inner.sagas.keys().collect::<Vec<_>>())
            .field("workflows", &inner.workflows.keys().collect::<Vec<_>>())
            .field("tccs", &inner.tccs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OrchestrationRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Definitions> {
        self.inner
            .read()
            .expect("firefly/orchestration: lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Definitions> {
        self.inner
            .write()
            .expect("firefly/orchestration: lock poisoned")
    }

    /// Registers a saga definition under its name and returns the shared
    /// handle.
    pub fn register_saga(&self, saga: Saga) -> Arc<Saga> {
        let saga = Arc::new(saga);
        self.write()
            .sagas
            .insert(saga.name().to_string(), Arc::clone(&saga));
        saga
    }

    /// Registers a workflow definition under its name and returns the
    /// shared handle.
    pub fn register_workflow(&self, workflow: Workflow) -> Arc<Workflow> {
        let workflow = Arc::new(workflow);
        self.write()
            .workflows
            .insert(workflow.name().to_string(), Arc::clone(&workflow));
        workflow
    }

    /// Registers a TCC definition under its name and returns the shared
    /// handle.
    pub fn register_tcc(&self, tcc: Tcc) -> Arc<Tcc> {
        let tcc = Arc::new(tcc);
        self.write()
            .tccs
            .insert(tcc.name().to_string(), Arc::clone(&tcc));
        tcc
    }

    /// Looks up a saga by name.
    pub fn saga(&self, name: &str) -> Option<Arc<Saga>> {
        self.read().sagas.get(name).cloned()
    }

    /// Looks up a workflow by name.
    pub fn workflow(&self, name: &str) -> Option<Arc<Workflow>> {
        self.read().workflows.get(name).cloned()
    }

    /// Looks up a TCC by name.
    pub fn tcc(&self, name: &str) -> Option<Arc<Tcc>> {
        self.read().tccs.get(name).cloned()
    }

    /// Registered saga names, sorted.
    pub fn saga_names(&self) -> Vec<String> {
        self.read().sagas.keys().cloned().collect()
    }

    /// Registered workflow names, sorted.
    pub fn workflow_names(&self) -> Vec<String> {
        self.read().workflows.keys().cloned().collect()
    }

    /// Registered TCC names, sorted.
    pub fn tcc_names(&self) -> Vec<String> {
        self.read().tccs.keys().cloned().collect()
    }

    /// Every registered definition as a [`DefinitionInfo`] row, sagas
    /// first, then workflows, then TCCs, each group sorted by name.
    pub fn definitions(&self) -> Vec<DefinitionInfo> {
        let inner = self.read();
        let mut out = Vec::new();
        for (name, saga) in &inner.sagas {
            out.push(DefinitionInfo {
                name: name.clone(),
                pattern: ExecutionPattern::Saga,
                steps: saga.step_names().iter().map(ToString::to_string).collect(),
            });
        }
        for (name, workflow) in &inner.workflows {
            out.push(DefinitionInfo {
                name: name.clone(),
                pattern: ExecutionPattern::Workflow,
                steps: workflow
                    .node_names()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }
        for (name, tcc) in &inner.tccs {
            out.push(DefinitionInfo {
                name: name.clone(),
                pattern: ExecutionPattern::Tcc,
                steps: tcc
                    .participant_names()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, Step, TccParticipant};

    fn populated() -> OrchestrationRegistry {
        let registry = OrchestrationRegistry::new();
        registry.register_saga(
            Saga::new("orderSaga")
                .step(Step::new("reserve", || async { Ok(()) }))
                .step(Step::new("charge", || async { Ok(()) })),
        );
        registry.register_workflow(
            Workflow::new("approval")
                .node(Node::new("submit", || async { Ok(()) }))
                .node(Node::new("approve", || async { Ok(()) }).depends_on(["submit"])),
        );
        registry.register_tcc(Tcc::new("transfer").participant(TccParticipant::new(
            "debit",
            || async { Ok(()) },
            || async { Ok(()) },
        )));
        registry
    }

    // Port of pyfly registry tests: definitions are listed by name.
    #[test]
    fn names_are_listed_sorted() {
        let registry = populated();
        registry.register_saga(Saga::new("aSaga"));
        assert_eq!(registry.saga_names(), ["aSaga", "orderSaga"]);
        assert_eq!(registry.workflow_names(), ["approval"]);
        assert_eq!(registry.tcc_names(), ["transfer"]);
    }

    #[test]
    fn lookup_returns_registered_definition() {
        let registry = populated();
        assert_eq!(
            registry.saga("orderSaga").expect("present").step_names(),
            ["reserve", "charge"]
        );
        assert!(registry.saga("missing").is_none());
        assert!(registry.workflow("approval").is_some());
        assert!(registry.tcc("transfer").is_some());
    }

    // Rust-specific: re-registering replaces, mirroring pyfly's
    // last-decorator-wins behavior.
    #[test]
    fn reregistering_replaces_definition() {
        let registry = populated();
        registry.register_saga(Saga::new("orderSaga").step(Step::new("only", || async { Ok(()) })));
        assert_eq!(
            registry.saga("orderSaga").expect("present").step_names(),
            ["only"]
        );
        assert_eq!(registry.saga_names().len(), 1);
    }

    #[test]
    fn definitions_lists_every_pattern() {
        let registry = populated();
        let defs = registry.definitions();
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0].pattern, ExecutionPattern::Saga);
        assert_eq!(defs[0].steps, ["reserve", "charge"]);
        assert_eq!(defs[1].pattern, ExecutionPattern::Workflow);
        assert_eq!(defs[2].pattern, ExecutionPattern::Tcc);
        let json = serde_json::to_value(&defs).expect("serialize");
        assert_eq!(json[0]["pattern"], "SAGA");
        assert_eq!(json[0]["name"], "orderSaga");
    }

    // Registered definitions stay runnable through the shared handle.
    #[tokio::test]
    async fn registered_saga_is_runnable() {
        let registry = populated();
        let saga = registry.saga("orderSaga").expect("present");
        let outcome = saga.run().await.expect("completes");
        assert_eq!(outcome.steps_executed, ["reserve", "charge"]);
    }
}
