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
typed context blackboard, and respects cooperative cancellation. And — this is
the change since the last edition — you no longer hand-build them as values.
Lumen declares each engine with an attribute macro on an `impl` block, exactly
as it declares CQRS handlers and controllers.

| Engine     | Topology                   | Compensation                       | Declared with                |
|------------|----------------------------|------------------------------------|------------------------------|
| `Saga`     | Dependency-ordered steps   | Reverse-order, configurable policy | `#[saga]` + `#[saga_step]`   |
| `Workflow` | DAG with parallel layers   | Reverse-order, configurable policy | `#[workflow]` + `#[workflow_step]` |
| `Tcc`      | Try-all then Confirm-all   | Cancel-tried on a Try failure      | `#[tcc]` + `#[participant]`   |

> **Design note.** Firefly's orchestration model is *declarative*. You write an
> ordinary `impl` block of `async fn(&self, …) -> Result<T, E>` methods and
> annotate them: `#[saga_step]` for a saga leg, `#[workflow_step]` for a DAG
> node, `#[participant]` for a TCC actor. The macro lowers those methods onto the
> same `firefly-orchestration` engines — `depends_on` orders them, `compensate`
> names the undo, a step's `Ok(T)` is published for later steps, and an `Err(E)`
> triggers compensation in reverse order. If you've used Java's `@Saga` or
> pyfly's saga decorators, this is the Rust spelling: the control flow lives in
> methods you can read top to bottom, and the wiring is generated for you.

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

<figure>
<svg viewBox="0 0 640 200" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="A two-step saga: debit then credit, with a compensation arrow from credit failure back to a refund of the debit.">
  <rect x="0" y="0" width="640" height="200" fill="#ffffff"/>
  <!-- debit node -->
  <rect x="48" y="48" width="150" height="56" rx="10" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="123" y="72" text-anchor="middle" font-family="sans-serif" font-size="15" font-weight="bold" fill="#3a2a1c">debit</text>
  <text x="123" y="92" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#3a2a1c">withdraw(amount)</text>
  <!-- forward arrow -->
  <line x1="198" y1="76" x2="278" y2="76" stroke="#d4793a" stroke-width="2.5" marker-end="url(#arrow)"/>
  <!-- credit node -->
  <rect x="278" y="48" width="150" height="56" rx="10" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="353" y="72" text-anchor="middle" font-family="sans-serif" font-size="15" font-weight="bold" fill="#3a2a1c">credit</text>
  <text x="353" y="92" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#3a2a1c">deposit(amount)</text>
  <text x="486" y="80" text-anchor="middle" font-family="sans-serif" font-size="13" fill="#b03a2e">fails</text>
  <!-- compensation arrow: from credit, curving back under debit -->
  <path d="M 353 104 C 353 160, 123 160, 123 104" fill="none" stroke="#b03a2e" stroke-width="2.5" stroke-dasharray="7 5" marker-end="url(#arrowred)"/>
  <text x="238" y="178" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#b03a2e">compensate: refund_debit — deposit(amount) back to source</text>
  <defs>
    <marker id="arrow" markerWidth="9" markerHeight="9" refX="7" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#d4793a"/></marker>
    <marker id="arrowred" markerWidth="9" markerHeight="9" refX="7" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#b03a2e"/></marker>
  </defs>
</svg>
<figcaption>The transfer saga: <code>debit</code> then <code>credit</code>. A failed credit runs the debit's compensation in reverse, refunding the source.</figcaption>
</figure>

## Saga — dependency-ordered steps with compensation

A `Saga` runs steps in dependency order; on any step failure it compensates the
completed steps in reverse order. You declare it with `#[firefly::saga]` on an
`impl` block, marking each leg with `#[saga_step]`:

```rust
#[firefly::saga(name = "money-transfer", policy = "stop_on_error")]
impl TransferSaga {
    #[saga_step(id = "reserve", compensate = "refund")]
    async fn reserve(&self, #[input] req: TransferReq) -> Result<Reserved, MyErr> { /* … */ }

    async fn refund(&self, #[from_step("reserve")] r: Reserved) -> Result<(), MyErr> { /* … */ }

    #[saga_step(id = "credit", depends_on = ["reserve"], retry = 3, backoff_ms = 100)]
    async fn credit(&self, #[from_step("reserve")] r: Reserved) -> Result<(), MyErr> { /* … */ }
}
```

The macro generates two methods on the type:

- `TransferSaga::saga(self: Arc<Self>) -> Saga` — builds the engine from your
  steps, their `depends_on` order, compensations, and retry policies.
- `TransferSaga::run(self: Arc<Self>, input) -> Result<Outcome, SagaFailure>` —
  serialises `input` into the step context and runs the whole DAG, compensating
  on failure.

Each step is an `async fn(&self, …) -> Result<T, E>`. Its **parameters are
injected** from the saga context with markers the macro reads and strips:

- `#[input]` (the whole input) or `#[input("field")]` (one field of it);
- `#[from_step("id")]` — the `Ok` value an earlier step published;
- `#[variable("key")]` — a saga-scoped context variable;
- `#[ctx]` — the `StepContext` blackboard itself.

A step's `Ok(T)` is serialised and made available to later steps via
`#[from_step]`; an `Err(E)` (where `E: std::error::Error + Send + Sync`)
triggers compensation in reverse order. `#[saga_step]` accepts `id` (required),
`depends_on = ["…"]`, `compensate = "method"`, and the per-step recovery knobs
`retry`, `backoff_ms`, `timeout_ms`, and `jitter`. `#[saga(...)]` accepts a
`name`, a `crate` facade override, and a compensation `policy`:

- **`best_effort`** (the engine default) — log and continue compensating the
  remaining steps even if one compensation fails.
- **`stop_on_error`** — abort rollback at the first compensation failure and
  surface a `SagaError::Compensation` wrapping the original.
- plus `retry_with_backoff`, `circuit_breaker`, `best_effort_parallel`, and
  `grouped_parallel` for larger fan-outs.

> **Spring parity.** This is the same shape as Java's `@Saga` / `@SagaStep` and
> pyfly's saga decorators — a step method, a compensation named by string, a
> `depends_on` order — but lowered onto Rust's type system, so a step that
> returns the wrong type or names a compensation that does not exist is a
> *compile error*, not a runtime surprise.

> **Note** — the macro lowers onto a lower-level programmatic builder. If you
> ever need to construct a saga dynamically (steps known only at run time), the
> same engine exposes `Saga::new(name).step(Step::with_context(id, action)
> .with_context_compensation(undo))`. The declarative `#[saga]` form is the way
> you'll write them in practice; the builder is the seam it expands onto.

## Lumen's transfer saga

Lumen's transfer is a two-step saga: `debit` the source, then `credit` the
destination. The debit's compensation refunds the source; the credit is the last
leg, so it needs no compensation — a failure there rolls back only the debit. The
whole thing lives in `src/transfer.rs`, and every leg drives the same `Ledger`
the CQRS handlers use.

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

### Declaring the saga

The saga is an `impl` block on a tiny struct that holds the `Ledger`. Each leg
is an annotated method that calls the ledger directly and returns a typed
`Result<(), DomainError>`. There is no closure capture, no `Mutex` to smuggle
the cause out of an erased error channel, no builder call — the macro reads the
attributes and generates all of it:

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

**How it reads.** `debit` is the first step (`id = "debit"`), and it names its
undo with `compensate = "refund_debit"`. `refund_debit` carries *no*
`#[saga_step]` marker — it is a plain method referenced by name, and the macro
includes it in the generated saga only because `debit` points at it. `credit`
declares `depends_on = ["debit"]`, so the engine runs it strictly after the
debit; it has no compensation because it is the last leg, so the only thing to
undo on its failure is the debit, which the engine handles automatically.

Each leg takes `#[input] req: TransferRequest`, so the macro deserialises the
saga's input (the request) into that parameter on every step and compensation.
Because the methods return `DomainError` directly, the typed failure cause is
preserved all the way through the engine — no shared `Mutex` needed.

### Running the saga

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

**What the generated `run` does for you.** `saga.run(req.clone())` is the macro's
`TransferSaga::run(self: Arc<Self>, input)`: it serialises `req` into a fresh
`StepContext`, builds the saga (`debit` → `credit`, with the debit's
compensation attached), and runs the DAG. On the happy path it returns an
`Outcome` whose `status` is `Completed`, `steps_executed` lists the legs that
ran, and `steps_rolled` is empty. On failure it returns a `SagaFailure`: its
`outcome()` is fully populated (status `Compensated`, with `steps_rolled` naming
the compensations that ran), and its `error()` is a `SagaError`. We match
`SagaError::Step { source, .. }` to recover the leg's `DomainError` message —
that is how `POST /api/v1/transfers` answers `insufficient funds` instead of an
opaque "step \"credit\" failed".

> **Design note.** Nothing here is reflection or runtime scanning. `#[saga]`
> expands at compile time into the exact `Saga::new(...).step(...)` calls you
> would otherwise write by hand, threaded through the `firefly` facade's `__rt`
> contract so a one-dependency service compiles it without ever naming
> `firefly-orchestration`. A step that returns the wrong type, or a `compensate`
> that points at a method that doesn't exist, fails the build with a message
> pointing at the offending line.

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

## Workflow — a DAG with parallel layers and conditions

When a process has *independent* steps that should run in parallel, reach for a
`Workflow`: a DAG of nodes with `depends_on` declarations. Independent nodes run
concurrently within a topological layer, and a node that declares dependencies
runs only after they complete. You declare it with `#[firefly::workflow]` and
mark each node with `#[workflow_step]` — the same parameter injection as a saga.

`#[workflow_step]` accepts `id` (required), `depends_on = ["…"]`,
`compensate = "method"`, `when = "expr"` (a skip condition — the node is skipped
when the predicate is false), and `fire_and_forget` (schedule the node without
blocking the layer). The macro generates `Workflow::workflow(self: Arc<Self>)`
and `run(self, input) -> Result<(), WorkflowError>`.

<figure>
<svg viewBox="0 0 640 220" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="A compliance DAG: balance-check and limit-check run in parallel and both feed the approve node.">
  <rect x="0" y="0" width="640" height="220" fill="#ffffff"/>
  <!-- balance-check -->
  <rect x="40" y="36" width="180" height="52" rx="10" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="130" y="58" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#3a2a1c">balance-check</text>
  <text x="130" y="76" text-anchor="middle" font-family="sans-serif" font-size="11" fill="#3a2a1c">funds_ok: bool</text>
  <!-- limit-check -->
  <rect x="40" y="132" width="180" height="52" rx="10" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="130" y="154" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#3a2a1c">limit-check</text>
  <text x="130" y="172" text-anchor="middle" font-family="sans-serif" font-size="11" fill="#3a2a1c">within_limit: bool</text>
  <!-- approve -->
  <rect x="420" y="84" width="180" height="52" rx="10" fill="#d4793a" stroke="#3a2a1c" stroke-width="2"/>
  <text x="510" y="106" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#ffffff">approve</text>
  <text x="510" y="124" text-anchor="middle" font-family="sans-serif" font-size="11" fill="#fff3e0">depends_on both</text>
  <!-- edges -->
  <path d="M 220 62 C 320 62, 330 100, 418 104" fill="none" stroke="#d4793a" stroke-width="2.5" marker-end="url(#wfarrow)"/>
  <path d="M 220 158 C 320 158, 330 120, 418 116" fill="none" stroke="#d4793a" stroke-width="2.5" marker-end="url(#wfarrow)"/>
  <text x="300" y="28" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#8a6d3b">parallel layer</text>
  <defs>
    <marker id="wfarrow" markerWidth="9" markerHeight="9" refX="7" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#d4793a"/></marker>
  </defs>
</svg>
<figcaption>The compliance workflow: <code>balance-check</code> and <code>limit-check</code> have no dependency on each other, so they run in the same layer; <code>approve</code> waits for both and consumes their verdicts.</figcaption>
</figure>

### Lumen's compliance workflow

A large transfer should be gated behind compliance checks *before* the money
moves. Lumen's `src/compliance.rs` runs two independent checks in parallel and
then an approval gate that consumes both. `balance-check` and `limit-check` have
no dependency on each other, so the engine runs them in the same topological
layer; `approve` declares `depends_on` on both and reads their boolean verdicts
through `#[from_step(...)]`:

```rust
use std::sync::Arc;

use firefly::orchestration::WorkflowError;

use crate::domain::Wallet;
use crate::ledger::Ledger;
use crate::transfer::TransferRequest;

/// The per-transfer ceiling, in minor units (cents).
pub const MAX_TRANSFER_CENTS: i64 = 1_000_000; // 10,000.00

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

This is the payoff of the injection model. `balance_check` returns `Ok(true)` or
`Ok(false)`; the engine serialises that `bool` under the node id `balance-check`.
`approve` declares `#[from_step("balance-check")] funds_ok: bool`, and the macro
deserialises the stored value back into that parameter — typed both ends, with
no manual context plumbing. `balance-check` reads the *real* source aggregate
from the `Ledger`; only the per-transfer ceiling is a new policy input.

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

The endpoint in `src/web.rs` is a read-only pre-check that never moves funds —
`200 OK` with the decision when approved, `404` when the source wallet is
unknown, and `422` carrying the reason when a compliance check rejects:

```rust
/// `POST /api/v1/transfers/compliance` — gate a transfer through the parallel
/// compliance workflow (balance + limit checks → approve).
#[post("/transfers/compliance")]
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

> **Where Lumen would grow this.** The experience-tier starter,
> `firefly-starter-experience`, builds on exactly this workflow engine with
> *signal-driven* steps that park until an external caller delivers a named
> signal, then resume from where they left off. We return to that tier in
> [HTTP Clients](./13-http-clients.md).

## TCC — reserve all, then confirm all or cancel all

`Tcc` runs a two-phase protocol: **Try** every participant (reserve resources),
then **Confirm** all on success; on any Try failure, **Cancel** the participants
already tried, in reverse order. Where a saga applies each leg immediately and
*undoes* a committed leg on failure, TCC reserves first and only commits once
every reservation succeeded — so a failed reservation is cancelled, never
compensated after the fact.

You declare it with `#[firefly::tcc]` and mark each *try* method with
`#[participant(name, confirm, cancel)]`. The confirm and cancel methods are plain
`async fn` referenced by name. A participant's try result is published under its
name, so confirm and cancel can read it via `#[from_step("<name>")]`.
`#[participant]` accepts `name` and `confirm` (required), plus `cancel`, `retry`,
`backoff_ms`, and `timeout_ms`. The macro generates `Tcc::tcc(self: Arc<Self>)`
and `run(self, input) -> Result<(), TccError>`.

<figure>
<svg viewBox="0 0 640 250" xmlns="http://www.w3.org/2000/svg" role="img" aria-label="TCC phases for two participants: a Try column, a Confirm column on success, and a Cancel column on failure.">
  <rect x="0" y="0" width="640" height="250" fill="#ffffff"/>
  <!-- column headers -->
  <text x="120" y="28" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#3a2a1c">Try (reserve)</text>
  <text x="330" y="28" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#2e7d32">Confirm (on success)</text>
  <text x="540" y="28" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="bold" fill="#b03a2e">Cancel (on try failure)</text>
  <!-- source row -->
  <text x="14" y="80" font-family="sans-serif" font-size="12" font-weight="bold" fill="#8a6d3b">source</text>
  <rect x="40" y="56" width="160" height="44" rx="9" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="120" y="83" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#3a2a1c">withdraw (hold)</text>
  <rect x="250" y="56" width="160" height="44" rx="9" fill="#e8f5e9" stroke="#2e7d32" stroke-width="2"/>
  <text x="330" y="83" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#2e7d32">(none — held)</text>
  <rect x="460" y="56" width="160" height="44" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="2"/>
  <text x="540" y="83" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#b03a2e">deposit (release)</text>
  <!-- dest row -->
  <text x="14" y="154" font-family="sans-serif" font-size="12" font-weight="bold" fill="#8a6d3b">dest</text>
  <rect x="40" y="130" width="160" height="44" rx="9" fill="#f6a821" stroke="#d4793a" stroke-width="2"/>
  <text x="120" y="157" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#3a2a1c">verify exists</text>
  <rect x="250" y="130" width="160" height="44" rx="9" fill="#e8f5e9" stroke="#2e7d32" stroke-width="2"/>
  <text x="330" y="157" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#2e7d32">deposit (capture)</text>
  <rect x="460" y="130" width="160" height="44" rx="9" fill="#fdecea" stroke="#b03a2e" stroke-width="2"/>
  <text x="540" y="157" text-anchor="middle" font-family="sans-serif" font-size="12" fill="#b03a2e">(none — nothing held)</text>
  <!-- flow note -->
  <line x1="200" y1="210" x2="250" y2="210" stroke="#2e7d32" stroke-width="2.5" marker-end="url(#okarrow)"/>
  <text x="225" y="202" text-anchor="middle" font-family="sans-serif" font-size="11" fill="#2e7d32">all tried</text>
  <line x1="200" y1="232" x2="250" y2="232" stroke="#b03a2e" stroke-width="2.5" stroke-dasharray="6 4" marker-end="url(#failarrow)"/>
  <text x="372" y="236" text-anchor="middle" font-family="sans-serif" font-size="11" fill="#b03a2e">any try fails → cancel tried in reverse</text>
  <defs>
    <marker id="okarrow" markerWidth="9" markerHeight="9" refX="7" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#2e7d32"/></marker>
    <marker id="failarrow" markerWidth="9" markerHeight="9" refX="7" refY="4.5" orient="auto"><path d="M0,0 L9,4.5 L0,9 z" fill="#b03a2e"/></marker>
  </defs>
</svg>
<figcaption>The two-phase transfer: Try holds on the source and verifies the destination; Confirm captures on the destination; a failed Try cancels by releasing the source hold.</figcaption>
</figure>

### Lumen's two-phase transfer

Lumen's `src/tcc_transfer.rs` models the transfer as a reserve-then-capture. The
source's try *holds* the funds by debiting now; its confirm is a no-op (the debit
already captured), and its cancel releases the hold with a refund. The
destination's try *verifies* it exists (nothing committed yet, so no cancel); its
confirm captures by crediting:

```rust
use std::sync::Arc;

use firefly::orchestration::TccError;

use crate::domain::DomainError;
use crate::ledger::Ledger;
use crate::money::Money;
use crate::transfer::{TransferError, TransferRequest};

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

The `source` participant names all three phases — `confirm = "capture_source"`,
`cancel = "release_source"` — while `dest` omits `cancel` because its try holds
nothing. The `capture_source` confirm takes only `&self`: a participant method
with no injected parameters is valid, and a no-op confirm is exactly the right
shape when the try already captured.

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

`TccError::Try { source, .. }` carries the reservation that failed (e.g. an
overdrawn source or a missing destination); `TccError::Confirm(errors)` collects
the confirm-phase failures if a capture somehow fails after every reservation
succeeded. The endpoint mirrors the saga's: `200 OK` with the confirmed result,
or `422` when a reservation failed and the source hold was released.

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

> **TCC vs. Saga.** Lumen ships *both* framings of the same transfer so you can
> compare them. The saga applies each leg locally and refunds the debit if the
> credit fails — simplest when an undo is itself a clean local action. The TCC
> reserves on both sides, then commits or releases together — better when a
> participant can cheaply *hold* a reservation and you want all-or-nothing
> semantics with no window where one side has committed and the other has not.

## Cancellation

All three engines respect a `CancellationToken` for cooperative cancellation.
The engines depend only on `futures`, so any executor (Tokio included) drives
them. When you need it, the lower-level builder API exposes
`run_cancellable(&token)`; cancel the token from a timeout or a shutdown signal
to drain the run.

## Choosing an engine

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

## What changed in Lumen

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
  rollback as a `422` business problem and invalidating the `GetWallet` cache
  when it moves funds.
- Because each leg returns its typed `DomainError` / `ComplianceError`, the
  failing cause is preserved through the engine and recovered from
  `SagaError::Step`, `WorkflowError::Node`, and `TccError::Try` — no `Mutex`
  smuggling, no opaque boxed strings.
- The behaviors — happy path, overdraft short-circuit, credit-failure refund
  (three events on the source stream), parallel rejection, and a released TCC
  hold — are all pinned by tests, so the prose can never drift from the code.

## Exercises

1. **Add a `notify` step to the saga.** Append a third `#[saga_step(id =
   "notify", depends_on = ["credit"])]` method to `TransferSaga` that "sends a
   receipt" (return `Ok(())` for now). Assert that on the happy path
   `steps_executed == ["debit", "credit", "notify"]`, and that when the credit
   fails the notify step never runs and only the debit is rolled back.

2. **Make the debit retry.** Give the `debit` step
   `#[saga_step(id = "debit", compensate = "refund_debit", retry = 2,
   backoff_ms = 50)]`. Drive a flaky `Ledger` that fails the first withdraw and
   succeeds the second, and assert the transfer still completes — proving the
   per-step retry recovers a transient failure before compensation is ever
   considered.

3. **Add a KYC node to the workflow.** Add a third independent
   `#[workflow_step(id = "kyc-check")]` to `ComplianceCheck` that returns a
   `bool`, and make `approve` `depends_on` all three, reading the new verdict via
   `#[from_step("kyc-check")]`. Assert that `kyc-check` runs in the same parallel
   layer as the existing checks and that a failed KYC rejects the transfer.

4. **Confirm the TCC is all-or-nothing.** Write a test that runs
   `run_tcc_transfer` with an overdrawn source and asserts neither balance moved
   — the source try aborts before holding anything, so there is nothing to
   cancel. Contrast it with the missing-destination test, where the source *is*
   held and then released.

To call the external services these engines coordinate — a Payments processor,
an FX provider — you need an HTTP client. Continue to
[HTTP Clients](./13-http-clients.md).
