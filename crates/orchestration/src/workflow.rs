//! Workflow engine: a DAG of nodes executed in topological waves.

use crate::{boxed_action, ActionFn, BoxError, CancellationToken};
use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use thiserror::Error;

/// A single workflow vertex. Its dependencies list the names of nodes that
/// must complete before this one runs.
pub struct Node {
    name: String,
    depends_on: Vec<String>,
    run: ActionFn,
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
            run: boxed_action(run),
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

    /// The node name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared dependency names.
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("name", &self.name)
            .field("depends_on", &self.depends_on)
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
pub struct Workflow {
    name: String,
    nodes: Vec<Node>,
}

impl Workflow {
    /// Creates an empty workflow.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
        }
    }

    /// Appends a node to the DAG.
    pub fn node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }

    /// The workflow name, as reported in error messages.
    pub fn name(&self) -> &str {
        &self.name
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
        let mut pending: Vec<&Node> = self.nodes.iter().collect();

        while !pending.is_empty() {
            let (ready, still_pending): (Vec<&Node>, Vec<&Node>) = pending
                .into_iter()
                .partition(|node| node.depends_on.iter().all(|dep| done.contains(dep)));
            if ready.is_empty() {
                return Err(WorkflowError::NoProgress {
                    workflow: self.name.clone(),
                });
            }

            let wave = ready.iter().map(|node| {
                let internal = internal.clone();
                async move {
                    if token.is_cancelled() || internal.is_cancelled() {
                        return Err(WorkflowError::Cancelled);
                    }
                    match (node.run)().await {
                        Ok(()) => Ok(node.name.clone()),
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
                return Err(WorkflowError::join(errors));
            }
            pending = still_pending;
        }
        Ok(())
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
}
