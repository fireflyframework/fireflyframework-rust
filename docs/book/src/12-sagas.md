# Sagas, Workflows & TCC

By the end of this chapter Lumen can **move money between two wallets** — and do
it *safely*. A transfer is not a single command: it debits one wallet, then
credits another, and those are two independent writes to two independent event
streams. If the credit leg fails after the debit already committed, the source
owner is out of pocket with nothing on the other side. There is no
`BEGIN … COMMIT` that spans two aggregates, so Lumen reaches for the patterns
that do this job across a distributed boundary: a **saga** that compensates the
debit when the credit fails, a **workflow** that runs pre-flight checks in
parallel, and a **TCC** coordinator that reserves on both sides before
committing either.

You build all three on top of the event-sourced `Ledger` you grew in
[Event Sourcing](./11-event-sourcing.md), so a transfer raises *real*
`MoneyWithdrawn` / `MoneyDeposited` events on both streams — and a refund raises
a real `MoneyDeposited` on the source stream. Nothing here is a toy; every leg
drives the same application service the CQRS handlers use, and every outcome is
observable on the ledger.

By the end of this chapter you will:

- Explain *why* a money transfer across two aggregates needs a saga, not a
  database transaction, and what a *compensation* is.
- Declare a `Saga` with `#[firefly::saga]` and `#[saga_step]` — including
  `depends_on` ordering, a named `compensate` method, and per-step retry — then
  run it and read its `Outcome`.
- Declare a `Workflow` with `#[firefly::workflow]` that runs independent checks
  in a parallel layer and joins their typed verdicts in a decision node.
- Declare a TCC coordinator with `#[firefly::tcc]` and `#[participant]` to
  reserve-then-confirm across two resources.
- Mount all three on Lumen's web surface, rendering a clean rollback as an
  RFC 9457 `422` problem instead of a `500`.
- Choose the right engine for a given process, and recognise the
  eventual-consistency trade-off each one makes.

## Concepts you will meet

Before the first line of code, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — saga.** A *saga* is a sequence of local transactions
> where each step has a *compensating* action that semantically undoes it. If a
> later step fails, the engine runs the completed steps' compensations in reverse
> order. It is how you get "all-or-nothing" across services that cannot share one
> database transaction. The Java analog is the `@Saga` / `@SagaStep` pattern;
> pyfly spells it with saga decorators.

> **Note** **Key term — compensation.** A *compensation* is not a database
> rollback — it is a *semantic undo*. "Re-credit the source" is a brand-new
> `deposit` that restores the balance and leaves an auditable refund event
> behind; it does not erase history, it appends a correcting fact.

> **Note** **Key term — workflow (DAG).** A *workflow* is a directed acyclic
> graph of steps. Steps with no dependency between them run concurrently in the
> same *topological layer*; a step that declares `depends_on` waits for its
> predecessors. Use it when a process has independent branches that should run in
> parallel and then join.

> **Note** **Key term — TCC (Try-Confirm-Cancel).** *TCC* is a two-phase
> protocol: **Try** every participant (reserve resources), then **Confirm** all
> on success; on any Try failure, **Cancel** the participants already tried.
> Where a saga applies each leg immediately and undoes it later, TCC reserves
> first and only commits once every reservation succeeded.

> **Note** **Key term — eventual consistency.** Operating across independent
> aggregates without a distributed lock means there is a window where one leg has
> committed and another has not. These engines guarantee consistency *in the end*
> — all legs committed, or all compensated — not at every instant.

`firefly-orchestration` ships the three classic distributed-transaction engines
every Firefly platform agrees on. Each composes async steps, runs as a plain
future on the caller's task, applies a per-step retry policy, threads a typed
context blackboard, and respects cooperative cancellation. And — this is the key
property — you do not hand-build them as values. Lumen declares each engine with
an attribute macro on an `impl` block, exactly as it declares CQRS handlers and
controllers.

| Engine     | Topology                   | Compensation                       | Declared with                      |
|------------|----------------------------|------------------------------------|------------------------------------|
| `Saga`     | Dependency-ordered steps   | Reverse-order, configurable policy | `#[saga]` + `#[saga_step]`         |
| `Workflow` | DAG with parallel layers   | Reverse-order, configurable policy | `#[workflow]` + `#[workflow_step]` |
| `Tcc`      | Try-all then Confirm-all   | Cancel-tried on a Try failure      | `#[tcc]` + `#[participant]`        |

> **Design note.** Firefly's orchestration model is *declarative*. You write an
> ordinary `impl` block of `async fn(&self, …) -> Result<T, E>` methods and
> annotate them: `#[saga_step]` for a saga leg, `#[workflow_step]` for a DAG
> node, `#[participant]` for a TCC actor. The macro lowers those methods onto the
> same `firefly-orchestration` engines — `depends_on` orders them, `compensate`
> names the undo, a step's `Ok(T)` is published for later steps, and an `Err(E)`
> triggers compensation in reverse order. If you have used Java's `@Saga` or
> pyfly's saga decorators, this is the Rust spelling: the control flow lives in
> methods you can read top to bottom, and the wiring is generated for you.

## Step 1 — Understand the problem with distributed writes

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
— a compensating transaction — for every step that could succeed before a later
one fails. "Re-credit the source" is a brand-new `deposit` that restores the
balance, and it leaves an auditable refund event behind.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 220" role="img"
     aria-label="Saga with compensation: forward steps debit, credit and notify run in dependency order; if credit fails, the engine runs the debit's compensation in reverse order to refund"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="280.0" y="24.0" text-anchor="middle" font-size="12" font-weight="700" fill="#3a2a1c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">forward: dependency-ordered steps</text>
<rect x="40.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="115.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">debit</text><text x="115.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">withdraw(amount)</text><line x1="190.0" y1="74.0" x2="216.0" y2="74.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="224.0,74.0 216.0,78.5 216.0,69.5" fill="#b5531f"/><rect x="224.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="224.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="299.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">credit</text><text x="299.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit(amount)</text><line x1="374.0" y1="74.0" x2="400.0" y2="74.0" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="408.0,74.0 400.0,78.5 400.0,69.5" fill="#b5531f"/><rect x="408.0" y="50.5" width="150.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="408.0" y="48.0" width="150.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="483.0" y="71.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">notify</text><text x="483.0" y="85.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">publish event</text>
<text x="299.0" y="44.0" text-anchor="middle" font-size="10.5" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">may fail</text>
<path d="M299.0,100 V150 H115.0 V108" fill="none" stroke="#b03a2e" stroke-width="2.6" stroke-dasharray="6 5" stroke-linecap="round"/>
<polygon points="115.0,100 110.5,109.0 119.5,109.0" fill="#b03a2e"/>
<text x="207.0" y="143.0" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">compensate — reverse order</text>
<text x="280.0" y="200.0" text-anchor="middle" font-size="11" font-weight="600" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">a compensation is a forward undo, not a database rollback</text>
</svg>
<figcaption>A saga runs its steps in dependency order. If a step fails, the engine runs the already-completed steps' compensations in <strong>reverse order</strong> — here a failed <code>credit</code> refunds the <code>debit</code>. A compensation is a forward action that undoes, not a database rollback.</figcaption>
</figure>

What just happened: you named the two writes, saw why neither retry nor
skip-the-failure is safe, and settled on the saga shape — debit, then credit,
with a refund waiting if the credit ever fails. The rest of the chapter turns
that shape into code.

> **Tip** **Checkpoint.** You can state, in one sentence each, why a money
> transfer cannot be a single database transaction and what a compensation does
> that a rollback does not. If both are clear, you are ready to declare the saga.

## Step 2 — Declare the wire types

Lumen's transfer lives in `src/transfer.rs`. Start with the types that cross the
HTTP boundary: the request body and the result `POST /api/v1/transfers` returns.

```rust
use serde::{Deserialize, Serialize};

/// `POST /api/v1/transfers` command — move `amount` (cents) from `from` to `to`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
#[serde(default)]
pub struct TransferRequest {
    /// The source wallet id (debited).
    pub from: String,
    /// The destination wallet id (credited).
    pub to: String,
    /// The amount to move, in minor units (cents); must be `> 0`.
    pub amount: i64,
}

/// The result of a completed (or compensated) transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
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

What just happened: `TransferRequest` carries the two wallet ids and an amount in
minor units (cents). `TransferResult` echoes the saga's status as a lowercase
string plus the two step lists, so the API tells the caller *exactly* what the
engine did — which steps ran and which were rolled back.

> **Note** **Key term — `firefly::Schema`.** The `Schema` derive teaches the
> auto-generated OpenAPI docs (served on the management port) what this DTO looks
> like. It is the Rust analog of springdoc's model reflection, computed at
> compile time. You met the management-port docs in
> [Quickstart](./02-quickstart.md); every DTO that crosses the wire derives it.

A transfer also needs a typed error that distinguishes a malformed request from a
clean, compensated business failure:

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

What just happened: `Invalid` is a bad request (a `422` that never touched the
ledger); `Compensated` is a business failure that ran, rolled back cleanly, and
carries the failing leg's cause. Keeping them as distinct variants lets the
endpoint map each to the right HTTP status.

> **Note** Lumen hand-writes `Display` + `std::error::Error` instead of pulling
> in `thiserror`. That is the same one-dependency discipline the rest of the book
> keeps: the whole framework and every typed error still arrive through the
> single `firefly` facade, with no extra crate to align.

## Step 3 — Declare the saga

The saga is an `impl` block on a tiny struct that holds the `Ledger`. Each leg is
an annotated method that calls the ledger directly and returns a typed
`Result<(), DomainError>`. There is no closure capture, no `Mutex` to smuggle the
cause out of an erased error channel, no builder call — the macro reads the
attributes and generates all of it.

```rust
use std::sync::Arc;

use firefly::orchestration::SagaError;

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;

/// The money-transfer saga, declared with `#[firefly::saga]`: each leg is an
/// annotated method driving the `Ledger`. The macro generates
/// `TransferSaga::run` (used by `run_transfer`) and `TransferSaga::saga`.
struct TransferSaga {
    ledger: Ledger,
}

#[firefly::saga(name = "money-transfer")]
impl TransferSaga {
    /// Debit the source wallet (a real `MoneyWithdrawn` event). Rolled back by
    /// `refund_debit` when a later leg fails.
    #[saga_step(id = "debit", compensate = "refund_debit")]
    async fn debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.withdraw(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    /// Compensation for `debit`: a refund is a normal deposit, so it raises a
    /// real `MoneyDeposited` event on the source stream.
    async fn refund_debit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.from, Money::cents(req.amount)).await?;
        Ok(())
    }

    /// Credit the destination (a real `MoneyDeposited` event). The last leg, so
    /// a failure here rolls back only the debit.
    #[saga_step(id = "credit", depends_on = ["debit"])]
    async fn credit(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
```

How it reads, block by block:

- `debit` is the first step (`id = "debit"`), and it names its undo with
  `compensate = "refund_debit"`.
- `refund_debit` carries *no* `#[saga_step]` marker — it is a plain method
  referenced by name, and the macro includes it in the generated saga only
  because `debit` points at it.
- `credit` declares `depends_on = ["debit"]`, so the engine runs it strictly
  after the debit. It has no compensation because it is the last leg: the only
  thing to undo on its failure is the debit, which the engine handles
  automatically.

Each leg takes `#[input] req: TransferRequest`. That marker is the heart of the
model.

> **Note** **Key term — parameter injection.** Each step's parameters are
> *injected* from the saga context by markers the macro reads and strips:
> `#[input]` is the whole input (or `#[input("field")]` for one field);
> `#[from_step("id")]` is the `Ok` value an earlier step published;
> `#[variable("key")]` is a saga-scoped context variable; and `#[ctx]` is the
> `StepContext` blackboard itself. Because every step here needs the whole
> request, every parameter is `#[input] req: TransferRequest`.

A step's `Ok(T)` is serialised and made available to later steps via
`#[from_step]`; an `Err(E)` (where `E: std::error::Error + Send + Sync`) triggers
compensation in reverse order. Because the methods return `DomainError` directly,
the typed failure cause is preserved all the way through the engine — no shared
`Mutex` needed.

The `#[saga_step]` attribute accepts `id` (required), `depends_on = ["…"]`,
`compensate = "method"`, and the per-step recovery knobs `retry`, `backoff_ms`,
`timeout_ms`, and `jitter`. The `#[saga(...)]` attribute accepts a `name`, a
`crate` facade override, and a compensation `policy`:

- **`best_effort`** (the engine default) — log and continue compensating the
  remaining steps even if one compensation fails.
- **`stop_on_error`** — abort rollback at the first compensation failure and
  surface a `SagaError::Compensation` wrapping the original.
- plus `retry_with_backoff`, `circuit_breaker`, `best_effort_parallel`, and
  `grouped_parallel` for larger fan-outs.

> **Note** This is the same shape as Java's `@Saga` / `@SagaStep` and pyfly's
> saga decorators — a step method, a compensation named by string, a `depends_on`
> order — but lowered onto Rust's type system. A step that returns the wrong type
> or names a compensation that does not exist is a *compile error*, not a runtime
> surprise.

> **Design note.** Nothing here is reflection or runtime scanning. `#[saga]`
> expands at compile time into the exact `Saga::new(...).step(...)` calls you
> would otherwise write by hand, threaded through the `firefly` facade's `__rt`
> contract so a one-dependency service compiles it without ever naming
> `firefly-orchestration`. If you ever need to construct a saga dynamically
> (steps known only at run time), the same engine exposes the programmatic seam
> the macro lowers onto:
> `Saga::new(name).step(Step::with_context(id, action).with_context_compensation(undo))`.

> **Tip** **Checkpoint.** You have a `TransferSaga` struct, a `#[firefly::saga]`
> `impl` with two `#[saga_step]` legs and one named compensation. `cargo build`
> should compile it — and if you rename `refund_debit` without updating the
> `compensate = "…"` string, the build should fail with a message pointing at the
> offending line. Try it, then change it back.

## Step 4 — Run the saga

The macro generates two methods on the type:

- `TransferSaga::saga(self: Arc<Self>) -> Saga` — builds the engine from your
  steps, their `depends_on` order, compensations, and retry policies.
- `TransferSaga::run(self: Arc<Self>, input) -> Result<Outcome, SagaFailure>` —
  serialises `input` into a fresh step context and runs the whole DAG,
  compensating on failure.

`run_transfer` validates the request, constructs the saga value behind an `Arc`,
and calls the generated `run`. On success it reads the `Outcome`; on failure it
unwraps the failing leg's typed `DomainError` out of the engine's
`SagaError::Step` so the API can answer `insufficient funds` verbatim:

```rust
/// Validates and runs a money transfer as a declarative saga, returning the
/// terminal `TransferResult`.
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

    let saga = Arc::new(TransferSaga {
        ledger: ledger.clone(),
    });
    match saga.run(req.clone()).await {
        Ok(outcome) => Ok(TransferResult {
            status: outcome.status.to_string(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
            steps_executed: outcome.steps_executed,
            steps_rolled_back: outcome.steps_rolled,
        }),
        Err(failure) => {
            // Surface the failing leg's typed domain error (e.g. "insufficient
            // funds"), unwrapped from the saga's generic step error.
            let detail = match failure.error() {
                SagaError::Step { source, .. } => source.to_string(),
                other => other.to_string(),
            };
            Err(TransferError::Compensated(detail))
        }
    }
}
```

What the generated `run` does for you, line by line:

- `saga.run(req.clone())` serialises `req` into a fresh `StepContext`, builds the
  saga (`debit` → `credit`, with the debit's compensation attached), and runs the
  DAG.
- On the happy path it returns an `Outcome` whose `status` is `Completed`,
  `steps_executed` lists the legs that ran, and `steps_rolled` is empty. Note the
  field is `outcome.steps_rolled` on the engine side; `run_transfer` copies it
  into the wire field `steps_rolled_back`.
- On failure it returns a `SagaFailure`: its `outcome()` is fully populated
  (status `Compensated`, with `steps_rolled` naming the compensations that ran),
  and its `error()` is a `SagaError`. We match `SagaError::Step { source, .. }`
  to recover the leg's `DomainError` message — that is how
  `POST /api/v1/transfers` answers `insufficient funds` instead of an opaque
  `step "credit" failed`.

> **Note** **Key term — `Outcome` / `SagaFailure`.** `Outcome` is the saga's
> terminal record: `status` (a `SagaStatus` that displays lowercase —
> `completed` / `compensated` / `failed`), `steps_executed`, and `steps_rolled`.
> `SagaFailure` is the failure pair — `outcome()` gives the same record, and
> `error()` gives the typed `SagaError` that ended the run. There is no separate
> "did it roll back?" flag to consult; the outcome tells you everything.

> **Tip** **Checkpoint.** `run_transfer` compiles and its three branches are
> clear: an `Invalid` validation failure, an `Ok(Outcome)` happy path, and a
> `SagaError::Step` failure unwrapped into `TransferError::Compensated`. You are
> ready to mount it.

## Step 5 — Mount the saga endpoint

The controller method in `src/web.rs` is thin: drive the saga, then translate the
typed outcome into the HTTP contract. A clean rollback is a *business* failure,
so it surfaces as a `422` problem carrying the cause — not a `500`:

```rust
/// `POST /api/v1/transfers` — run a money transfer as a saga.
#[post(
    "/transfers",
    summary = "Transfer funds (saga)",
    description = "Moves funds between two wallets as a compensating saga (debit then credit).",
    tags = ["Transfers"],
    status = 200
)]
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

What just happened:

- `run_transfer` returns the typed `TransferError`; the `map_err` translates both
  variants into a validation problem. `FireflyError::validation(...)` renders as
  an RFC 9457 `422 application/problem+json` document carrying the detail string,
  so the caller sees `insufficient funds`, not a stack trace.
- `invalidate_type::<GetWallet>()` drops the cached `GetWallet` views, because a
  transfer changed two balances and a read after the write must be honest. That
  cache and its invalidation are the subject of [Caching](./17-caching.md); the
  transfer is just one more mutation that plays by its rules.

> **Note** This handler lives inside Lumen's `#[rest_controller(path = "...")]`
> `impl WalletApi`, mounted automatically at boot — you never edit `main` to add
> a route. `WebResult<T>` is `Result<T, WebError>`, and any `WebError` renders as
> an RFC 9457 problem. You first met both in
> [Your First HTTP API](./06-first-http-api.md).

## Step 6 — Read the three saga paths

The tests in `src/transfer.rs` exercise all three paths, and they are the best
documentation of the behaviour. The **happy path** moves funds and rolls back
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

What just happened: the third assertion is the whole point of compensation as a
*semantic undo*. The balance is restored to `1_000`, but the stream is **not**
two events long as if nothing happened — it is *three* events long: the open, the
withdraw, and the refund deposit. The history of what actually occurred is
preserved and auditable.

> **Note** A saga does not give you serializability. Between the moment the
> source is debited and the moment the credit commits (or the refund runs),
> another request could read the source and see a balance lower than it will
> ultimately be. That is the trade-off for operating across independent
> aggregates without a distributed lock: consistency *in the end* — all legs
> committed, or all compensated — not at every instant.

> **Tip** **Checkpoint.** Run `cargo test -p lumen transfer`. The happy-path,
> overdraft, and credit-failure tests pass, and the credit-failure test confirms
> three events on the source stream. That three-event trail is your proof the
> compensation appended rather than erased.

## Step 7 — Add a parallel compliance workflow

A large transfer should be gated behind compliance checks *before* the money
moves. Those checks are independent of each other — a balance check and a
per-transfer ceiling have nothing to do with one another — so they should run in
parallel. That is a `Workflow`: a DAG of nodes with `depends_on` declarations,
where independent nodes run concurrently within a topological layer and a node
that declares dependencies runs only after they complete.

You declare it with `#[firefly::workflow]` and mark each node with
`#[workflow_step]` — the same parameter injection as a saga. `#[workflow_step]`
accepts `id` (required), `depends_on = ["…"]`, `compensate = "method"`,
`when = "expr"` (a skip condition — the node is skipped when the predicate is
false), and `fire_and_forget` (schedule the node without blocking the layer). The
macro generates `Workflow::workflow(self: Arc<Self>)` and
`run(self, input) -> Result<(), WorkflowError>`.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 560 220" role="img"
     aria-label="Workflow DAG: balance-check and limit-check run in parallel in one layer and both feed the approve gate, which depends on both"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="170.0" y="26.0" text-anchor="middle" font-size="11" font-weight="700" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">parallel layer</text>
<rect x="40.0" y="42.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="40.0" width="188.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="134.0" y="63.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">balance-check</text><text x="134.0" y="77.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">funds_ok: bool</text>
<rect x="40.0" y="130.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="40.0" y="128.0" width="188.0" height="52.0" rx="9" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/><text x="134.0" y="151.0" text-anchor="middle" font-size="13.0" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">limit-check</text><text x="134.0" y="165.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">within_limit: bool</text>
<rect x="360.0" y="86.5" width="188.0" height="52.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="360.0" y="84.0" width="188.0" height="52.0" rx="9" fill="#fff6e6" stroke="#e0b96a" stroke-width="1.5"/><text x="454.0" y="107.0" text-anchor="middle" font-size="14" font-weight="700" fill="#2a1d10" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">approve</text><text x="454.0" y="121.0" text-anchor="middle" font-size="10.0" fill="#7a6450" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">depends_on both</text>
<path d="M228.0,66.0 Q288.2,105.2 352.0,102.4" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="360.0,102.0 352.2,106.9 351.8,97.9" fill="#b5531f"/>
<path d="M228.0,154.0 Q288.2,114.8 352.0,117.6" fill="none" stroke="#d4793a" stroke-width="3.0" stroke-linecap="round"/><polygon points="360.0,118.0 351.8,122.1 352.2,113.1" fill="#b5531f"/>
</svg>
<figcaption>A workflow is a DAG of steps. <code>balance-check</code> and <code>limit-check</code> have no dependency on each other, so they run in the same parallel layer; <code>approve</code> waits for both and consumes their verdicts.</figcaption>
</figure>

Lumen's `src/compliance.rs` runs two independent checks in parallel and then an
approval gate that consumes both. First the error type and the policy input:

```rust
use std::sync::Arc;

use firefly::orchestration::WorkflowError;

use crate::domain::Wallet;
use crate::ledger::Ledger;
use crate::transfer::TransferRequest;

/// The per-transfer ceiling, in minor units (cents).
pub const MAX_TRANSFER_CENTS: i64 = 1_000_000; // 10,000.00

/// Why a transfer failed compliance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComplianceError {
    /// The source wallet does not exist, so its balance cannot be checked.
    NotFound(String),
    /// A check failed — the transfer is not allowed (the string says why).
    Rejected(String),
}

impl std::fmt::Display for ComplianceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplianceError::NotFound(id) => write!(f, "source wallet {id} not found"),
            ComplianceError::Rejected(why) => write!(f, "transfer rejected: {why}"),
        }
    }
}

impl std::error::Error for ComplianceError {}
```

Now the workflow itself. `balance-check` and `limit-check` have no dependency on
each other, so the engine runs them in the same topological layer; `approve`
declares `depends_on` on both and reads their boolean verdicts through
`#[from_step(...)]`:

```rust
/// The compliance workflow: each node drives the `Ledger` or a policy input.
struct ComplianceCheck {
    ledger: Ledger,
    max_cents: i64,
}

#[firefly::workflow(name = "transfer-compliance")]
impl ComplianceCheck {
    /// Does the source wallet hold enough to cover the transfer? Reads the real
    /// source aggregate. Errors if the source does not exist.
    #[workflow_step(id = "balance-check")]
    async fn balance_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        let events = self
            .ledger
            .load_events(&req.from)
            .await
            .map_err(|e| ComplianceError::NotFound(e.to_string()))?;
        if events.is_empty() {
            return Err(ComplianceError::NotFound(req.from.clone()));
        }
        let balance = Wallet::rehydrate(&req.from, &events).view().balance;
        Ok(balance >= req.amount)
    }

    /// Is the amount within the per-transfer ceiling? Independent of the
    /// balance check, so it runs in the same parallel layer.
    #[workflow_step(id = "limit-check")]
    async fn limit_check(&self, #[input] req: TransferRequest) -> Result<bool, ComplianceError> {
        Ok(req.amount <= self.max_cents)
    }

    /// The decision node: runs only after both checks (`depends_on`) and
    /// consumes their boolean verdicts via `#[from_step]`.
    #[workflow_step(id = "approve", depends_on = ["balance-check", "limit-check"])]
    async fn approve(
        &self,
        #[from_step("balance-check")] funds_ok: bool,
        #[from_step("limit-check")] within_limit: bool,
    ) -> Result<(), ComplianceError> {
        if !funds_ok {
            return Err(ComplianceError::Rejected("insufficient funds".into()));
        }
        if !within_limit {
            return Err(ComplianceError::Rejected(format!(
                "amount exceeds the {} cent per-transfer ceiling",
                self.max_cents
            )));
        }
        Ok(())
    }
}
```

What just happened — and why it matters: this is the payoff of the injection
model. `balance_check` returns `Ok(true)` or `Ok(false)`, and the engine
serialises that `bool` under the node id `balance-check`. `approve` declares
`#[from_step("balance-check")] funds_ok: bool`, and the macro deserialises the
stored value back into that parameter — typed at both ends, with no manual
context plumbing. `balance-check` reads the *real* source aggregate from the
`Ledger`; only the per-transfer ceiling is a new policy input.

> **Tip** **Checkpoint.** Notice the difference in topology from the saga: the
> saga's `credit` declares `depends_on = ["debit"]` so the two run *in series*;
> the workflow's `balance-check` and `limit-check` declare *no* dependency on each
> other, so they run *in the same layer*. Only `approve` waits. That single
> `depends_on` difference is the difference between a chain and a DAG.

## Step 8 — Run the workflow and recover the cause

`run_compliance` builds the workflow behind an `Arc` and calls the generated
`run`. `Ok(())` means approved; an `Err` is recovered into a typed
`ComplianceError`. The workflow engine surfaces a node failure as
`WorkflowError::Node { source, .. }`, where the boxed `source` can be downcast
back to the original error:

```rust
/// Runs the compliance workflow for `req`. `Ok(())` means the transfer is
/// approved (both checks passed); `Err` carries the typed reason it was rejected.
pub async fn run_compliance(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<(), ComplianceError> {
    let check = Arc::new(ComplianceCheck {
        ledger: ledger.clone(),
        max_cents: MAX_TRANSFER_CENTS,
    });
    match check.run(req.clone()).await {
        Ok(()) => Ok(()),
        Err(failure) => Err(compliance_cause(failure)),
    }
}

/// Recovers a typed `ComplianceError` from the failing node's error.
fn compliance_cause(failure: WorkflowError) -> ComplianceError {
    let detail = match &failure {
        WorkflowError::Node { source, .. } => {
            if let Some(err) = source.downcast_ref::<ComplianceError>() {
                return err.clone();
            }
            source.to_string()
        }
        other => other.to_string(),
    };
    if detail.contains("not found") {
        ComplianceError::NotFound(detail)
    } else {
        ComplianceError::Rejected(detail)
    }
}
```

What just happened: `WorkflowError::Node` boxes the failing node's error as a
`source`. `compliance_cause` first tries `downcast_ref::<ComplianceError>()` to
recover the exact typed variant; if that succeeds it returns the original error
verbatim. The fall-through string-matching is a belt-and-suspenders path for when
the boxed type cannot be downcast.

The endpoint in `src/web.rs` is a read-only pre-check that never moves funds —
`200 OK` with the decision when approved, `404` when the source wallet is
unknown, and `422` carrying the reason when a compliance check rejects:

```rust
/// `POST /api/v1/transfers/compliance` — gate a transfer through the parallel
/// compliance workflow (balance + limit checks → approve).
#[post(
    "/transfers/compliance",
    summary = "Compliance-gated transfer (workflow)",
    description = "Runs the parallel compliance workflow (balance + limit checks) before approving a transfer.",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer_compliance(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<serde_json::Value>> {
    run_compliance(&api.ledger, &body).await.map_err(|e| match e {
        // An unknown source wallet is a 404 (like GET /wallets/:id); a
        // failed check is a 422.
        ComplianceError::NotFound(detail) => WebError::from(FireflyError::not_found(detail)),
        ComplianceError::Rejected(detail) => WebError::from(FireflyError::validation(detail)),
    })?;
    Ok(Json(serde_json::json!({
        "decision": "approved",
        "from": body.from,
        "to": body.to,
        "amount": body.amount,
    })))
}
```

What just happened: a missing source maps to `FireflyError::not_found` (a `404`
problem, consistent with `GET /wallets/:id`), and a rejected check maps to
`FireflyError::validation` (a `422` problem). Because the check never moves
funds, there is no cache to invalidate.

> **Note** The experience-tier starter, `firefly-starter-experience`, builds on
> exactly this workflow engine with *signal-driven* steps that park until an
> external caller delivers a named signal, then resume from where they left off.
> We return to that tier in [HTTP Clients](./13-http-clients.md).

> **Tip** **Checkpoint.** Run `cargo test -p lumen compliance`. A funded,
> in-limit transfer is approved; an overdrawn one is `Rejected`; an over-ceiling
> one is `Rejected` with a "ceiling" message; an unknown source is `NotFound`.

## Step 9 — Reframe the transfer as TCC

The same transfer can be modelled a second way — reserve-then-capture — and Lumen
ships both so you can compare them. `Tcc` runs a two-phase protocol: **Try** every
participant (reserve resources), then **Confirm** all on success; on any Try
failure, **Cancel** the participants already tried, in reverse order. Where a
saga applies each leg immediately and undoes a committed leg on failure, TCC
reserves first and only commits once every reservation succeeded — so a failed
reservation is cancelled, never compensated after the fact.

You declare it with `#[firefly::tcc]` and mark each *try* method with
`#[participant(name, confirm, cancel)]`. The confirm and cancel methods are plain
`async fn` referenced by name. A participant's try result is published under its
name, so confirm and cancel can read it via `#[from_step("<name>")]`.
`#[participant]` accepts `name` and `confirm` (required), plus `cancel`, `retry`,
`backoff_ms`, and `timeout_ms`. The macro generates `Tcc::tcc(self: Arc<Self>)`
and `run(self, input) -> Result<(), TccError>`.

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 616 250" role="img"
     aria-label="TCC phases for two participants source and dest: a Try column reserves, a Confirm column captures on success, and a Cancel column releases on a try failure"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
<text x="176.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Try</text>
<text x="176.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#b5531f" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">reserve</text>
<text x="356.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Confirm</text>
<text x="356.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">on all-tried</text>
<text x="536.0" y="28.0" text-anchor="middle" font-size="14" font-weight="800" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">Cancel</text>
<text x="536.0" y="44.0" text-anchor="middle" font-size="10" font-weight="600" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">on a try failure</text>
<text x="20.0" y="88.0" text-anchor="start" font-size="11.5" font-weight="700" fill="#8a6d3b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">source</text>
<rect x="97.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="97.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#fdf6ea" stroke="#d4793a" stroke-width="1.5"/><text x="176.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">withdraw (hold)</text>
<rect x="277.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="277.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#ecf9f0" stroke="#1f8a4c" stroke-width="1.5"/><text x="356.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">(none — held)</text>
<rect x="457.0" y="62.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="457.0" y="60.0" width="158.0" height="46.0" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="1.5"/><text x="536.0" y="87.5" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit (release)</text>
<text x="20.0" y="162.0" text-anchor="start" font-size="11.5" font-weight="700" fill="#8a6d3b" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">dest</text>
<rect x="97.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="97.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#fdf6ea" stroke="#d4793a" stroke-width="1.5"/><text x="176.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#d4793a" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">verify exists</text>
<rect x="277.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="277.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#ecf9f0" stroke="#1f8a4c" stroke-width="1.5"/><text x="356.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">deposit (capture)</text>
<rect x="457.0" y="136.5" width="158.0" height="46.0" rx="9" fill="#d9c4a3" opacity="0.22"/><rect x="457.0" y="134.0" width="158.0" height="46.0" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="1.5"/><text x="536.0" y="161.5" text-anchor="middle" font-size="11" font-weight="700" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">(none — nothing held)</text>
<line x1="252.0" y1="216.0" x2="260.0" y2="216.0" stroke="#1f8a4c" stroke-width="2.5" stroke-linecap="round"/><polygon points="268.0,216.0 260.0,220.5 260.0,211.5" fill="#1f8a4c"/>
<text x="348.0" y="212.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#1f8a4c" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">all tried → confirm</text>
<text x="430.0" y="236.0" text-anchor="middle" font-size="10.5" font-weight="600" fill="#b03a2e" font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">any try fails → cancel tried in reverse</text>
</svg>
<figcaption>Try / Confirm / Cancel. Every participant's <strong>Try</strong> reserves; once all have tried, <strong>Confirm</strong> captures; if any Try fails, the engine <strong>Cancels</strong> the already-tried participants in reverse order. The source holds funds on Try and releases them on Cancel; the destination captures on Confirm.</figcaption>
</figure>

Lumen's `src/tcc_transfer.rs` models the transfer as a reserve-then-capture. The
source's try *holds* the funds by debiting now; its confirm is a no-op (the debit
already captured), and its cancel releases the hold with a refund. The
destination's try *verifies* it exists (nothing committed yet, so no cancel); its
confirm captures by crediting:

```rust
use std::sync::Arc;

use firefly::orchestration::TccError;
use serde::{Deserialize, Serialize};

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;
use crate::transfer::{TransferError, TransferRequest};

/// The wire result of a confirmed two-phase transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, firefly::Schema)]
pub struct TccTransferResult {
    /// `"confirmed"` when both participants captured.
    pub status: String,
    pub from: String,
    pub to: String,
    pub amount: i64,
}

/// The two-phase transfer coordinator: each participant drives the `Ledger`.
struct TwoPhaseTransfer {
    ledger: Ledger,
}

#[firefly::tcc(name = "transfer-2pc")]
impl TwoPhaseTransfer {
    /// Source **try**: hold the funds by debiting now (a real `MoneyWithdrawn`).
    #[participant(name = "source", confirm = "capture_source", cancel = "release_source")]
    async fn hold_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .withdraw(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }
    /// Source **confirm**: the debit on try already captured the funds.
    async fn capture_source(&self) -> Result<(), DomainError> {
        Ok(())
    }
    /// Source **cancel**: release the hold by refunding it (a real `MoneyDeposited`).
    async fn release_source(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger
            .deposit(&req.from, Money::cents(req.amount))
            .await?;
        Ok(())
    }

    /// Destination **try**: pre-authorize by verifying the destination exists;
    /// nothing is committed yet, so there is no cancel.
    #[participant(name = "dest", confirm = "capture_dest")]
    async fn hold_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        let events = self.ledger.load_events(&req.to).await?;
        if events.is_empty() {
            return Err(DomainError::NotFound(req.to.clone()));
        }
        Ok(())
    }
    /// Destination **confirm**: capture by crediting the destination.
    async fn capture_dest(&self, #[input] req: TransferRequest) -> Result<(), DomainError> {
        self.ledger.deposit(&req.to, Money::cents(req.amount)).await?;
        Ok(())
    }
}
```

How it reads: the `source` participant names all three phases —
`confirm = "capture_source"`, `cancel = "release_source"` — while `dest` omits
`cancel` because its try holds nothing. The `capture_source` confirm takes only
`&self`: a participant method with no injected parameters is valid, and a no-op
confirm is exactly the right shape when the try already captured.

> **Tip** **Checkpoint.** Compare the source and destination participants. The
> source's try *commits a side-effect* (the withdraw), so it needs a real cancel
> that refunds. The destination's try only *reads* (verifies existence), so it
> holds nothing and needs no cancel. The asymmetry is intentional and is exactly
> why TCC lets you skip a cancel when there is nothing to release.

## Step 10 — Run the TCC and mount it

`run_tcc_transfer` builds the coordinator behind an `Arc` and runs it. On success
both sides captured (`status: "confirmed"`); on any reservation failure the tried
participants are cancelled and the failing phase's cause is rendered out of
`TccError`:

```rust
/// Validates and runs a two-phase transfer. On success both sides captured
/// (`status: "confirmed"`); on any reservation failure the tried participants
/// are cancelled (the source hold released) and this returns
/// `TransferError::Compensated` with the cause.
pub async fn run_tcc_transfer(
    ledger: &Ledger,
    req: &TransferRequest,
) -> Result<TccTransferResult, TransferError> {
    if req.amount <= 0 {
        return Err(TransferError::Invalid("amount must be > 0".into()));
    }
    if req.from == req.to {
        return Err(TransferError::Invalid("from and to must differ".into()));
    }
    let tcc = Arc::new(TwoPhaseTransfer {
        ledger: ledger.clone(),
    });
    match tcc.run(req.clone()).await {
        Ok(()) => Ok(TccTransferResult {
            status: "confirmed".into(),
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount,
        }),
        Err(err) => Err(TransferError::Compensated(tcc_cause(err))),
    }
}

/// Renders the failing phase's cause for the caller.
fn tcc_cause(err: TccError) -> String {
    match err {
        TccError::Try { source, .. } => source.to_string(),
        TccError::Confirm(errors) => errors
            .into_iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; "),
    }
}
```

What just happened: `TccError::Try { source, .. }` carries the reservation that
failed (e.g. an overdrawn source or a missing destination); `tcc_cause` renders
its message. `TccError::Confirm(errors)` collects the confirm-phase failures if a
capture somehow fails after every reservation succeeded — its messages are joined
with `; `. The endpoint mirrors the saga's: `200 OK` with the confirmed result,
or `422` when a reservation failed and the source hold was released.

```rust
/// `POST /api/v1/transfers/2pc` — run a two-phase (Try/Confirm/Cancel) transfer
/// via the TCC coordinator.
#[post(
    "/transfers/2pc",
    summary = "Two-phase transfer (TCC)",
    description = "Runs a Try/Confirm/Cancel two-phase transfer via the TCC coordinator.",
    tags = ["Transfers"],
    status = 200
)]
async fn transfer_2pc(
    State(api): State<WalletApi>,
    Json(body): Json<TransferRequest>,
) -> WebResult<Json<TccTransferResult>> {
    let result = run_tcc_transfer(&api.ledger, &body)
        .await
        .map_err(|e| match e {
            TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
            TransferError::Compensated(detail) => {
                WebError::from(FireflyError::validation(detail))
            }
        })?;
    api.query_cache.invalidate_type::<GetWallet>();
    Ok(Json(result))
}
```

The tests pin the two-phase semantics. A transfer to a missing destination
*holds then releases* the source, leaving it untouched:

```rust
let err = run_tcc_transfer(
    &ledger,
    &TransferRequest { from: src.id.clone(), to: "wlt_missing".into(), amount: 400 },
)
.await
.unwrap_err();
assert!(matches!(err, TransferError::Compensated(_)));
// Source try held the funds, then the dest try failed → source cancel
// released them: the hold + its release net to the original balance.
assert_eq!(balance(&ledger, &src.id).await, 1_000);
```

> **Note** Lumen ships *both* framings of the same transfer so you can compare
> them. The saga applies each leg locally and refunds the debit if the credit
> fails — simplest when an undo is itself a clean local action. The TCC reserves
> on both sides, then commits or releases together — better when a participant can
> cheaply *hold* a reservation and you want all-or-nothing semantics with no
> window where one side has committed and the other has not.

> **Tip** **Checkpoint.** Run `cargo test -p lumen tcc_transfer`. The success
> test moves funds and reports `confirmed`; the missing-destination test holds
> then releases the source so its balance is back to `1_000`; the short-source
> test aborts before holding anything.

## Step 11 — Cancellation

All three engines respect a `CancellationToken` for cooperative cancellation. The
engines depend only on `futures`, so any executor (Tokio included) drives them.
The declarative `run` always honours a token threaded through the context; when
you need to drive it explicitly, the lower-level builder API exposes
`run_cancellable(&token)`. Cancel the token from a timeout or a shutdown signal to
drain the run.

```rust,ignore
// Sketch — the builder seam `#[saga]` lowers onto, for a run you cancel yourself.
let token = firefly::orchestration::CancellationToken::new();
let outcome = saga.run_cancellable(&token).await?;
// elsewhere: token.cancel();  // drains the run cooperatively
```

What just happened: cancellation is *cooperative* — the engine checks the token
before executing the next step, so an in-flight step finishes but no further step
starts. A cancelled run surfaces as `SagaError::Cancelled` (and the equivalents
on `WorkflowError` / `TccError`), not as a step failure.

## Recap — what changed in Lumen

- Lumen now declares its orchestrations with **macros**, not hand-built values.
  The transfer is a `#[firefly::saga(name = "money-transfer")]` `impl` whose
  `debit` step names `compensate = "refund_debit"` and whose `credit` step
  declares `depends_on = ["debit"]`. The macro generates `TransferSaga::saga` and
  `TransferSaga::run`, and `run_transfer` just calls `saga.run(req.clone())`.
- A new **compliance workflow** in `src/compliance.rs`:
  `#[firefly::workflow(name = "transfer-compliance")]` runs `balance-check` and
  `limit-check` in one parallel layer, then `approve` (which `depends_on` both)
  consumes their `bool` verdicts through `#[from_step(...)]`.
- A new **two-phase TCC transfer** in `src/tcc_transfer.rs`:
  `#[firefly::tcc(name = "transfer-2pc")]` with a `source` participant
  (`confirm = "capture_source"`, `cancel = "release_source"`) and a `dest`
  participant (`confirm = "capture_dest"`, no cancel) — reserve all, then confirm
  all or cancel the tried ones.
- All three are mounted on the web surface in `src/web.rs`:
  `POST /api/v1/transfers` (saga), `POST /api/v1/transfers/compliance`
  (workflow), and `POST /api/v1/transfers/2pc` (TCC) — each rendering a clean
  rollback as a `422` RFC 9457 problem and invalidating the `GetWallet` cache
  when it moves funds.
- Because each leg returns its typed `DomainError` / `ComplianceError`, the
  failing cause is preserved through the engine and recovered from
  `SagaError::Step`, `WorkflowError::Node`, and `TccError::Try` — no `Mutex`
  smuggling, no opaque boxed strings.
- The behaviours — happy path, overdraft short-circuit, credit-failure refund
  (three events on the source stream), parallel rejection, and a released TCC
  hold — are all pinned by tests, so the prose can never drift from the code.

You also now know how to **choose an engine**:

| Need                                          | Engine     |
|-----------------------------------------------|------------|
| Dependency-ordered process, undo on failure   | `Saga`     |
| Parallel branches that join                   | `Workflow` |
| Reserve-then-commit across resources          | `Tcc`      |

Lumen's money-transfer is a `Saga` (debit → credit, with a debit refund). Its
pre-flight compliance gate is a `Workflow` (balance + limit checks in parallel,
then approve). And the same transfer reframed as reserve-then-capture is a `Tcc`.
All three are declared the same way — an annotated `impl` block — and all three
are mounted on the web surface.

## Exercises

1. **Add a `notify` step to the saga.** Append a third
   `#[saga_step(id = "notify", depends_on = ["credit"])]` method to
   `TransferSaga` that "sends a receipt" (return `Ok(())` for now). Assert that on
   the happy path `steps_executed == ["debit", "credit", "notify"]`, and that
   when the credit fails the notify step never runs and only the debit is rolled
   back.
2. **Make the debit retry.** Give the `debit` step
   `#[saga_step(id = "debit", compensate = "refund_debit", retry = 2, backoff_ms = 50)]`.
   Drive a flaky `Ledger` that fails the first withdraw and succeeds the second,
   and assert the transfer still completes — proving the per-step retry recovers a
   transient failure before compensation is ever considered.
3. **Add a KYC node to the workflow.** Add a third independent
   `#[workflow_step(id = "kyc-check")]` to `ComplianceCheck` that returns a
   `bool`, and make `approve` `depends_on` all three, reading the new verdict via
   `#[from_step("kyc-check")]`. Assert that `kyc-check` runs in the same parallel
   layer as the existing checks and that a failed KYC rejects the transfer.
4. **Confirm the TCC is all-or-nothing.** Write a test that runs
   `run_tcc_transfer` with an overdrawn source and asserts neither balance moved —
   the source try aborts before holding anything, so there is nothing to cancel.
   Contrast it with the missing-destination test, where the source *is* held and
   then released.
5. **Switch the saga's compensation policy.** Change the saga attribute to
   `#[firefly::saga(name = "money-transfer", policy = "stop_on_error")]` and read
   the docs for `SagaError::Compensation`. Reason about (or test) what the
   `run_transfer` error branch would surface if a *compensation* itself failed —
   and why `best_effort` is the engine's default.

## Where to go next

- To call the external services these engines coordinate — a Payments processor,
  an FX provider — you need an HTTP client. Continue to
  **[HTTP Clients](./13-http-clients.md)**.
- The transfer endpoints invalidate the `GetWallet` cache on every move; learn how
  that read-side cache and its invalidation work in **[Caching](./17-caching.md)**.
- Revisit the event-sourced `Ledger` every leg drives in
  **[Event Sourcing](./11-event-sourcing.md)** to see where the
  `MoneyWithdrawn` / `MoneyDeposited` events these sagas raise come from.
