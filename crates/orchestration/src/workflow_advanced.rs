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

//! Advanced workflow primitives — child workflows, continue-as-new, and a
//! query service over running executions.
//!
//! The Rust spelling of pyfly's
//! `pyfly.transactional.workflow.{child_workflow_service,continue_as_new_service,query_service}`.
//! pyfly binds these services to a `WorkflowEngine` and starts workflows by
//! id; the Rust port keeps the same shape with a registry of workflow
//! *factories* (an `Arc<dyn Fn() -> Workflow>` per id) so a child run gets a
//! fresh DAG each time, and threads the run's [`StepContext`] explicitly.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::model::{ExecutionPattern, ExecutionState, ExecutionStatus};
use crate::persistence::{PersistenceError, PersistenceProvider};
use crate::step_context::StepContext;
use crate::workflow::{Workflow, WorkflowError};

/// A factory producing a fresh [`Workflow`] instance for a registered id.
/// Each child run / continue-as-new gets a new DAG so state is not shared.
pub type WorkflowFactory = Arc<dyn Fn() -> Workflow + Send + Sync>;

/// Error produced when starting a child workflow — pyfly's
/// `ChildWorkflowService` failures.
#[derive(Debug, thiserror::Error)]
pub enum ChildWorkflowError {
    /// No workflow factory is registered under the requested id.
    #[error("child workflow {0:?} is not registered")]
    Unknown(String),
    /// The child run timed out before completing.
    #[error("child workflow {workflow:?} timed out after {timeout:?}")]
    TimedOut {
        /// The child workflow id.
        workflow: String,
        /// The configured timeout.
        timeout: Duration,
    },
    /// The child run failed.
    #[error("child workflow {workflow:?} failed: {source}")]
    Failed {
        /// The child workflow id.
        workflow: String,
        /// The underlying workflow error.
        #[source]
        source: WorkflowError,
    },
}

/// Outcome of a fire-and-forget child start: the spawned task handle plus the
/// child correlation id, mirroring pyfly returning the child correlation id.
#[derive(Debug)]
pub struct ChildHandle {
    /// The child run's correlation id.
    pub correlation_id: String,
    /// The background task driving the child run.
    pub task: tokio::task::JoinHandle<Result<(), WorkflowError>>,
}

/// Spawns nested workflows from a parent step, and performs
/// continue-as-new restarts — pyfly's `ChildWorkflowService` +
/// `ContinueAsNewService` folded into one registry-backed service.
///
/// ```
/// use firefly_orchestration::{ChildWorkflowService, Node, StepContext, Workflow};
/// use std::sync::Arc;
/// use serde_json::json;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let svc = Arc::new(ChildWorkflowService::new());
/// // Register a child workflow factory by id.
/// svc.register("greet", {
///     || Workflow::new("greet").node(Node::with_context("hello", |ctx| async move {
///         ctx.set_result("hello", json!("hi"));
///         Ok(())
///     }))
/// });
/// // A parent step runs the child synchronously and reads its result.
/// let child_ctx = svc.start("greet", StepContext::new()).await.expect("child ok");
/// assert_eq!(child_ctx.result("hello").unwrap(), json!("hi"));
/// # });
/// ```
#[derive(Default)]
pub struct ChildWorkflowService {
    factories: Mutex<BTreeMap<String, WorkflowFactory>>,
}

impl std::fmt::Debug for ChildWorkflowService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids = self.registered();
        f.debug_struct("ChildWorkflowService")
            .field("registered", &ids)
            .finish()
    }
}

impl ChildWorkflowService {
    /// Creates an empty service with no registered workflows.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a workflow *factory* under `id` — pyfly's engine binding.
    /// The factory is invoked once per child run so each gets a fresh DAG.
    pub fn register<F>(&self, id: impl Into<String>, factory: F)
    where
        F: Fn() -> Workflow + Send + Sync + 'static,
    {
        self.factories
            .lock()
            .expect("lock")
            .insert(id.into(), Arc::new(factory));
    }

    /// The ids of every registered workflow, sorted.
    pub fn registered(&self) -> Vec<String> {
        self.factories
            .lock()
            .expect("lock")
            .keys()
            .cloned()
            .collect()
    }

    fn factory(&self, id: &str) -> Result<WorkflowFactory, ChildWorkflowError> {
        self.factories
            .lock()
            .expect("lock")
            .get(id)
            .cloned()
            .ok_or_else(|| ChildWorkflowError::Unknown(id.to_string()))
    }

    /// Starts the child workflow `id` synchronously, threading `input_ctx`
    /// through it, and returns the child's [`StepContext`] on success so the
    /// parent can read its step results — pyfly's
    /// `ChildWorkflowService.start(wait_for_completion=True)`.
    pub async fn start(
        &self,
        id: &str,
        input_ctx: StepContext,
    ) -> Result<StepContext, ChildWorkflowError> {
        let factory = self.factory(id)?;
        let workflow = factory();
        workflow
            .run_with_context(&input_ctx)
            .await
            .map(|()| input_ctx)
            .map_err(|source| ChildWorkflowError::Failed {
                workflow: id.to_string(),
                source,
            })
    }

    /// Starts the child workflow `id` synchronously bounded by `timeout` —
    /// pyfly's `start(..., timeout_ms=...)`.
    pub async fn start_with_timeout(
        &self,
        id: &str,
        input_ctx: StepContext,
        timeout: Duration,
    ) -> Result<StepContext, ChildWorkflowError> {
        let factory = self.factory(id)?;
        let workflow = factory();
        let run = workflow.run_with_context(&input_ctx);
        match tokio::time::timeout(timeout, run).await {
            Ok(Ok(())) => Ok(input_ctx),
            Ok(Err(source)) => Err(ChildWorkflowError::Failed {
                workflow: id.to_string(),
                source,
            }),
            Err(_) => Err(ChildWorkflowError::TimedOut {
                workflow: id.to_string(),
                timeout,
            }),
        }
    }

    /// Starts the child workflow `id` fire-and-forget, returning a
    /// [`ChildHandle`] with the child correlation id and the spawned task —
    /// pyfly's `start(wait_for_completion=False)` returning the child
    /// correlation id.
    pub fn start_async(
        &self,
        id: &str,
        input_ctx: StepContext,
    ) -> Result<ChildHandle, ChildWorkflowError> {
        let factory = self.factory(id)?;
        let correlation_id = if input_ctx.correlation_id().is_empty() {
            let cid = uuid::Uuid::new_v4().to_string();
            input_ctx.set_correlation_id(cid.clone());
            cid
        } else {
            input_ctx.correlation_id()
        };
        let workflow = factory();
        let ctx = input_ctx;
        let task = tokio::spawn(async move { workflow.run_with_context(&ctx).await });
        Ok(ChildHandle {
            correlation_id,
            task,
        })
    }
}

/// Continue-as-new: restart a registered workflow with new input but a fresh
/// correlation id — pyfly's `ContinueAsNewService.restart`. Backed by the
/// same [`ChildWorkflowService`] registry.
#[derive(Debug)]
pub struct ContinueAsNew {
    service: Arc<ChildWorkflowService>,
}

impl ContinueAsNew {
    /// Wraps a [`ChildWorkflowService`] so its registered workflows can be
    /// restarted with new input.
    pub fn new(service: Arc<ChildWorkflowService>) -> Self {
        Self { service }
    }

    /// Restarts workflow `id` with `input`, returning the fresh
    /// [`StepContext`] (a new correlation id, the new input). Runs the new
    /// instance to completion — pyfly's `restart`.
    pub async fn restart(&self, id: &str, input: Value) -> Result<StepContext, ChildWorkflowError> {
        let ctx = StepContext::with_input(input);
        ctx.set_correlation_id(uuid::Uuid::new_v4().to_string());
        self.service.start(id, ctx).await
    }
}

/// Error produced by [`WorkflowQueryService`].
#[derive(Debug, thiserror::Error)]
pub enum WorkflowQueryError {
    /// No execution is registered under the requested correlation id.
    #[error("workflow execution {0:?} is not active")]
    Unknown(String),
    /// The requested query name is not registered for the execution.
    #[error("execution {correlation_id:?} has no query named {query:?}")]
    UnknownQuery {
        /// The execution correlation id.
        correlation_id: String,
        /// The missing query name.
        query: String,
    },
}

type QueryFn = Arc<dyn Fn(&StepContext) -> Value + Send + Sync>;

struct QueryEntry {
    ctx: StepContext,
    queries: BTreeMap<String, QueryFn>,
}

/// Routes read-side queries to registered query handlers against a live
/// workflow execution — pyfly's `WorkflowQueryService` / `@workflow_query`.
///
/// An execution registers its [`StepContext`] plus named query closures;
/// callers ask for a query by correlation id + name and get the projected
/// state of the running workflow without disturbing it.
///
/// ```
/// use firefly_orchestration::{StepContext, WorkflowQueryService};
/// use serde_json::json;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let svc = WorkflowQueryService::new();
/// let ctx = StepContext::new();
/// ctx.set_variable("progress", json!(50));
/// svc.register("run-1", ctx.clone());
/// svc.register_query("run-1", "progress", |ctx| {
///     ctx.variable("progress").unwrap_or(json!(0))
/// });
/// assert_eq!(svc.query("run-1", "progress").unwrap(), json!(50));
/// # });
/// ```
#[derive(Default)]
pub struct WorkflowQueryService {
    executions: Mutex<BTreeMap<String, QueryEntry>>,
}

impl std::fmt::Debug for WorkflowQueryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowQueryService")
            .field("active", &self.active())
            .finish()
    }
}

impl WorkflowQueryService {
    /// Creates an empty query service.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a running execution by correlation id with its live
    /// [`StepContext`] — pyfly's `register(definition, ctx)`.
    pub fn register(&self, correlation_id: impl Into<String>, ctx: StepContext) {
        self.executions.lock().expect("lock").insert(
            correlation_id.into(),
            QueryEntry {
                ctx,
                queries: BTreeMap::new(),
            },
        );
    }

    /// Registers a named query handler for an already-registered execution —
    /// pyfly's `@workflow_query(name=...)`. No-op when the execution is not
    /// registered.
    pub fn register_query<F>(&self, correlation_id: &str, query: impl Into<String>, handler: F)
    where
        F: Fn(&StepContext) -> Value + Send + Sync + 'static,
    {
        if let Some(entry) = self
            .executions
            .lock()
            .expect("lock")
            .get_mut(correlation_id)
        {
            entry.queries.insert(query.into(), Arc::new(handler));
        }
    }

    /// Unregisters an execution once it terminates — pyfly's `unregister`.
    pub fn unregister(&self, correlation_id: &str) {
        self.executions.lock().expect("lock").remove(correlation_id);
    }

    /// The correlation ids of every active execution, sorted.
    pub fn active(&self) -> Vec<String> {
        self.executions
            .lock()
            .expect("lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Runs query `query_name` against execution `correlation_id`, returning
    /// the projected value — pyfly's `query(correlation_id, query_name)`.
    pub fn query(
        &self,
        correlation_id: &str,
        query_name: &str,
    ) -> Result<Value, WorkflowQueryError> {
        let guard = self.executions.lock().expect("lock");
        let entry = guard
            .get(correlation_id)
            .ok_or_else(|| WorkflowQueryError::Unknown(correlation_id.to_string()))?;
        let handler =
            entry
                .queries
                .get(query_name)
                .ok_or_else(|| WorkflowQueryError::UnknownQuery {
                    correlation_id: correlation_id.to_string(),
                    query: query_name.to_string(),
                })?;
        Ok(handler(&entry.ctx))
    }
}

/// Durable suspend/resume for a workflow execution over a
/// [`PersistenceProvider`](crate::PersistenceProvider) — the low-gap wiring
/// of pyfly's `on_workflow_suspended` / `on_workflow_resumed` durable model.
///
/// A workflow that parks on a long wait can [`suspend`](Self::suspend) its
/// [`StepContext`] snapshot into persisted [`ExecutionState`] (status
/// `SUSPENDED`); a fresh process can later [`resume`](Self::resume) by
/// correlation id, rebuilding the context from the stored payload so the
/// run continues where it left off — surviving a restart that an in-memory
/// `oneshot` waiter would not.
#[derive(Clone)]
pub struct DurableWorkflowState {
    persistence: Arc<dyn PersistenceProvider>,
}

impl std::fmt::Debug for DurableWorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableWorkflowState")
            .finish_non_exhaustive()
    }
}

impl DurableWorkflowState {
    /// Wraps a [`PersistenceProvider`](crate::PersistenceProvider) for
    /// durable suspend/resume.
    pub fn new(persistence: Arc<dyn PersistenceProvider>) -> Self {
        Self { persistence }
    }

    /// Suspends a workflow run: snapshots `ctx` into a persisted
    /// [`ExecutionState`] keyed by `ctx.correlation_id()`, in status
    /// [`ExecutionStatus::Suspended`] — pyfly's `on_workflow_suspended`.
    pub async fn suspend(
        &self,
        name: impl Into<String>,
        ctx: &StepContext,
    ) -> Result<(), PersistenceError> {
        let state = ExecutionState::new(ctx.correlation_id(), name, ExecutionPattern::Workflow)
            .with_status(ExecutionStatus::Suspended)
            .with_payload(ctx.to_snapshot());
        self.persistence.save(state).await
    }

    /// Resumes a suspended workflow run: loads the persisted state by
    /// `correlation_id` and rebuilds its [`StepContext`] from the snapshot —
    /// pyfly's `on_workflow_resumed`. Returns `Ok(None)` when no such
    /// execution is persisted.
    pub async fn resume(
        &self,
        correlation_id: &str,
    ) -> Result<Option<StepContext>, PersistenceError> {
        let Some(state) = self.persistence.load(correlation_id).await? else {
            return Ok(None);
        };
        Ok(Some(StepContext::from_snapshot(&state.payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::MemoryPersistence;
    use crate::workflow::Node;
    use serde_json::json;

    fn greet_factory() -> Workflow {
        Workflow::new("greet").node(Node::with_context("hello", |ctx| async move {
            ctx.set_result("hello", json!("hi"));
            Ok(())
        }))
    }

    // Port of pyfly ChildWorkflowService.start(wait_for_completion=True):
    // the parent gets the child's result back.
    #[tokio::test]
    async fn child_workflow_runs_synchronously() {
        let svc = ChildWorkflowService::new();
        svc.register("greet", greet_factory);
        let child = svc
            .start("greet", StepContext::new())
            .await
            .expect("child ok");
        assert_eq!(child.result("hello").unwrap(), json!("hi"));
    }

    // An unknown child id errors.
    #[tokio::test]
    async fn child_workflow_unknown_errors() {
        let svc = ChildWorkflowService::new();
        let err = svc
            .start("ghost", StepContext::new())
            .await
            .expect_err("unknown");
        assert!(matches!(err, ChildWorkflowError::Unknown(_)));
    }

    // A failing child surfaces a Failed error.
    #[tokio::test]
    async fn child_workflow_failure_propagates() {
        let svc = ChildWorkflowService::new();
        svc.register("boom", || {
            Workflow::new("boom").node(Node::new("x", || async { Err("kaput".into()) }))
        });
        let err = svc
            .start("boom", StepContext::new())
            .await
            .expect_err("fails");
        assert!(matches!(err, ChildWorkflowError::Failed { .. }));
    }

    // Port of pyfly ChildWorkflowService.start(timeout_ms=...): a slow child
    // times out.
    #[tokio::test]
    async fn child_workflow_times_out() {
        let svc = ChildWorkflowService::new();
        svc.register("slow", || {
            Workflow::new("slow").node(Node::new("wait", || async {
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(())
            }))
        });
        let err = svc
            .start_with_timeout("slow", StepContext::new(), Duration::from_millis(20))
            .await
            .expect_err("times out");
        assert!(matches!(err, ChildWorkflowError::TimedOut { .. }));
    }

    // Port of pyfly ChildWorkflowService.start(wait_for_completion=False):
    // returns a child correlation id and runs in the background.
    #[tokio::test]
    async fn child_workflow_fire_and_forget() {
        let svc = ChildWorkflowService::new();
        svc.register("greet", greet_factory);
        let handle = svc
            .start_async("greet", StepContext::new())
            .expect("started");
        assert!(!handle.correlation_id.is_empty());
        handle.task.await.expect("join").expect("child ok");
    }

    // Port of pyfly ContinueAsNewService.restart: a fresh correlation id +
    // new input.
    #[tokio::test]
    async fn continue_as_new_restarts_with_new_input() {
        let svc = Arc::new(ChildWorkflowService::new());
        svc.register("greet", || {
            Workflow::new("greet").node(Node::with_context("echo", |ctx| async move {
                let amount = ctx.input_field("amount").unwrap_or(json!(0));
                ctx.set_result("echo", amount);
                Ok(())
            }))
        });
        let can = ContinueAsNew::new(Arc::clone(&svc));
        let child = can
            .restart("greet", json!({"amount": 7}))
            .await
            .expect("restart ok");
        assert_eq!(child.result("echo").unwrap(), json!(7));
        assert!(!child.correlation_id().is_empty());
    }

    // Port of pyfly WorkflowQueryService: query a running execution's state.
    #[tokio::test]
    async fn query_service_answers_registered_query() {
        let svc = WorkflowQueryService::new();
        let ctx = StepContext::new();
        ctx.set_variable("progress", json!(50));
        ctx.set_result("step-a", json!("done"));
        svc.register("run-1", ctx);
        svc.register_query("run-1", "progress", |ctx| {
            ctx.variable("progress").unwrap_or(json!(0))
        });
        svc.register_query("run-1", "last_step", |ctx| {
            ctx.result("step-a").unwrap_or(json!(null))
        });
        assert_eq!(svc.query("run-1", "progress").unwrap(), json!(50));
        assert_eq!(svc.query("run-1", "last_step").unwrap(), json!("done"));
        assert_eq!(svc.active(), ["run-1"]);
    }

    // Port of pyfly query_service errors: unknown execution / unknown query.
    #[tokio::test]
    async fn query_service_error_cases() {
        let svc = WorkflowQueryService::new();
        let err = svc.query("nope", "x").expect_err("unknown exec");
        assert!(matches!(err, WorkflowQueryError::Unknown(_)));

        svc.register("run-2", StepContext::new());
        let err = svc.query("run-2", "ghost").expect_err("unknown query");
        assert!(matches!(err, WorkflowQueryError::UnknownQuery { .. }));

        svc.unregister("run-2");
        assert!(svc.active().is_empty());
    }

    // Durable suspend/resume across "restart": a context snapshotted by one
    // DurableWorkflowState is reconstituted by a fresh one over the same
    // persistence — pyfly's on_workflow_suspended/resumed durable model.
    #[tokio::test]
    async fn durable_suspend_resume_survives_restart() {
        let persistence: Arc<dyn PersistenceProvider> = Arc::new(MemoryPersistence::new());

        // Process A: a workflow runs partway, then suspends.
        let ctx = StepContext::with_input(json!({"order": "O-7"}));
        ctx.set_correlation_id("run-durable");
        ctx.set_result("reserve", json!({"reservation": "R-3"}));
        let durable_a = DurableWorkflowState::new(Arc::clone(&persistence));
        durable_a.suspend("approval", &ctx).await.expect("suspend");

        // The suspended state is queryable as SUSPENDED.
        let state = persistence
            .load("run-durable")
            .await
            .expect("load")
            .expect("present");
        assert_eq!(state.status, ExecutionStatus::Suspended);

        // Process B (fresh DurableWorkflowState): resume by id and observe the
        // restored facts.
        let durable_b = DurableWorkflowState::new(Arc::clone(&persistence));
        let resumed = durable_b
            .resume("run-durable")
            .await
            .expect("resume")
            .expect("found");
        assert_eq!(resumed.input(), json!({"order": "O-7"}));
        assert_eq!(
            resumed.result_field("reserve", "reservation").unwrap(),
            json!("R-3")
        );
    }

    // Resuming an unknown execution yields None.
    #[tokio::test]
    async fn durable_resume_unknown_is_none() {
        let persistence: Arc<dyn PersistenceProvider> = Arc::new(MemoryPersistence::new());
        let durable = DurableWorkflowState::new(persistence);
        assert!(durable.resume("ghost").await.expect("resume").is_none());
    }
}
