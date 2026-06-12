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

## Testing

```bash
cargo test -p firefly-orchestration
```

Covers happy-path completion, reverse-order compensation, the two
compensation policies, concurrent DAG wave execution, fail-fast on
upstream error, cycle / duplicate / unknown-dependency validation, TCC
try / confirm / cancel orderings, joined confirm errors, cooperative
cancellation, and `Outcome` serde round-trips.
