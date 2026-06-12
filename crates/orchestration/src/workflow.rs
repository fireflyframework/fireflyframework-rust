//! Workflow engine: a DAG of nodes executed in topological waves.
//!
//! # Step compensation (pyfly parity)
//!
//! Beyond the original fail-fast model, a [`Node`] may declare a
//! [`Node::with_compensation`] hook. On any node failure the workflow rolls
//! back the *already-completed compensatable* nodes in reverse completion
//! order before surfacing the original error — mirroring pyfly's
//! `WorkflowExecutor._compensate` + `@compensation_step`. This reuses the
//! [`Saga`](crate::Saga) reverse-order [`CompensationPolicy`](crate::CompensationPolicy)
//! shape.

use crate::condition::evaluate as evaluate_condition;
use crate::saga::CompensationPolicy;
use crate::step_context::StepContext;
use crate::{boxed_action, ActionFn, BoxError, CancellationToken};
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use thiserror::Error;

/// A context-aware node action: it receives the run's [`StepContext`] so it
/// can read prior step results and publish its own.
pub(crate) type CtxActionFn = Box<
    dyn Fn(StepContext) -> futures::future::BoxFuture<'static, Result<(), BoxError>> + Send + Sync,
>;

/// The body a node runs — either a legacy zero-arg action (the original API)
/// or a context-aware one.
enum NodeBody {
    /// Original zero-arg closure.
    Plain(ActionFn),
    /// Context-aware closure (inter-step data passing).
    WithContext(CtxActionFn),
}

/// A single workflow vertex. Its dependencies list the names of nodes that
/// must complete before this one runs.
///
/// A node may additionally declare a compensation hook
/// ([`Node::with_compensation`]), a skip condition ([`Node::when`]), and
/// fire-and-forget semantics ([`Node::fire_and_forget`]) — pyfly's
/// `@workflow_step` compensation / `condition=` / `async_=True` options.
pub struct Node {
    name: String,
    depends_on: Vec<String>,
    body: NodeBody,
    compensate: Option<CtxActionFn>,
    /// SpEL-substitute condition expression — the node is skipped when it
    /// evaluates to false (pyfly's `@workflow_step(condition=...)`).
    condition: Option<String>,
    /// Fire-and-forget: the node is scheduled and the workflow proceeds
    /// without awaiting it (pyfly's `@workflow_step(async_=True)`).
    fire_and_forget: bool,
}

impl Node {
    /// Creates a node from a name and an async run action.
    pub fn new<F, Fut>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            depends_on: Vec::new(),
            body: NodeBody::Plain(boxed_action(run)),
            compensate: None,
            condition: None,
            fire_and_forget: false,
        }
    }

    /// Creates a node whose action receives the run's [`StepContext`], so it
    /// can read prior step results and publish its own — the engine spelling
    /// of pyfly's `Annotated[..., FromStep/Input/Variable]` argument
    /// injection. Inter-step data passing.
    ///
    /// ```
    /// use firefly_orchestration::{Node, StepContext, Workflow};
    /// use serde_json::json;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let workflow = Workflow::new("pipeline")
    ///     .node(Node::with_context("reserve", |ctx| async move {
    ///         ctx.set_result("reserve", json!({"id": "R-1"}));
    ///         Ok(())
    ///     }))
    ///     .node(
    ///         Node::with_context("charge", |ctx| async move {
    ///             // Consume the prior step's output.
    ///             assert_eq!(ctx.result_field("reserve", "id").unwrap(), json!("R-1"));
    ///             Ok(())
    ///         })
    ///         .depends_on(["reserve"]),
    ///     );
    /// workflow.run().await.expect("ok");
    /// # });
    /// ```
    pub fn with_context<F, Fut>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            depends_on: Vec::new(),
            body: NodeBody::WithContext(Box::new(move |ctx| Box::pin(run(ctx)))),
            compensate: None,
            condition: None,
            fire_and_forget: false,
        }
    }

    /// Declares the names of nodes that must complete before this one runs.
    pub fn depends_on<I>(mut self, dependencies: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        self.depends_on
            .extend(dependencies.into_iter().map(Into::into));
        self
    }

    /// Attaches a context-aware compensation hook that rolls back this
    /// node's side effects — the engine spelling of pyfly's
    /// `@workflow_step(compensatable=True, compensation_method=...)` plus
    /// `@compensation_step`. On any workflow failure, completed compensatable
    /// nodes are rolled back in reverse completion order before the original
    /// error propagates.
    pub fn with_compensation<F, Fut>(mut self, compensate: F) -> Self
    where
        F: Fn(StepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
    {
        self.compensate = Some(Box::new(move |ctx| Box::pin(compensate(ctx))));
        self
    }

    /// Sets a skip condition expression evaluated against the run's facts
    /// (`results`, `variables`, `headers`, `input`). When it resolves to
    /// false the node is skipped entirely — pyfly's
    /// `@workflow_step(condition=...)`. A malformed condition fails closed
    /// (the node is skipped).
    ///
    /// See [`crate::condition`] for the supported expression grammar.
    pub fn when(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }

    /// Marks this node fire-and-forget: the workflow schedules it and
    /// proceeds without awaiting it — pyfly's
    /// `@workflow_step(async_=True)`. A fire-and-forget node never fails the
    /// workflow and is never compensated.
    pub fn fire_and_forget(mut self) -> Self {
        self.fire_and_forget = true;
        self
    }

    /// The node name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared dependency names.
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }

    /// `true` when this node has a compensation hook.
    pub fn is_compensatable(&self) -> bool {
        self.compensate.is_some()
    }

    /// `true` when this node is fire-and-forget.
    pub fn is_fire_and_forget(&self) -> bool {
        self.fire_and_forget
    }

    /// Runs the node body with the supplied context.
    async fn run(&self, ctx: &StepContext) -> Result<(), BoxError> {
        self.run_owned(ctx.clone()).await
    }

    /// Produces a `'static` future for the node body — used for both blocking
    /// execution and fire-and-forget scheduling. The action callbacks
    /// themselves return owned `'static` futures, so the returned future does
    /// not borrow `self`.
    fn run_owned(
        &self,
        ctx: StepContext,
    ) -> futures::future::BoxFuture<'static, Result<(), BoxError>> {
        match &self.body {
            NodeBody::Plain(action) => action(),
            NodeBody::WithContext(action) => action(ctx),
        }
    }

    /// Evaluates this node's skip condition (if any) against the run facts.
    /// Returns `true` (run the node) when there is no condition or it holds;
    /// `false` (skip) when the condition resolves to false or is malformed
    /// (fail-closed) — pyfly's `_evaluate_condition`.
    fn condition_holds(&self, ctx: &StepContext) -> bool {
        match &self.condition {
            None => true,
            Some(expr) => evaluate_condition(expr, ctx).unwrap_or(false),
        }
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("name", &self.name)
            .field("depends_on", &self.depends_on)
            .field("compensatable", &self.compensate.is_some())
            .field("condition", &self.condition)
            .field("fire_and_forget", &self.fire_and_forget)
            .finish_non_exhaustive()
    }
}

/// Renders joined errors the way Go's `errors.Join` does: one message per
/// line.
fn joined(errors: &[WorkflowError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Errors produced by [`Workflow::run`].
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// Two nodes share the same name.
    #[error("workflow {workflow:?}: duplicate node {node:?}")]
    DuplicateNode {
        /// The workflow name.
        workflow: String,
        /// The duplicated node name.
        node: String,
    },
    /// A node depends on a name that is not declared in the workflow.
    #[error("workflow {workflow:?}: node {node:?} depends on unknown {dependency:?}")]
    UnknownDependency {
        /// The workflow name.
        workflow: String,
        /// The node declaring the dependency.
        node: String,
        /// The unknown dependency name.
        dependency: String,
    },
    /// No node became ready while some were still pending — a dependency
    /// cycle.
    #[error("workflow {workflow:?}: no progress (dependency cycle?)")]
    NoProgress {
        /// The workflow name.
        workflow: String,
    },
    /// A node's run action failed.
    #[error("node {node:?}: {source}")]
    Node {
        /// The failing node name.
        node: String,
        /// The error returned by the node's run action.
        #[source]
        source: BoxError,
    },
    /// The run observed a cancelled [`CancellationToken`].
    #[error("workflow cancelled")]
    Cancelled,
    /// Several nodes of the same wave failed; messages are joined one per
    /// line, mirroring Go's `errors.Join`.
    #[error("{}", joined(.0))]
    Multiple(Vec<WorkflowError>),
}

impl WorkflowError {
    /// Folds a non-empty error list into a single error: the sole element,
    /// or [`WorkflowError::Multiple`] — the analogue of Go's `errors.Join`.
    fn join(mut errors: Vec<WorkflowError>) -> WorkflowError {
        if errors.len() == 1 {
            errors.pop().expect("len checked")
        } else {
            WorkflowError::Multiple(errors)
        }
    }
}

/// Runs a DAG of [`Node`]s. Independent nodes execute concurrently within a
/// wave; a failure short-circuits remaining waves and returns the
/// aggregated errors.
///
/// When any node declares a compensation hook ([`Node::with_compensation`]),
/// a failure additionally rolls back the already-completed compensatable
/// nodes in reverse completion order under the configured
/// [`CompensationPolicy`](crate::CompensationPolicy) before the error
/// surfaces — pyfly's `WorkflowExecutor._compensate`.
pub struct Workflow {
    name: String,
    nodes: Vec<Node>,
    policy: CompensationPolicy,
}

impl Workflow {
    /// Creates an empty workflow with the default
    /// [`CompensationPolicy::BestEffort`].
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
            policy: CompensationPolicy::default(),
        }
    }

    /// Appends a node to the DAG.
    pub fn node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }

    /// Sets the compensation policy applied during rollback — same shape as
    /// [`Saga::policy`](crate::Saga::policy).
    pub fn policy(mut self, policy: CompensationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The workflow name, as reported in error messages.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured nodes, in insertion order — the definition-listing
    /// accessor used by the validator, registry, and admin surfaces.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Node names in insertion order.
    pub fn node_names(&self) -> Vec<&str> {
        self.nodes.iter().map(|n| n.name.as_str()).collect()
    }

    /// The dependency graph as `node -> dependencies`, the shape consumed
    /// by [`OrchestrationValidator::validate_dag`](crate::OrchestrationValidator::validate_dag).
    pub fn graph(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.nodes
            .iter()
            .map(|n| (n.name.clone(), n.depends_on.clone()))
            .collect()
    }

    /// Executes the workflow. Returns the first node error encountered
    /// (joined with the other failures of the same wave, if any).
    pub async fn run(&self) -> Result<(), WorkflowError> {
        self.run_cancellable(&CancellationToken::new()).await
    }

    /// Executes the workflow, checking `token` before each node starts. A
    /// cancelled token short-circuits the run with
    /// [`WorkflowError::Cancelled`].
    pub async fn run_cancellable(&self, token: &CancellationToken) -> Result<(), WorkflowError> {
        self.run_inner(token, &StepContext::new()).await
    }

    /// Executes the workflow threading `ctx` through every context-aware
    /// node ([`Node::with_context`]) and into compensation hooks. Use this
    /// to seed input ([`StepContext::with_input`]) or to inspect step
    /// results afterwards — pyfly's `WorkflowExecutor.execute(definition, ctx)`.
    pub async fn run_with_context(&self, ctx: &StepContext) -> Result<(), WorkflowError> {
        self.run_inner(&CancellationToken::new(), ctx).await
    }

    /// Executes the workflow with both an explicit cancellation token and a
    /// shared [`StepContext`].
    pub async fn run_with_context_cancellable(
        &self,
        token: &CancellationToken,
        ctx: &StepContext,
    ) -> Result<(), WorkflowError> {
        self.run_inner(token, ctx).await
    }

    async fn run_inner(
        &self,
        token: &CancellationToken,
        ctx: &StepContext,
    ) -> Result<(), WorkflowError> {
        let mut names: HashSet<&str> = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !names.insert(node.name.as_str()) {
                return Err(WorkflowError::DuplicateNode {
                    workflow: self.name.clone(),
                    node: node.name.clone(),
                });
            }
        }
        for node in &self.nodes {
            for dependency in &node.depends_on {
                if !names.contains(dependency.as_str()) {
                    return Err(WorkflowError::UnknownDependency {
                        workflow: self.name.clone(),
                        node: node.name.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        // Internal token cancelled by the first node failure so that
        // not-yet-started siblings short-circuit, mirroring the Go port's
        // derived `runCtx`.
        let internal = CancellationToken::new();
        let mut done: HashSet<String> = HashSet::with_capacity(self.nodes.len());
        // Completed compensatable node names, in completion order, used to
        // roll back newest-first on failure (pyfly's _compensate ordering).
        let completed_order = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        // Strong handles to fire-and-forget tasks so they are not dropped
        // mid-flight, mirroring pyfly's `_async_step_tasks` set.
        let mut async_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        let mut pending: Vec<&Node> = self.nodes.iter().collect();

        while !pending.is_empty() {
            let (ready, still_pending): (Vec<&Node>, Vec<&Node>) = pending
                .into_iter()
                .partition(|node| node.depends_on.iter().all(|dep| done.contains(dep)));
            if ready.is_empty() {
                self.compensate(ctx, &completed_order).await;
                return Err(WorkflowError::NoProgress {
                    workflow: self.name.clone(),
                });
            }

            // Fire-and-forget nodes are scheduled and treated as done so the
            // wave proceeds without awaiting them (pyfly async_=True).
            let mut blocking = Vec::with_capacity(ready.len());
            for node in &ready {
                if node.fire_and_forget {
                    if !node.condition_holds(ctx) {
                        done.insert(node.name.clone());
                        continue;
                    }
                    let ctx = ctx.clone();
                    // SAFETY: the node body outlives the task because the
                    // workflow awaits all spawned tasks before returning.
                    let fut = node.run_owned(ctx);
                    async_tasks.push(tokio::spawn(async move {
                        let _ = fut.await;
                    }));
                    done.insert(node.name.clone());
                } else {
                    blocking.push(*node);
                }
            }

            let wave = blocking.iter().map(|node| {
                let internal = internal.clone();
                let completed_order = Arc::clone(&completed_order);
                async move {
                    if token.is_cancelled() || internal.is_cancelled() {
                        return Err(WorkflowError::Cancelled);
                    }
                    // Skip-when-condition-false (pyfly condition=...).
                    if !node.condition_holds(ctx) {
                        return Ok(node.name.clone());
                    }
                    match node.run(ctx).await {
                        Ok(()) => {
                            if node.is_compensatable() {
                                completed_order
                                    .lock()
                                    .expect("lock")
                                    .push(node.name.clone());
                            }
                            Ok(node.name.clone())
                        }
                        Err(source) => {
                            internal.cancel();
                            Err(WorkflowError::Node {
                                node: node.name.clone(),
                                source,
                            })
                        }
                    }
                }
            });

            let mut errors = Vec::new();
            for result in futures::future::join_all(wave).await {
                match result {
                    Ok(name) => {
                        done.insert(name);
                    }
                    Err(err) => errors.push(err),
                }
            }
            if !errors.is_empty() {
                // Roll back completed compensatable nodes before surfacing.
                self.compensate(ctx, &completed_order).await;
                Self::join_async(async_tasks).await;
                return Err(WorkflowError::join(errors));
            }
            pending = still_pending;
        }
        Self::join_async(async_tasks).await;
        Ok(())
    }

    /// Awaits every fire-and-forget task to completion so they run to the
    /// end (pyfly keeps strong references; the Rust port joins them at the
    /// end of the run rather than detaching).
    async fn join_async(tasks: Vec<tokio::task::JoinHandle<()>>) {
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Rolls back completed compensatable nodes in reverse completion order,
    /// honoring the configured [`CompensationPolicy`](crate::CompensationPolicy) —
    /// pyfly's `WorkflowExecutor._compensate`. Compensation errors never mask
    /// the original failure: under
    /// [`CompensationPolicy::BestEffort`](crate::CompensationPolicy::BestEffort)
    /// rollback continues; under
    /// [`CompensationPolicy::StopOnError`](crate::CompensationPolicy::StopOnError)
    /// it aborts at the first failure.
    async fn compensate(
        &self,
        ctx: &StepContext,
        completed_order: &Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let order: Vec<String> = completed_order.lock().expect("lock").clone();
        for name in order.iter().rev() {
            if let Some(node) = self.nodes.iter().find(|n| &n.name == name) {
                if let Some(compensate) = &node.compensate {
                    if compensate(ctx.clone()).await.is_err()
                        && self.policy == CompensationPolicy::StopOnError
                    {
                        return;
                    }
                }
            }
        }
    }
}

impl fmt::Debug for Workflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workflow")
            .field("name", &self.name)
            .field("nodes", &self.nodes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn counting_node(name: &str, counter: &Arc<AtomicU32>, deps: &[&str]) -> Node {
        let counter = counter.clone();
        Node::new(name, move || {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })
        .depends_on(deps.iter().copied())
    }

    // Port of Go TestWorkflowParallel.
    #[tokio::test]
    async fn workflow_runs_all_nodes() {
        let counter = Arc::new(AtomicU32::new(0));
        let workflow = Workflow::new("w")
            .node(counting_node("a", &counter, &[]))
            .node(counting_node("b", &counter, &[]))
            .node(counting_node("c", &counter, &["a", "b"]));
        workflow.run().await.expect("workflow should succeed");
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    // Port of Go TestWorkflowFailsFast.
    #[tokio::test]
    async fn workflow_fails_fast_skips_downstream() {
        let ran: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let mk = |name: &str, fail: bool, deps: &[&str]| {
            let ran = ran.clone();
            let entry = name.to_string();
            Node::new(name, move || {
                let ran = ran.clone();
                let entry = entry.clone();
                async move {
                    ran.lock().unwrap().insert(entry);
                    if fail {
                        Err("boom".into())
                    } else {
                        Ok(())
                    }
                }
            })
            .depends_on(deps.iter().copied())
        };
        let workflow = Workflow::new("w")
            .node(mk("a", true, &[]))
            .node(mk("b", false, &["a"]));

        let err = workflow.run().await.expect_err("expected error");
        // Error message matches the Go port's `node %q: %w` wrapping.
        assert_eq!(err.to_string(), "node \"a\": boom");
        assert!(
            !ran.lock().unwrap().contains("b"),
            "downstream node should not run after upstream failure"
        );
    }

    // Rust-specific: duplicate node names are rejected up-front.
    #[tokio::test]
    async fn workflow_rejects_duplicate_nodes() {
        let workflow = Workflow::new("w")
            .node(Node::new("a", || async { Ok(()) }))
            .node(Node::new("a", || async { Ok(()) }));
        let err = workflow.run().await.expect_err("expected error");
        assert!(matches!(err, WorkflowError::DuplicateNode { .. }));
        assert_eq!(err.to_string(), "workflow \"w\": duplicate node \"a\"");
    }

    // Rust-specific: unknown dependencies are rejected up-front.
    #[tokio::test]
    async fn workflow_rejects_unknown_dependency() {
        let workflow =
            Workflow::new("w").node(Node::new("a", || async { Ok(()) }).depends_on(["ghost"]));
        let err = workflow.run().await.expect_err("expected error");
        assert!(matches!(err, WorkflowError::UnknownDependency { .. }));
        assert_eq!(
            err.to_string(),
            "workflow \"w\": node \"a\" depends on unknown \"ghost\""
        );
    }

    // Rust-specific: a dependency cycle aborts with "no progress".
    #[tokio::test]
    async fn workflow_detects_dependency_cycle() {
        let workflow = Workflow::new("w")
            .node(Node::new("a", || async { Ok(()) }).depends_on(["b"]))
            .node(Node::new("b", || async { Ok(()) }).depends_on(["a"]));
        let err = workflow.run().await.expect_err("expected error");
        assert!(matches!(err, WorkflowError::NoProgress { .. }));
        assert_eq!(
            err.to_string(),
            "workflow \"w\": no progress (dependency cycle?)"
        );
    }

    // Rust-specific: nodes of the same wave really run concurrently. Two
    // nodes rendezvous on a barrier — sequential execution would deadlock,
    // so the run is guarded by a timeout instead of hanging.
    #[tokio::test]
    async fn workflow_wave_nodes_run_concurrently() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mk = |name: &str| {
            let barrier = barrier.clone();
            Node::new(name, move || {
                let barrier = barrier.clone();
                async move {
                    barrier.wait().await;
                    Ok(())
                }
            })
        };
        let workflow = Workflow::new("concurrent").node(mk("a")).node(mk("b"));
        tokio::time::timeout(Duration::from_millis(200), workflow.run())
            .await
            .expect("nodes in the same wave must run concurrently")
            .expect("workflow should succeed");
    }

    // Rust-specific: a diamond DAG respects topological order.
    #[tokio::test]
    async fn workflow_diamond_respects_topological_order() {
        let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mk = |name: &str, deps: &[&str]| {
            let order = order.clone();
            let entry = name.to_string();
            Node::new(name, move || {
                let order = order.clone();
                let entry = entry.clone();
                async move {
                    order.lock().unwrap().push(entry);
                    Ok(())
                }
            })
            .depends_on(deps.iter().copied())
        };
        let workflow = Workflow::new("diamond")
            .node(mk("d", &["b", "c"]))
            .node(mk("b", &["a"]))
            .node(mk("c", &["a"]))
            .node(mk("a", &[]));
        workflow.run().await.expect("workflow should succeed");

        let order = order.lock().unwrap();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], "a");
        assert_eq!(order[3], "d");
    }

    // Rust-specific: empty workflows are a no-op.
    #[tokio::test]
    async fn workflow_with_no_nodes_is_ok() {
        Workflow::new("empty").run().await.expect("empty is ok");
    }

    // Rust-specific: a cancelled token short-circuits before any node runs.
    #[tokio::test]
    async fn workflow_cancellation_short_circuits() {
        let token = CancellationToken::new();
        token.cancel();
        let ran = Arc::new(AtomicU32::new(0));
        let workflow = Workflow::new("w").node(counting_node("a", &ran, &[]));
        let err = workflow
            .run_cancellable(&token)
            .await
            .expect_err("expected cancellation");
        assert!(matches!(err, WorkflowError::Cancelled));
        assert_eq!(ran.load(Ordering::SeqCst), 0);
    }

    // Rust-specific: wave failures join one message per line, like Go's
    // errors.Join.
    #[tokio::test]
    async fn workflow_joins_multiple_wave_failures() {
        let workflow = Workflow::new("w")
            .node(Node::new("a", || async { Err("boom-a".into()) }))
            .node(Node::new("b", || async { Err("boom-b".into()) }));
        let err = workflow.run().await.expect_err("expected error");
        match &err {
            WorkflowError::Multiple(errors) => assert_eq!(errors.len(), 2),
            WorkflowError::Node { .. } | WorkflowError::Cancelled => {
                // Acceptable race: the internal token may stop the sibling
                // before it starts, exactly as in the Go port.
            }
            other => panic!("unexpected error: {other}"),
        }
        assert!(err.to_string().contains("node \"a\": boom-a"));
    }

    // ── Workflow step compensation (pyfly test_compensation.py) ──────────

    type Log = Arc<Mutex<Vec<String>>>;

    fn comp_node(name: &str, fail: bool, deps: &[&str], log: &Log) -> Node {
        let run_entry = name.to_string();
        let comp_entry = name.to_string();
        let run_log = log.clone();
        let comp_log = log.clone();
        Node::with_context(name, move |_ctx| {
            let log = run_log.clone();
            let entry = run_entry.clone();
            async move {
                log.lock().unwrap().push(entry);
                if fail {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            }
        })
        .depends_on(deps.iter().copied())
        .with_compensation(move |_ctx| {
            let log = comp_log.clone();
            let entry = format!("undo_{comp_entry}");
            async move {
                log.lock().unwrap().push(entry);
                Ok(())
            }
        })
    }

    // Port of pyfly test_completed_compensatable_step_is_rolled_back_on_later_failure.
    #[tokio::test]
    async fn completed_compensatable_step_rolled_back_on_later_failure() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::new("comp-basic")
            .node(comp_node("reserve", false, &[], &log))
            .node(
                Node::new("charge", || async { Err("payment declined".into()) })
                    .depends_on(["reserve"]),
            );
        let err = workflow.run().await.expect_err("must fail");
        assert!(matches!(err, WorkflowError::Node { .. }));
        // reserve ran, charge failed, reserve's compensation ran.
        assert_eq!(*log.lock().unwrap(), ["reserve", "undo_reserve"]);
    }

    // Port of pyfly test_multiple_compensations_run_in_reverse_order.
    #[tokio::test]
    async fn multiple_compensations_run_in_reverse_order() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::new("comp-order")
            .node(comp_node("a", false, &[], &log))
            .node(comp_node("b", false, &["a"], &log))
            .node(Node::new("c", || async { Err("boom".into()) }).depends_on(["b"]));
        let err = workflow.run().await.expect_err("must fail");
        assert!(matches!(err, WorkflowError::Node { .. }));
        let order = log.lock().unwrap();
        // a, b ran; then compensation newest-first: undo_b, undo_a.
        assert_eq!(*order, ["a", "b", "undo_b", "undo_a"]);
    }

    // Port of pyfly test_non_compensatable_step_is_not_compensated.
    #[tokio::test]
    async fn non_compensatable_step_is_not_compensated() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let plain_log = log.clone();
        let workflow = Workflow::new("comp-skip")
            .node(Node::with_context("plain", move |_ctx| {
                let log = plain_log.clone();
                async move {
                    log.lock().unwrap().push("plain".to_string());
                    Ok(())
                }
            }))
            .node(Node::new("fail", || async { Err("boom".into()) }).depends_on(["plain"]));
        let err = workflow.run().await.expect_err("must fail");
        assert!(matches!(err, WorkflowError::Node { .. }));
        // 'plain' is not compensatable -> no rollback recorded.
        assert_eq!(*log.lock().unwrap(), ["plain"]);
    }

    // Port of pyfly test_compensation_receives_triggering_error_and_step_result:
    // compensation can read the prior step's result from the context.
    #[tokio::test]
    async fn compensation_reads_prior_step_result() {
        use serde_json::json;
        let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
        let cap = captured.clone();
        let workflow = Workflow::new("comp-args")
            .node(
                Node::with_context("reserve", |ctx| async move {
                    ctx.set_result("reserve", json!({"reservation_id": "R-1"}));
                    Ok(())
                })
                .with_compensation(move |ctx| {
                    let cap = cap.clone();
                    async move {
                        *cap.lock().unwrap() = ctx.result("reserve");
                        Ok(())
                    }
                }),
            )
            .node(Node::new("charge", || async { Err("declined".into()) }).depends_on(["reserve"]));
        workflow.run().await.expect_err("must fail");
        assert_eq!(
            captured.lock().unwrap().clone().unwrap(),
            json!({"reservation_id": "R-1"})
        );
    }

    // Port of pyfly test_no_compensation_when_workflow_succeeds.
    #[tokio::test]
    async fn no_compensation_on_success() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow::new("comp-happy")
            .node(comp_node("reserve", false, &[], &log))
            .node(comp_node("charge", false, &["reserve"], &log));
        workflow.run().await.expect("must complete");
        // Only the run actions, never the compensations.
        assert_eq!(*log.lock().unwrap(), ["reserve", "charge"]);
    }

    // StopOnError policy aborts the rollback at the first compensation
    // failure, mirroring the saga policy shape reused here.
    #[tokio::test]
    async fn workflow_compensation_stop_on_error() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let a_log = log.clone();
        let a = Node::with_context("a", |_ctx| async { Ok(()) }).with_compensation(move |_ctx| {
            let log = a_log.clone();
            async move {
                log.lock().unwrap().push("undo_a".to_string());
                Ok(())
            }
        });
        // b's compensation fails; under StopOnError, a's never runs.
        let b = Node::with_context("b", |_ctx| async { Ok(()) })
            .depends_on(["a"])
            .with_compensation(|_ctx| async { Err("compensate-fail".into()) });
        let c = Node::new("c", || async { Err("trigger".into()) }).depends_on(["b"]);
        let workflow = Workflow::new("policy")
            .policy(CompensationPolicy::StopOnError)
            .node(a)
            .node(b)
            .node(c);
        workflow.run().await.expect_err("must fail");
        // Rollback aborted at b's failure → a's compensation did not run.
        assert!(log.lock().unwrap().is_empty());
    }

    // ── Conditional steps (pyfly test_step_condition_false_skips_step) ───

    #[tokio::test]
    async fn conditional_step_skipped_when_predicate_false() {
        use serde_json::json;
        let ran: Log = Arc::new(Mutex::new(Vec::new()));
        let always_log = ran.clone();
        let maybe_log = ran.clone();
        let workflow = Workflow::new("condwf")
            .node(Node::with_context("always", move |ctx| {
                let log = always_log.clone();
                async move {
                    log.lock().unwrap().push("always".to_string());
                    ctx.set_result("always", json!("ran"));
                    Ok(())
                }
            }))
            .node(
                Node::with_context("maybe", move |_ctx| {
                    let log = maybe_log.clone();
                    async move {
                        log.lock().unwrap().push("maybe".to_string());
                        Ok(())
                    }
                })
                .depends_on(["always"])
                .when("results['always'] == 'nope'"),
            );
        workflow.run().await.expect("completes");
        // 'maybe' was skipped by its condition.
        assert_eq!(*ran.lock().unwrap(), ["always"]);
    }

    #[tokio::test]
    async fn conditional_step_runs_when_predicate_true() {
        use serde_json::json;
        let ran: Log = Arc::new(Mutex::new(Vec::new()));
        let maybe_log = ran.clone();
        let workflow = Workflow::new("condwf2")
            .node(Node::with_context("always", |ctx| async move {
                ctx.set_result("always", json!("ran"));
                Ok(())
            }))
            .node(
                Node::with_context("maybe", move |_ctx| {
                    let log = maybe_log.clone();
                    async move {
                        log.lock().unwrap().push("maybe".to_string());
                        Ok(())
                    }
                })
                .depends_on(["always"])
                .when("results['always'] == 'ran'"),
            );
        workflow.run().await.expect("completes");
        assert_eq!(*ran.lock().unwrap(), ["maybe"]);
    }

    // ── Async fire-and-forget steps (pyfly test_async_step_is_fire_and_forget) ──

    #[tokio::test]
    async fn async_step_is_fire_and_forget() {
        let done = Arc::new(tokio::sync::Notify::new());
        let notify = done.clone();
        let workflow = Workflow::new("asyncwf").node(
            Node::new("bg", move || {
                let notify = notify.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    notify.notify_one();
                    Ok(())
                }
            })
            .fire_and_forget(),
        );
        // The workflow completes; the bg task runs to completion in the
        // background (the engine joins fire-and-forget tasks before return).
        tokio::time::timeout(Duration::from_millis(500), workflow.run())
            .await
            .expect("must finish")
            .expect("completes");
    }

    // A fire-and-forget step that errors never fails the workflow.
    #[tokio::test]
    async fn async_step_error_does_not_fail_workflow() {
        let workflow = Workflow::new("asyncwf2")
            .node(Node::new("bg", || async { Err("ignored".into()) }).fire_and_forget())
            .node(Node::new("main", || async { Ok(()) }));
        workflow.run().await.expect("completes despite bg error");
    }

    // ── Inter-step data passing (pyfly FromStep argument injection) ──────

    #[tokio::test]
    async fn workflow_threads_step_results_between_nodes() {
        use serde_json::json;
        let ctx = StepContext::with_input(json!({"order": "O-1"}));
        let workflow = Workflow::new("pipeline")
            .node(Node::with_context("reserve", |ctx| async move {
                let order = ctx.input_field("order").unwrap();
                ctx.set_result("reserve", json!({"order": order, "reservation": "R-9"}));
                Ok(())
            }))
            .node(
                Node::with_context("charge", |ctx| async move {
                    // Consume the prior step's output.
                    let reservation = ctx.result_field("reserve", "reservation").unwrap();
                    ctx.set_result("charge", json!({"charged_for": reservation}));
                    Ok(())
                })
                .depends_on(["reserve"]),
            );
        workflow.run_with_context(&ctx).await.expect("completes");
        assert_eq!(
            ctx.result_field("charge", "charged_for").unwrap(),
            json!("R-9")
        );
    }
}
