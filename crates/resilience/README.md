# `firefly-resilience`

> **Tier:** Platform · **Status:** Full · **Java original:** Resilience4j · **Go module:** `resilience`

## Overview

`firefly-resilience` provides **Resilience4j-equivalent decorators** that
compose around any async Rust operation:

| Primitive        | Failure mode it shields against              | Error variant                          |
|------------------|----------------------------------------------|----------------------------------------|
| `CircuitBreaker` | Cascading failure of a slow / failing dep    | `ResilienceError::CircuitOpen`         |
| `RateLimiter`    | Outbound rate cap (token bucket)             | `ResilienceError::RateLimited`         |
| `Bulkhead`       | Resource exhaustion via runaway concurrency  | `ResilienceError::BulkheadFull` (or block) |
| `Timeout`        | Stuck calls                                  | `ResilienceError::Timeout`             |

`Chain` composes them into a single guarded call. Error messages are
byte-identical to the Go port's sentinels (`firefly/resilience: circuit
open`, …) so logs and dashboards stay consistent across the sibling
framework ports. Where Go threads a `context.Context` through every
call for cancellation, the Rust analogue is dropping the future.

## Mental model — the canonical guarded call

```
   Chain::new().with(timeout).with(breaker).with(bulkhead).execute(call)

                      ┌────────────┐
                      │  Timeout   │  per-call deadline
                      └────────────┘
                            │
                            ▼
                      ┌────────────┐
                      │ Breaker    │  short-circuit when downstream is sick
                      └────────────┘
                            │
                            ▼
                      ┌────────────┐
                      │ Bulkhead   │  cap concurrent in-flight calls
                      └────────────┘
                            │
                            ▼
                          call
```

## Quick start

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly_resilience::{
    Bulkhead, Chain, CircuitBreaker, CircuitConfig, ResilienceError, Timeout,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ResilienceError> {
    let breaker = Arc::new(CircuitBreaker::new(CircuitConfig::default()));

    let guarded = Chain::new()
        .with(Timeout::new(Duration::from_secs(2)))   // per-call deadline
        .with_shared(breaker.clone())                 // short-circuit when sick
        .with(Bulkhead::new(20));                     // cap in-flight calls

    guarded.execute(|| async { Ok(()) }).await?;
    assert_eq!(breaker.state().to_string(), "closed");
    Ok(())
}
```

## CircuitBreaker

```rust,ignore
let cb = CircuitBreaker::new(CircuitConfig {
    failure_threshold: 5,
    window: Duration::from_secs(1),
    open_duration: Duration::from_secs(30),
    now: None, // injectable clock for tests
});

let result = cb.execute(|| async { charge(amount).await }).await;
// matches!(err, ResilienceError::CircuitOpen) when the breaker is open
// — or err.is_circuit_open(), the analogue of errors.Is(err, ErrCircuitOpen)
```

State machine:
```
Closed   → (failures ≥ threshold) → Open
Open     → (after open_duration)  → HalfOpen
HalfOpen → success                → Closed
HalfOpen → failure                → Open
```

A zero `window` counts consecutive failures only (any success resets); a
non-zero `window` counts failures within a rolling window. While
`HalfOpen`, exactly one trial call is allowed — concurrent calls are
gated with `CircuitOpen` until the trial settles.

## RateLimiter

Token-bucket — `burst` tokens accumulate at `rate` per second.

```rust,ignore
let rl = RateLimiter::new(100.0, 200); // 100 rps, burst 200
match rl.execute(|| async { call().await }).await {
    Err(err) if err.is_rate_limited() => { /* back-pressure */ }
    other => { /* ... */ }
}
// or block until a token frees up:
rl.wait().await;
// or poll without consuming the call:
if rl.allow() { /* token consumed */ }
```

## Bulkhead

Semaphore-based concurrency cap.

```rust,ignore
let bh = Bulkhead::new(20);
bh.execute(|| async { call().await }).await?;     // waits until a slot frees
bh.try_execute(|| async { call().await }).await?; // non-blocking; BulkheadFull if full
```

## Timeout

Per-call deadline. Where the Go port runs `fn` in its own goroutine
(leaving it running after the deadline), the Rust port cancels the
operation's future outright — the caller-visible contract is identical.

```rust,ignore
let to = Timeout::new(Duration::from_secs(2));
let result = to.execute(|| async { slow_call().await }).await;
// err.is_timeout() on budget exceeded
```

## Chain

```rust,ignore
let guarded = Chain::new()
    .with(Timeout::new(Duration::from_secs(2)))
    .with_shared(breaker.clone()) // keep the Arc for state inspection
    .with(Bulkhead::new(20));
let result = guarded.execute(|| async { charge(amount).await }).await;
```

`Chain` runs decorators left-to-right (leftmost = outermost). The
`Decorator` trait replaces both Go's `Decorator` func type and its
`AsDecorator` adapter: the four primitives — and `Chain` itself, so
chains nest — implement it directly, and `from_fn` adapts plain
functions of shape `for<'a> Fn(Operation<'a>) -> OpFuture<'a>`.

## Public surface

```rust,ignore
pub enum CircuitState { Closed, Open, HalfOpen } // Display: "closed" | "open" | "half-open"
pub struct CircuitConfig {
    pub failure_threshold: usize,  // 0 → 5
    pub window: Duration,          // ZERO = consecutive-only
    pub open_duration: Duration,   // ZERO → 30 s
    pub now: Option<Clock>,        // None → Instant::now
}
impl Default for CircuitConfig;    // 5 failures / 1 s window / 30 s open
impl CircuitBreaker {
    pub fn new(cfg: CircuitConfig) -> Self;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub fn state(&self) -> CircuitState;
}

impl RateLimiter {
    pub fn new(rate: f64, burst: usize) -> Self;
    pub fn allow(&self) -> bool;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub async fn wait(&self);
}

impl Bulkhead {
    pub fn new(max_concurrent: usize) -> Self;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub async fn try_execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
}

impl Timeout {
    pub fn new(budget: Duration) -> Self;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
}

#[async_trait]
pub trait Decorator: Send + Sync {
    async fn call(&self, op: Operation<'_>) -> Result<(), ResilienceError>;
}
pub struct Chain;                  // ::new().with(d).with_shared(arc).execute(op)
pub fn from_fn(f) -> FnDecorator;  // adapt a plain function
pub fn operation(f) -> Operation;  // box an async closure

pub enum ResilienceError { CircuitOpen, RateLimited, BulkheadFull, Timeout, Operation(BoxError) }
```

## Testing

```bash
cargo test -p firefly-resilience
```

Covers breaker trip + half-open trial + recovery (with a fake clock),
half-open re-open and single-trial gating, rolling-window pruning,
rate-limiter allow + wait semantics + burst capping, bulkhead concurrent
cap, timeout cancellation, and chain ordering (verifies leftmost runs
outermost) — plus Rust-specific cases: Go-identical sentinel messages,
`Send`/`Sync` bounds, and nested chains.
