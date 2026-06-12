# Sagas, Workflows & TCC

`firefly-orchestration` ships the three classic **distributed-transaction
engines** every Firefly platform agrees on. Each composes async steps, runs as a
plain future on the caller's task, applies a per-step retry policy, threads a
typed context blackboard, and respects cooperative cancellation.

| Engine     | Topology                   | Compensation                       |
|------------|----------------------------|------------------------------------|
| `Saga`     | Sequential steps           | Reverse-order, configurable policy |
| `Workflow` | DAG with parallel branches | Reverse-order, configurable policy |
| `Tcc`      | Try-all then Confirm-all   | Cancel-tried-on-Try-failure        |

> **Spring parity** — This is the `firefly-common-domain` orchestration model:
> orchestrated multi-step processes with compensation, the same step/compensation
> shapes you know from the JVM, expressed as async closures.

## Saga — sequential with compensation

A `Saga` runs steps in order; on any step failure it compensates the completed
steps in reverse order. Attach a compensation to each step with
`with_compensation`:

```rust
use firefly_orchestration::{CompensationPolicy, Saga, SagaStatus, Step};

let saga = Saga::new("checkout")
    .policy(CompensationPolicy::BestEffort) // or StopOnError
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

assert_eq!(outcome.status, SagaStatus::Completed);
assert_eq!(outcome.steps_executed, ["reserve", "charge", "ship"]);
```

On success the outcome's `status` is `Completed` and `steps_executed` lists the
steps that ran. On failure, `run` returns a `SagaFailure` carrying both the
error and the fully-populated `Outcome` (with `steps_rolled` naming the
compensations that ran, reverse order). The status is then `Compensated`
(rollback succeeded) or `Failed`.

`CompensationPolicy`:

- **`BestEffort`** (default) — log and continue compensating remaining steps
  even if one compensation fails.
- **`StopOnError`** — abort rollback at the first compensation failure and
  surface a `SagaError::Compensation` wrapping the original.

### Passing data between steps

A step built with `Step::with_context(name, |ctx| …)` reads the typed
`StepContext` blackboard so a later step can consume earlier outputs. Run the
saga with `run_with_context(&ctx)`. Per-step `with_retry(policy)` adds max
attempts, exponential backoff, jitter, and a per-attempt timeout.

## Workflow — a DAG

A `Workflow` is a DAG of `Node`s with `depends_on` declarations. Independent
nodes run concurrently within a wave; the first node error short-circuits the
run:

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

`credit-check` and `fraud-scan` run in parallel; `approve` waits for both.
Duplicate node names and unknown dependencies are rejected up-front, and an
unreachable node aborts the run (`"no progress (dependency cycle?)"`).

Nodes get the same superpowers as saga steps: `Node::with_compensation` rolls
back in reverse completion order under the configurable `CompensationPolicy`,
`Node::with_context` reads prior results, `Node::when(expr)` skips a node on a
false predicate, and `Node::fire_and_forget` schedules it without blocking the
wave.

## TCC — Try / Confirm / Cancel

`Tcc` runs a two-phase protocol: Try all participants, then Confirm all on
success; on any Try failure, Cancel the tried participants in reverse order
(best-effort):

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

`TccParticipant::new(name, try_fn, confirm_fn)` takes the Try and Confirm phases;
`with_cancel` adds the compensation for a failed Try.

## Cancellation

All three engines respect a `CancellationToken` — the Rust analog of the Go
port's `context.Context` cancellation. Pass one to `run_cancellable(&token)` and
cancel it from elsewhere to drain the run:

```rust,ignore
use firefly_orchestration::CancellationToken;

let token = CancellationToken::new();
let handle = token.clone();
// ... cancel from a timeout or a shutdown signal:
handle.cancel();

let outcome = saga.run_cancellable(&token).await;
```

The engines depend only on `futures`, so any executor (Tokio included) drives
them.

## Choosing an engine

| Need                                          | Engine     |
|-----------------------------------------------|------------|
| Linear process, undo on failure               | `Saga`     |
| Parallel branches that join                   | `Workflow` |
| Reserve-then-commit across resources          | `Tcc`      |

A typical money-transfer is a `Saga` (debit → credit → notify, with debit/credit
compensations) or a `Tcc` (try-debit + try-credit, confirm both, cancel on any
try failure). A multi-check approval (credit + fraud + KYC, then decision) is a
`Workflow`.

To call the external services these steps coordinate, you need an HTTP client.
Continue to [HTTP Clients](./13-http-clients.md).
