# Sagas, Workflows & TCC

By the end of this chapter, Lumen can **move money between two wallets** — and
do it *safely*. A transfer is not a single command: it debits one wallet, then
credits another, and those are two independent writes to two independent event
streams. If the credit leg fails after the debit already committed, the source
owner is out of pocket with nothing on the other side. There is no
`BEGIN … COMMIT` that spans two aggregates, so Lumen reaches for the pattern
that does the job: a **saga** that compensates the debit when the credit fails.

We build the `POST /api/v1/transfers` endpoint on top of the `Ledger` the CQRS
handlers already use, so a transfer raises *real* `MoneyWithdrawn` /
`MoneyDeposited` events on both streams — and a refund raises a real
`MoneyDeposited` on the source stream. Everything stays observable on the
event-sourced ledger you built in chapter 11.

`firefly-orchestration` ships the three classic **distributed-transaction
engines** every Firefly platform agrees on. Each composes async steps, runs as a
plain future on the caller's task, applies a per-step retry policy, threads a
typed context blackboard, and respects cooperative cancellation.

| Engine     | Topology                   | Compensation                       |
|------------|----------------------------|------------------------------------|
| `Saga`     | Sequential steps           | Reverse-order, configurable policy |
| `Workflow` | DAG with parallel branches | Reverse-order, configurable policy |
| `Tcc`      | Try-all then Confirm-all   | Cancel-tried-on-Try-failure        |

> **Spring parity.** This is the `firefly-common-domain` orchestration model:
> orchestrated multi-step processes with compensation, the same
> step/compensation shapes you know from the JVM's `@Saga` / `@SagaStep` (and
> pyfly's `@saga` / `@saga_step`), expressed here as async closures rather than
> a bean the container discovers.

## The problem with distributed writes

Make the failure modes concrete before writing a line of code. A Lumen transfer
has two legs:

1. **Debit the source** — `withdraw(amount)`, which enforces `balance >= 0`.
2. **Credit the destination** — `deposit(amount)`.

Each leg is an independent `Ledger` call that appends to that wallet's own event
stream. A missing destination wallet, or an overdraft on the source, fails one
leg after the other may already have committed. Retrying the *whole* operation
is unsafe — you might debit twice. Silently skipping the failed leg leaves the
balances inconsistent.

The principled answer is **eventual consistency with explicit compensation**.
Each leg commits to its own stream independently, and you design a recovery path
— a *compensating transaction* — for every step that could succeed before a
later one fails. A compensation is not a database rollback; it is a *semantic
undo*: "re-credit the source" is a brand-new `deposit` that restores the
balance, and it leaves an auditable refund event behind.

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
error (`failure.error()`) and the fully-populated `Outcome` (with `steps_rolled`
naming the compensations that ran, reverse order). The status is then
`Compensated` (rollback succeeded) or `Failed`.

`CompensationPolicy`:

- **`BestEffort`** (default) — log and continue compensating remaining steps
  even if one compensation fails.
- **`StopOnError`** — abort rollback at the first compensation failure and
  surface a `SagaError::Compensation` wrapping the original.

## Lumen's transfer saga

Lumen's transfer is a two-step saga: `debit` the source, then `credit` the
destination. The debit's compensation refunds the source; the credit is the last
leg, so it needs no compensation — a failure there rolls back only the debit. The
whole thing lives in `src/transfer.rs`, and it threads the `Ledger` through both
legs.

First, the wire types — the request body and the result `POST /api/v1/transfers`
returns:

```rust
use serde::{Deserialize, Serialize};

/// `POST /api/v1/transfers` command — move `amount` (cents) from `from` to `to`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
    /// The amount to move, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

/// The result of a completed (or compensated) transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResult {
    /// `"completed"` when both legs succeeded — the lowercase `SagaStatus`.
    pub status: String,
    pub from: String,
    pub to: String,
    pub amount: i64,
    #[serde(rename = "stepsExecuted")]
    pub steps_executed: Vec<String>,
    #[serde(rename = "stepsRolledBack")]
    pub steps_rolled_back: Vec<String>,
}
```

The result echoes the `SagaStatus` as a lowercase string plus the two step
lists, so the API tells the caller *exactly* what the engine did — which steps
ran and which were rolled back. A transfer also has a typed error that
distinguishes a malformed request from a clean, compensated business failure:

```rust
/// The typed error a transfer surfaces to its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// The request was malformed (same wallet, non-positive amount).
    Invalid(String),
    /// The transfer failed and was rolled back; the inner string is the
    /// failing leg's domain error (e.g. `insufficient funds`).
    Compensated(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Invalid(detail) => f.write_str(detail),
            TransferError::Compensated(detail) => write!(f, "transfer rolled back: {detail}"),
        }
    }
}

impl std::error::Error for TransferError {}
```

> **One dependency, even here.** `TransferError` hand-writes `Display` +
> `std::error::Error` instead of pulling in `thiserror` — the same discipline
> the rest of Lumen keeps, so the whole framework and every typed error still
> arrive through the single `firefly` facade.

### Building and running the saga

`run_transfer` validates the request, builds the two steps, and runs the saga.
The interesting part is how the *typed* domain cause escapes the saga engine's
generic `BoxError`: each leg stashes the failing `DomainError` in a shared
`Mutex` so we can surface "insufficient funds" to the caller verbatim rather
than an opaque boxed error string.

```rust
use std::sync::{Arc, Mutex};

use firefly::orchestration::{Saga, SagaStatus, Step};

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;

pub async fn run_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }
    let amount = Money::cents(req.amount);

    // Captures the domain error of the failing leg so the saga's generic
    // BoxError can be surfaced as a typed cause to the caller.
    let cause: Arc<Mutex<Option<DomainError>>> = Arc::new(Mutex::new(None));

    // Step 1 — debit the source; compensation refunds it.
    let debit = {
        let ledger = ledger.clone();
        let from = req.from.clone();
        let refund_ledger = ledger.clone();
        let refund_from = req.from.clone();
        let cause = Arc::clone(&cause);
        Step::new("debit", move || {
            let ledger = ledger.clone();
            let from = from.clone();
            let cause = Arc::clone(&cause);
            async move {
                ledger.withdraw(&from, amount).await.map_err(|e| {
                    *cause.lock().expect("cause lock") = Some(e.clone());
                    box_err(e)
                })?;
                Ok(())
            }
        })
        .with_compensation(move || {
            let ledger = refund_ledger.clone();
            let from = refund_from.clone();
            async move {
                // A refund is a normal deposit, so it raises a real
                // MoneyDeposited event on the source stream.
                ledger.deposit(&from, amount).await.map_err(box_err)?;
                Ok(())
            }
        })
    };

    // Step 2 — credit the destination (no compensation; it is the last leg,
    // so a failure here rolls back only the debit).
    let credit = {
        let ledger = ledger.clone();
        let to = req.to.clone();
        let cause = Arc::clone(&cause);
        Step::new("credit", move || {
            let ledger = ledger.clone();
            let to = to.clone();
            let cause = Arc::clone(&cause);
            async move {
                ledger.deposit(&to, amount).await.map_err(|e| {
                    *cause.lock().expect("cause lock") = Some(e.clone());
                    box_err(e)
                })?;
                Ok(())
            }
        })
    };

    let saga = Saga::new("money-transfer").step(debit).step(credit);

    match saga.run().await {
        Ok(outcome) => Ok(TransferResult {
            status: SagaStatus::Completed.to_string(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
            steps_executed: outcome.steps_executed,
            steps_rolled_back: outcome.steps_rolled,
        }),
        Err(failure) => {
            let detail = cause
                .lock()
                .expect("cause lock")
                .clone()
                .map(|e| e.to_string())
                .unwrap_or_else(|| failure.error().to_string());
            Err(TransferError::Compensated(detail))
        }
    }
}

/// Boxes a `DomainError` as the saga engine's `BoxError`.
fn box_err(e: DomainError) -> firefly::orchestration::BoxError {
    Box::<dyn std::error::Error + Send + Sync>::from(e.to_string())
}
```

**How it works.** Each `Step::new(name, action)` takes a closure returning a
future that resolves to `Result<(), BoxError>`. The debit leg clones the
`Ledger` (it is cheap to clone — `Arc`s inside) so the step owns what it
captures; `with_compensation` takes a *second* closure that runs only if a later
step fails. The credit leg has no compensation: it is the last step, so the only
thing to undo on its failure is the debit, which the engine handles
automatically by running the debit's compensation.

The `cause` `Mutex` is the bridge between the saga's untyped error channel and
Lumen's typed `DomainError`. When a leg fails, it records the `DomainError`
*before* boxing it; if `saga.run()` returns `Err`, we read the captured cause
back out and wrap it in `TransferError::Compensated`. That is how
`POST /api/v1/transfers` can answer with `insufficient funds` instead of a
stringly-typed box.

> **Spring parity.** On the JVM you would annotate a bean with `@Saga` and each
> method with `@SagaStep(compensate = "refundDebit")`, and the
> `WorkflowBeanPostProcessor` would discover and register it. Rust has no
> reflection, so Lumen builds the `Saga` value explicitly with `Step::new` /
> `with_compensation`. The shape is identical — forward step, named
> compensation, reverse-order rollback — but the wiring is a value you
> construct, not a bean the container scans.

### The endpoint

The controller method in `src/web.rs` is thin: drive the saga, then translate
the typed outcome into the HTTP contract. A clean rollback is a *business*
failure, so it surfaces as a `422` problem carrying the cause — not a `500`:

```rust
/// `POST /api/v1/transfers` — run a money transfer as a saga.
#[post("/transfers")]
async fn transfer(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<TransferResult>> {
    let result = run_transfer(&api.ledger, &body)
        .await
        .map_err(|e| match e {
            TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
            TransferError::Compensated(detail) => {
                WebError::from(FireflyError::validation(detail))
            }
        })?;
    // A transfer touches both wallets' views; invalidate the family.
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(result))
}
```

Note the `invalidate_type::<GetWallet>()` at the end: a transfer changed two
balances, so the cached `GetWallet` views must be dropped to keep a read after
the write honest. That cache and its invalidation are the subject of
[Caching](./17-caching.md); the transfer is just one more mutation that has to
play by its rules.

### What the saga does on each path

The tests in `src/transfer.rs` exercise all three paths, and they are the best
documentation of the behavior. The happy path moves funds and rolls back
nothing:

```rust
let result = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: dst.id.clone(), amount: 300 },
)
.await
.unwrap();

assert_eq!(result.status, "completed");
assert_eq!(result.steps_executed, ["debit", "credit"]);
assert!(result.steps_rolled_back.is_empty());
assert_eq!(balance(&ledger, &src.id).await, 700);
assert_eq!(balance(&ledger, &dst.id).await, 300);
```

The **overdraft** path short-circuits at the debit — the source never has the
funds, so the withdraw fails *before* anything applies. There is nothing to
compensate, and both balances are left intact:

```rust
let err = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: dst.id.clone(), amount: 500 },
)
.await
.unwrap_err();

assert_eq!(err, TransferError::Compensated("insufficient funds".into()));
assert_eq!(balance(&ledger, &src.id).await, 100); // untouched
assert_eq!(balance(&ledger, &dst.id).await, 0);   // untouched
```

The **credit-failure** path is where compensation earns its keep. The debit
applied, then the credit failed (the destination does not exist), so the engine
runs the debit's compensation — a refund deposit. The source's net balance is
restored, and the stream records *both* the debit and its refund, an audit trail
of exactly what happened:

```rust
let err = run_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: "wlt_missing".into(), amount: 400 },
)
.await
.unwrap_err();
assert!(matches!(err, TransferError::Compensated(_)));

// open(1000) − withdraw(400) + refund(400) = 1000, with 3 events on the stream.
let src_events = ledger.load_events(&src.id).await.unwrap();
assert_eq!(Wallet::rehydrate(&src.id, &src_events).view().balance, 1_000);
assert_eq!(src_events.len(), 3); // open + withdraw + refund-deposit
```

> **Sagas are eventually consistent.** A saga does not give you serializability.
> Between the moment the source is debited and the moment the credit commits (or
> the refund runs), another request could read the source and see a balance
> lower than it will ultimately be. That is the trade-off for operating across
> independent aggregates without a distributed lock: consistency *in the end* —
> all legs committed, or all compensated — not at every instant.

## Passing data between steps

Lumen's transfer steps are self-contained — each captures what it needs — but
many sagas need a later step to consume an earlier step's output. A step built
with `Step::with_context(name, |ctx| …)` reads the typed `StepContext`
blackboard; run the saga with `run_with_context(&ctx)`. Per-step
`with_retry(policy)` adds max attempts, exponential backoff, jitter, and a
per-attempt timeout, so a flaky leg can recover on its own before the engine
declares it failed.

## Workflow — a DAG

When a process has *independent* steps that should run in parallel, reach for a
`Workflow`: a DAG of `Node`s with `depends_on` declarations. Independent nodes
run concurrently within a wave; the first node error short-circuits the run:

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

> **Where Lumen would grow this.** A large transfer that needs a parallel
> compliance check (a credit check *and* a fraud scan that both feed an approval
> gate) is the textbook `Workflow`. The experience-tier starter,
> `firefly-starter-experience`, builds on exactly this engine with
> *signal-driven* workflow steps that park until an external caller delivers a
> named signal — the Rust spelling of pyfly's `@wait_for_signal` and the JVM's
> `@WaitForSignal`. We return to that tier in [HTTP Clients](./13-http-clients.md).

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

> **TCC vs. Saga.** Lumen models its transfer as a `Saga` because each leg
> commits locally and a refund cleanly undoes the debit. A TCC framing would
> instead *reserve* (hold the funds, pre-authorize the credit), then confirm
> both or cancel both — better when a participant can cheaply hold a reservation
> and you want all-or-nothing semantics with no window where one side has
> committed and the other has not.

## Cancellation

All three engines respect a `CancellationToken` — the Rust analog of the Go
port's `context.Context` cancellation and pyfly's cooperative cancellation. Pass
one to `run_cancellable(&token)` and cancel it from elsewhere to drain the run:

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

Lumen's money-transfer is a `Saga` (debit → credit, with a debit refund). A
multi-check approval (credit + fraud + KYC, then decision) is a `Workflow`. A
reserve-funds-then-capture flow across a wallet and a card processor is a `Tcc`.

## What changed in Lumen

- Lumen grew a **transfer saga** in `src/transfer.rs`: a `Saga` named
  `"money-transfer"` with a `debit` step (compensation: refund) and a `credit`
  step (no compensation — it is last).
- `run_transfer` validates the request, runs the saga, and translates the
  outcome into the typed `TransferResult` / `TransferError`, surfacing the
  *typed* failing `DomainError` (e.g. `insufficient funds`) by capturing it in a
  shared `Mutex` before the engine boxes it.
- The `POST /api/v1/transfers` endpoint in `src/web.rs` drives the saga, renders
  a clean rollback as a `422` business problem, and invalidates the `GetWallet`
  cache because the transfer changed two balances.
- The three behaviors — happy path (funds move), overdraft (short-circuit, both
  balances intact), and credit failure (debit refunded, three events on the
  source stream) — are pinned by tests, so the prose can never drift from the
  code.
- `TransferError` keeps the one-dependency discipline: hand-written `Display` +
  `Error`, no `thiserror`.

## Exercises

1. **Add a `notify` step.** Append a third, no-op `Step::new("notify", …)` to
   the saga that "sends a receipt" (just `Ok(())` for now). Assert that on the
   happy path `steps_executed == ["debit", "credit", "notify"]`, and that when
   the credit fails the notify step never runs and only the debit is rolled
   back.

2. **Make the debit retry.** Give the `debit` step a `with_retry(policy)` with a
   small number of attempts and a short backoff. Wrap a flaky `Ledger` that
   fails the first withdraw and succeeds the second, and assert the transfer
   still completes — proving per-step retry recovers a transient failure before
   compensation is ever considered.

3. **Switch the compensation policy.** Build a three-step saga where two
   completed steps both have compensations and the third step fails. Run it once
   with `CompensationPolicy::BestEffort` and once with `StopOnError`, making one
   compensation itself fail, and assert how `steps_rolled` and the returned
   `SagaError` differ between the two policies.

To call the external services these steps coordinate — a Payments processor, an
FX provider — you need an HTTP client. Continue to
[HTTP Clients](./13-http-clients.md).
