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

//! Distributed-transaction engines: Saga, Workflow (DAG), and TCC.
//!
//! `firefly-orchestration` ships the three classic distributed-transaction
//! engines every Firefly platform agrees on:
//!
//! | Engine       | Topology                   | Compensation                       |
//! |--------------|----------------------------|------------------------------------|
//! | [`Saga`]     | Sequential steps           | Reverse-order, configurable policy |
//! | [`Workflow`] | DAG with parallel branches | Reverse-order, configurable policy |
//! | [`Tcc`]      | Try-all then Confirm-all   | Cancel-tried-on-Try-failure        |
//!
//! Each engine accepts a typed step / node / participant built from async
//! closures, runs as a plain future on the caller's task, and respects
//! cooperative cancellation through a [`CancellationToken`] — the Rust
//! analogue of the Go port's `context.Context` cancellation.
//!
//! # pyfly parity — advanced orchestration
//!
//! On top of the in-process engines the crate ports pyfly's
//! `pyfly.transactional.workflow` advanced layer:
//!
//! * **Per-step retry** — [`invoke_with_policy`] applies a
//!   [`RetryPolicy`] (max attempts, exponential backoff, jitter, per-attempt
//!   timeout) to every step; opt in with [`Step::with_retry`] /
//!   [`TccParticipant::with_retry`] — pyfly's `StepInvoker`.
//! * **Inter-step data passing** — the [`StepContext`] blackboard threads
//!   prior step results, variables, headers and input through
//!   [`Step::with_context`] / [`Node::with_context`] — pyfly's `@FromStep` /
//!   `@Input` / `@Variable` argument injection.
//! * **Workflow step compensation** — [`Node::with_compensation`] rolls back
//!   completed compensatable nodes in reverse order on any failure — pyfly's
//!   `WorkflowExecutor._compensate`.
//! * **Advanced primitives** — [`wait_all`] / [`wait_any`] gather/race over
//!   signals + timers, [`ChildWorkflowService`] (child workflows),
//!   [`ContinueAsNew`], conditional steps ([`Node::when`]), async
//!   fire-and-forget steps ([`Node::fire_and_forget`]),
//!   [`WorkflowQueryService`] and durable suspend/resume
//!   ([`DurableWorkflowState`]).
//!
//! # pyfly parity — observability
//!
//! The crate ports pyfly's `pyfly.transactional.core.{events,metrics,tracer}`:
//! an [`OrchestrationEvents`] async listener trait with the full lifecycle
//! hook set, a [`CompositeOrchestrationEvents`] fan-out, a `tracing`-backed
//! [`LoggerOrchestrationEvents`] default, an in-memory [`OrchestrationMetrics`]
//! listener (counters + p50/p95 latency histograms with a JSON
//! [`OrchestrationMetrics::snapshot`]), and an [`OrchestrationTracer`] span
//! facade. The saga / workflow / TCC engines fire the hooks when run through
//! their additive `run_with_listener` methods; the base `run` methods are
//! unchanged. An [`OrchestrationHealthIndicator`] surfaces persistence
//! liveness on `/actuator/health`, and TCC's
//! [`TccParticipant::with_context`] threads the try phase's result into
//! confirm / cancel (pyfly's `@FromTry`).
//!
//! # pyfly parity — saga composition
//!
//! [`SagaCompositor`] (+ [`SagaCompositionBuilder`] / [`SagaComposition`] /
//! [`CompositionEntry`] / [`SagaDataFlow`] / [`DataFlowManager`] /
//! [`CompositionValidator`] / [`CompositionContext`]) ports pyfly's
//! `pyfly.transactional.saga.composition` subpackage: orchestrate several
//! registered sagas as a DAG, running same-layer sagas concurrently, wiring
//! each saga's output into downstream sagas' input, and compensating all
//! completed sagas in reverse on a failure.
//!
//! # Quick start
//!
//! ```rust
//! use firefly_orchestration::{CompensationPolicy, Saga, SagaStatus, Step};
//!
//! let saga = Saga::new("checkout")
//!     .policy(CompensationPolicy::BestEffort)
//!     .step(
//!         Step::new("reserve", || async { Ok(()) })
//!             .with_compensation(|| async { Ok(()) }),
//!     )
//!     .step(
//!         Step::new("charge", || async { Ok(()) })
//!             .with_compensation(|| async { Ok(()) }),
//!     )
//!     .step(Step::new("ship", || async { Ok(()) }));
//!
//! let outcome = tokio::runtime::Runtime::new()
//!     .unwrap()
//!     .block_on(saga.run())
//!     .expect("saga completes");
//! assert_eq!(outcome.status, SagaStatus::Completed);
//! assert_eq!(outcome.steps_executed, ["reserve", "charge", "ship"]);
//! ```

mod cancel;
mod saga;
mod saga_composition;
mod tcc;
mod workflow;

// ── pyfly-parity durable-orchestration layer ────────────────────────────
mod condition;
mod dlq;
mod gateway;
mod health;
mod model;
mod observability;
mod persistence;
mod recovery;
mod registry;
mod report;
mod scheduling;
mod signal;
mod step_context;
mod step_invoker;
mod timer;
mod validator;
mod wait;
mod web;
mod workflow_advanced;

pub use cancel::CancellationToken;
pub use saga::{CompensationPolicy, Outcome, Saga, SagaError, SagaFailure, SagaStatus, Step};
pub use tcc::{ConfirmError, Tcc, TccError, TccParticipant};
pub use workflow::{Node, Workflow, WorkflowError};

// Observability: lifecycle event listeners, metrics, tracing.
pub use observability::{
    CompositeOrchestrationEvents, LoggerOrchestrationEvents, NoOpOrchestrationEvents,
    OrchestrationEvents, OrchestrationMetrics, OrchestrationTracer, SpanGuard,
};
// Persistence-backed health indicator.
pub use health::{OrchestrationHealthIndicator, ORCHESTRATION_HEALTH_INDICATOR_NAME};
// Multi-saga composition (DAG of sagas + cross-saga data flow + compensation).
pub use saga_composition::{
    CompositionContext, CompositionEntry, CompositionError, CompositionValidator, DataFlowManager,
    SagaComposition, SagaCompositionBuilder, SagaCompositor, SagaDataFlow,
};

// Durable model + value types.
pub use model::{
    ExecutionPattern, ExecutionState, ExecutionStatus, RetryPolicy, StepStatus, TccPhase,
    TriggerMode,
};
// Persistence port + adapters.
pub use persistence::{
    ExecutionFilter, MemoryPersistence, PersistenceError, PersistenceProvider, SqlitePersistence,
};
// Recovery service.
pub use recovery::{RecoveryAction, RecoveryDecider, RecoveryHandler, RecoveryService};
// Dead-letter queue.
pub use dlq::{
    DeadLetterCapture, DeadLetterEntry, DeadLetterFilter, DeadLetterService, DeadLetterStore,
    MemoryDeadLetterStore,
};
// Signal + timer services (workflow wait nodes).
pub use signal::{SignalError, SignalService};
pub use timer::TimerService;
// Inter-step data passing (runtime blackboard).
pub use step_context::StepContext;
// Conditional-step expression error (Node::when).
pub use condition::ConditionError;
// Per-step retry / backoff / jitter / timeout enforcement.
pub use step_invoker::{invoke_with_policy, StepInvokeError};
// Advanced wait/compose primitives (wait-all / wait-any).
pub use wait::{wait_all, wait_any, WaitError, WaitOutcome, WaitTarget};
// Advanced workflow primitives: compensation, child workflow,
// continue-as-new, conditional + async steps, query service, and durable
// suspend/resume.
pub use workflow_advanced::{
    ChildHandle, ChildWorkflowError, ChildWorkflowService, ContinueAsNew, DurableWorkflowState,
    WorkflowFactory, WorkflowQueryError, WorkflowQueryService,
};
// Event gateway + broker-driven saga starts.
pub use gateway::{trigger_handler, EventGateway, EventTrigger, TriggerHandler};
// Definition registry + listing accessors.
pub use registry::{DefinitionInfo, OrchestrationRegistry};
// Execution reports.
pub use report::{CompensationReport, ExecutionReport, StepReport};
// Definition validator (DAG lint).
pub use validator::{
    IssueLevel, OrchestrationValidator, ValidationError, ValidationIssue, ValidationReport,
};
// Scheduled saga / workflow / TCC starts.
pub use scheduling::{
    OrchestrationScheduler, ScheduleTrigger, ScheduledTask, ScheduledTaskInfo, SchedulerError,
};
// axum REST router for executions / DLQ / signals.
pub use web::{router, OrchestrationApi};

use std::future::Future;

/// Framework version stamp.
pub const VERSION: &str = "26.6.5";

/// Boxed error returned by step / node / participant callbacks — the Rust
/// analogue of Go's `error` interface value.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Boxed future produced by an orchestration action callback.
pub type ActionFuture = futures::future::BoxFuture<'static, Result<(), BoxError>>;

/// Type-erased action callback stored by the engines.
pub(crate) type ActionFn = Box<dyn Fn() -> ActionFuture + Send + Sync>;

/// Boxes an async closure into the engines' type-erased callback form.
pub(crate) fn boxed_action<F, Fut>(f: F) -> ActionFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), BoxError>> + Send + 'static,
{
    Box::new(move || -> ActionFuture { Box::pin(f()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_stamp() {
        assert_eq!(VERSION, "26.6.5");
    }

    #[test]
    fn engines_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Saga>();
        assert_send_sync::<Workflow>();
        assert_send_sync::<Tcc>();
        assert_send_sync::<Step>();
        assert_send_sync::<Node>();
        assert_send_sync::<TccParticipant>();
        assert_send_sync::<CancellationToken>();
        assert_send_sync::<Outcome>();
    }

    #[test]
    fn run_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let saga = Saga::new("s").step(Step::new("a", || async { Ok(()) }));
        assert_send(&saga.run());
        let workflow = Workflow::new("w").node(Node::new("a", || async { Ok(()) }));
        assert_send(&workflow.run());
        let tcc = Tcc::new("t");
        assert_send(&tcc.run());
    }
}
