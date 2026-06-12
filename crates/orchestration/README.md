# `firefly-orchestration`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-common-domain` orchestration · **Go module:** `orchestration`

## Overview

`firefly-orchestration` ships the three classic **distributed-transaction
engines** every Firefly platform agrees on:

| Engine     | Topology                      | Compensation                       |
|------------|-------------------------------|------------------------------------|
| `Saga`     | Sequential steps              | Reverse-order, configurable policy |
| `Workflow` | DAG with parallel branches    | None — fail-fast                   |
| `Tcc`      | Try-all then Confirm-all      | Cancel-tried-on-Try-failure        |

Each engine accepts a typed step / node / participant built from async
closures, runs as a plain future on the caller's task, and respects
cooperative cancellation through a `CancellationToken` — the Rust analogue
of the Go port's `context.Context` cancellation. The engines are
runtime-agnostic: they depend only on `futures`, so any executor (tokio
included) can drive them.

## `Saga`

Sequential execution with reverse-order compensation on any step failure.

```rust
use firefly_orchestration::{CompensationPolicy, Saga, SagaStatus, Step};

let saga = Saga::new("checkout")
    .policy(CompensationPolicy::BestEffort) // or CompensationPolicy::StopOnError
    .step(
        Step::new("reserve", || async { Ok(()) })
            .with_compensation(|| async { Ok(()) }),
    )
    .step(
        Step::new("charge", || async { Ok(()) })
            .with_compensation(|| async { Ok(()) }),
    )
    .step(Step::new("ship", || async { Ok(()) }));

let outcome = tokio::runtime::Runtime::new()
    .unwrap()
    .block_on(saga.run())
    .expect("saga completes");
// outcome.status:         Completed | Compensated | Failed
// outcome.steps_executed: ["reserve", "charge", "ship"]
// outcome.steps_rolled:   reverse-order names of compensations that ran
assert_eq!(outcome.status, SagaStatus::Completed);
assert_eq!(outcome.steps_executed, ["reserve", "charge", "ship"]);
```

On failure, `run` returns a `SagaFailure` carrying both the error and the
fully-populated `Outcome` — the Rust shape of Go's `(Outcome, error)`
return pair.

`CompensationPolicy`:
* `BestEffort` (default) — log + continue compensating remaining steps
  even if one compensation fails.
* `StopOnError` — abort rollback at first compensation failure; surface a
  `SagaError::Compensation` wrapping the original.

## `Workflow`

DAG of `Node`s with `depends_on` declarations. Independent nodes run
concurrently within a wave; the first node error short-circuits the run.

```rust
use firefly_orchestration::{Node, Workflow};

let workflow = Workflow::new("approval")
    .node(Node::new("credit-check", || async { Ok(()) }))
    .node(Node::new("fraud-scan", || async { Ok(()) }))
    .node(
        Node::new("approve", || async { Ok(()) })
            .depends_on(["credit-check", "fraud-scan"]),
    );

let result = tokio::runtime::Runtime::new()
    .unwrap()
    .block_on(workflow.run());
assert!(result.is_ok());
```

Duplicate node names and unknown dependencies are rejected up-front; an
unreachable node aborts the run with `"no progress (dependency cycle?)"`.
Failures within the same wave are aggregated one message per line,
mirroring Go's `errors.Join`.

## `Tcc`

Try-Confirm-Cancel. Try-all participants; Confirm-all on success;
Cancel-tried participants (reverse order, best-effort) on any Try failure.

```rust
use firefly_orchestration::{Tcc, TccParticipant};

let tcc = Tcc::new("transfer")
    .participant(
        TccParticipant::new("debit", || async { Ok(()) }, || async { Ok(()) })
            .with_cancel(|| async { Ok(()) }),
    )
    .participant(
        TccParticipant::new("credit", || async { Ok(()) }, || async { Ok(()) })
            .with_cancel(|| async { Ok(()) }),
    );

let result = tokio::runtime::Runtime::new()
    .unwrap()
    .block_on(tcc.run());
assert!(result.is_ok());
```

## Public surface

```rust,ignore
pub struct Step;                       // Step::new(name, execute).with_compensation(f)
pub struct Saga;                       // Saga::new(name).policy(p).step(s)
impl Saga {
    pub async fn run(&self) -> Result<Outcome, SagaFailure>;
    pub async fn run_cancellable(&self, token: &CancellationToken) -> Result<Outcome, SagaFailure>;
}
pub enum CompensationPolicy { BestEffort, StopOnError }
pub enum SagaStatus { Completed, Compensated, Failed }
pub struct Outcome { saga, status, steps_executed, steps_rolled, error, started_at, finished_at }
pub enum SagaError { Step, Compensation, Cancelled }  // SagaError::is_compensation_error
pub struct SagaFailure;                // .outcome() / .error() / .into_parts()

pub struct Node;                       // Node::new(name, run).depends_on(["a", "b"])
pub struct Workflow;                   // Workflow::new(name).node(n)
impl Workflow {
    pub async fn run(&self) -> Result<(), WorkflowError>;
    pub async fn run_cancellable(&self, token: &CancellationToken) -> Result<(), WorkflowError>;
}
pub enum WorkflowError { DuplicateNode, UnknownDependency, NoProgress, Node, Cancelled, Multiple }

pub struct TccParticipant;             // TccParticipant::new(name, try, confirm).with_cancel(f)
pub struct Tcc;                        // Tcc::new(name).participant(p)
impl Tcc { pub async fn run(&self) -> Result<(), TccError>; }
pub enum TccError { Try, Confirm }
pub struct ConfirmError { participant, source }

pub struct CancellationToken;          // new() / cancel() / is_cancelled()
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
```

## pyfly parity — durable orchestration layer

On top of the three in-process engines, the crate ports pyfly's
`pyfly.transactional` durability layer: persistent execution state, stuck-run
recovery, a dead-letter queue, signal/timer workflow nodes, broker-driven
saga starts, scheduled starts, definition validation, and a REST admin
surface.

### Execution model

```rust,ignore
pub enum ExecutionStatus { Pending, Running, Waiting, Suspended, Completed,
    Failed, Cancelled, TimedOut, Trying, Confirming, Confirmed, Canceling,
    Canceled, Compensating, Compensated }              // wire: "TIMED_OUT", …
pub enum ExecutionPattern { Saga, Workflow, Tcc }       // wire: "SAGA", …
pub enum StepStatus { Pending, Running, Done, Failed, Skipped, Compensating,
    Compensated, CompensationFailed }
pub enum TccPhase { Try, Confirm, Cancel }
pub enum TriggerMode { Sync, Async }
pub struct RetryPolicy { max_attempts, backoff_ms, timeout_ms, jitter, jitter_factor }
pub struct ExecutionState { correlation_id, name, pattern, status,
    started_at, updated_at, completed_at, payload }     // .to_json() / .from_json()
```

### Persistence + recovery

```rust,ignore
#[async_trait] pub trait PersistenceProvider {
    async fn save(&self, state: ExecutionState) -> Result<(), PersistenceError>;
    async fn load(&self, correlation_id: &str) -> Result<Option<ExecutionState>, _>;
    async fn list(&self, filter: ExecutionFilter) -> Result<Vec<ExecutionState>, _>;
    async fn list_stale(&self, before: DateTime<Utc>) -> Result<Vec<ExecutionState>, _>;
    async fn delete(&self, correlation_id: &str) -> Result<bool, _>;
    async fn cleanup(&self, older_than: Duration) -> Result<usize, _>;
    async fn is_healthy(&self) -> bool;
}
pub struct MemoryPersistence;                            // default in-process adapter
pub struct SqlitePersistence;                            // durable dev adapter over rusqlite

pub struct RecoveryService;   // .recover_stale() resumes / compensates / marks-failed;
                              // .cleanup() evicts old terminal history.
pub enum RecoveryAction { MarkFailed, Resume, Compensate, Skip }
```

`MemoryPersistence` and `SqlitePersistence` pass the identical port test suite
(`save`/`load`/`list`/filter/`list_stale`/`cleanup`/`delete`/health), and the
SQLite adapter additionally survives a reopen of its file. Production
deployments plug a server-grade `PersistenceProvider`; only the port and the
two dev adapters ship here (the workspace carries no redis/postgres driver).

### Dead-letter queue

```rust,ignore
#[async_trait] pub trait DeadLetterStore { add / get / list / delete / clear / count }
pub struct MemoryDeadLetterStore;
pub struct DeadLetterService;  // .capture(DeadLetterCapture) / .list / .get / .count /
                              // .mark_retried / .delete
pub struct DeadLetterEntry { id, execution_name, correlation_id, step_id,
    error_type, error_message, timestamp, retry_count, input }
```

### Signals & timers (workflow wait nodes)

```rust,ignore
pub struct SignalService;      // subscribe / wait_for / deliver / list_active / unregister
Node::wait_for_signal(name, &signals, correlation_id, signal)  // parks until delivered
pub struct TimerService;       // sleep_ms / sleep
Node::timer(name, Duration)    // sleeps, then completes
```

### Event gateway, scheduler & registry

```rust,ignore
pub struct EventGateway;       // register / register_saga_trigger(TriggerMode) /
                              // dispatch / bind(&Subscriber, topic) over firefly-eda
pub struct OrchestrationScheduler;   // register(ScheduledTask) / start / stop / list
pub enum ScheduleTrigger { FixedRate(Duration), FixedDelay(Duration), Cron(String) }
ScheduledTask::for_saga(&registry, name, trigger, mode)   // @scheduled_saga
pub struct OrchestrationRegistry;    // register_{saga,workflow,tcc}; {saga,workflow,tcc}_names();
                                    // definitions() -> Vec<DefinitionInfo>  (admin listing)
```

Cron triggers are inert without a cron evaluator, exactly as pyfly behaves
without `croniter`; fixed-rate / fixed-delay are the always-active forms.

### Definition validation & reports

```rust,ignore
pub struct OrchestrationValidator;   // validate_dag(target, graph) -> ValidationReport
pub struct ValidationReport { issues }  // has_errors() / raise_if_errors()
pub struct ExecutionReport;          // from_saga_outcome(cid, &Outcome) / from_state(&state)
```

`Workflow::graph()` lowers a workflow to the `node -> [deps]` shape the
validator lints for empty graphs, unknown dependencies, and cycles.

### REST router

```rust,ignore
pub fn router(api: OrchestrationApi) -> axum::Router;   // mounts /api/orchestration/*
```

| Method   | Path                                  | Behavior                              |
|----------|---------------------------------------|---------------------------------------|
| `GET`    | `/api/orchestration/executions`       | in-flight runs (default) or `?status=`|
| `GET`    | `/api/orchestration/executions/{cid}` | one run, `204` when absent            |
| `GET`    | `/api/orchestration/dlq`              | entries, `?execution_name=`/`?correlation_id=` |
| `GET`    | `/api/orchestration/dlq/count`        | `{"count": n}`                        |
| `GET`    | `/api/orchestration/dlq/{id}`         | one entry, `204` when absent          |
| `POST`   | `/api/orchestration/dlq/{id}/retry`   | `{"retried": bool}`                   |
| `DELETE` | `/api/orchestration/dlq/{id}`         | `{"deleted": bool}`                   |
| `POST`   | `/api/orchestration/workflow/signal`  | `{"delivered": bool}`                 |
| `GET`    | `/api/orchestration/definitions`      | registered definitions (admin)        |

## Testing

```bash
cargo test -p firefly-orchestration
```

Covers happy-path completion, reverse-order compensation, the two
compensation policies, concurrent DAG wave execution, fail-fast on
upstream error, cycle / duplicate / unknown-dependency validation, TCC
try / confirm / cancel orderings, joined confirm errors, cooperative
cancellation, and `Outcome` serde round-trips — plus the ported pyfly
transactional-engine suites: persistence (memory + sqlite), recovery,
dead-letter capture/retry, signal delivery, timer nodes, event-gateway
dispatch and broker-driven saga starts, scheduled fixed-rate/delay starts,
DAG validation, execution reports, and the `axum` REST router exercised via
`tower::ServiceExt::oneshot`.
