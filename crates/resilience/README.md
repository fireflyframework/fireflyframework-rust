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
Closed   → (failures ≥ threshold, or windowed failure rate ≥ rate) → Open
Open     → (after open_duration)              → HalfOpen
HalfOpen → (half_open_max_calls successes)    → Closed
HalfOpen → any failure                        → Open
```

A zero `window` counts consecutive failures only (any success resets); a
non-zero `window` counts failures within a rolling time window. While
`HalfOpen`, up to `half_open_max_calls` trial calls are allowed (default
1 — the historical single-trial behavior); excess calls are gated with
`CircuitOpen` until the trials settle.

### Failure-rate window mode (pyfly parity)

Setting `failure_rate_threshold` switches the breaker from
consecutive-failure counting to pyfly's count-based failure-rate window
(Resilience4j `COUNT_BASED`): the outcomes of the last `window_size`
calls are kept in a ring buffer, and the breaker opens once the window
is **full** and the failure fraction reaches the threshold.

```rust,ignore
let cb = CircuitBreaker::new(CircuitConfig {
    failure_rate_threshold: Some(0.5), // open at ≥ 50% failures…
    window_size: 10,                   // …over the last 10 calls (full window required)
    half_open_max_calls: 2,            // 2 successful probes close the circuit
    ..CircuitConfig::default()
});
```

For manual instrumentation the breaker also exposes pyfly's hooks:

```rust,ignore
cb.before_call()?;  // Err(CircuitOpen) while open / probe budget spent
match do_call().await {
    Ok(v) => { cb.on_success(); /* … */ }
    Err(e) => { cb.on_failure(); /* … */ }
}
```

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

## pyfly parity

The crate also ports pyfly's `pyfly.resilience` extensions:

### Fallback

`Fallback` is the graceful-degradation decorator (pyfly's `@fallback`).
It forwards successes untouched; when the inner operation — or an inner
decorator's short-circuit sentinel — fails with an error matched by the
`on` predicate, the handler runs and its result replaces the
operation's. `Chain` operations are unit-valued, so pyfly's static
`fallback_value` becomes `Fallback::recover()` ("swallow and `Ok(())`");
for value-returning calls plain `Result` combinators remain idiomatic.

```rust,ignore
let chain = Chain::new()
    .with(Fallback::new(|err| { serve_cached(); Ok(()) })  // fallback_method
        .on(ResilienceError::is_timeout))                  // pyfly on=(...)
    .with(Timeout::new(Duration::from_secs(2)));
// async handlers: Fallback::new_async(|err| async move { ... })
```

### ResilienceRegistry

`ResilienceRegistry` materialises named breakers / limiters / bulkheads /
time-limiters from `firefly.resilience.*` configuration keys (pyfly's
`ResilienceRegistry.from_config`), consuming the flat dot-keyed map that
`firefly-config` sources produce:

```yaml
firefly:
  resilience:
    circuit-breaker:
      payment-api:
        failure-threshold: 3        # default 5
        recovery-timeout: 10s       # default 30s
        failure-rate-threshold: 0.5 # optional → count-based window mode
        window-size: 8              # default 10
        half-open-max-calls: 2      # default 1
    rate-limiter:
      search-api: { max-tokens: 200, refill-rate: 100.0 } # defaults 10 / 10.0
    bulkhead:
      db-pool: { max-concurrent: 5 }                      # default 10
    time-limiter:
      slow-report: { timeout: 30s }                       # default 30s
```

```rust,ignore
let registry = ResilienceRegistry::from_config(&layered)?; // or from_sources / from_map
let cb = registry.circuit_breaker("payment-api")?;  // Arc<CircuitBreaker>
let rl = registry.rate_limiter("search-api")?;      // Arc<RateLimiter>
let bh = registry.bulkhead("db-pool")?;             // Arc<Bulkhead>
let d  = registry.time_limiter("slow-report")?;     // Duration
let to = registry.timeout("slow-report")?;          // ready-made Timeout decorator
```

Unknown names return `RegistryError::NotFound` with pyfly's `KeyError`
message (`No bulkhead named 'x'. Available: ['alpha', 'beta']`).
Kebab-case and snake_case bind interchangeably (Spring relaxed binding,
matching `firefly-config`'s merge normalization), for sections,
properties, and instance names alike. Durations accept pyfly forms
(`"5s"`, `"500ms"`, `"1m"`, `"2h"`, bare seconds `"2.5"`) plus anything
`humantime` parses (`"1h 30m"`); the parser is exported as
`parse_duration`.

## Public surface

```rust,ignore
pub enum CircuitState { Closed, Open, HalfOpen } // Display: "closed" | "open" | "half-open"
pub struct CircuitConfig {
    pub failure_threshold: usize,             // 0 → 5
    pub window: Duration,                     // ZERO = consecutive-only
    pub open_duration: Duration,              // ZERO → 30 s
    pub now: Option<Clock>,                   // None → Instant::now
    pub failure_rate_threshold: Option<f64>,  // Some(r) → count-based window mode (pyfly)
    pub window_size: usize,                   // 0 → 10 (ring-buffer length)
    pub half_open_max_calls: usize,           // 0 → 1 (probe budget)
}
impl Default for CircuitConfig;    // 5 failures / 1 s window / 30 s open / consecutive mode
impl CircuitBreaker {
    pub fn new(cfg: CircuitConfig) -> Self;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub fn state(&self) -> CircuitState;
    // pyfly manual hooks
    pub fn before_call(&self) -> Result<(), ResilienceError>;
    pub fn on_success(&self);
    pub fn on_failure(&self);
    // effective-config getters
    pub fn failure_threshold(&self) -> usize;
    pub fn open_duration(&self) -> Duration;
    pub fn failure_rate_threshold(&self) -> Option<f64>;
    pub fn window_size(&self) -> usize;
    pub fn half_open_max_calls(&self) -> usize;
}

impl RateLimiter {
    pub fn new(rate: f64, burst: usize) -> Self;
    pub fn rate(&self) -> f64;     // pyfly refill_rate
    pub fn burst(&self) -> usize;  // pyfly max_tokens
    pub fn allow(&self) -> bool;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub async fn wait(&self);
}

impl Bulkhead {
    pub fn new(max_concurrent: usize) -> Self;
    pub fn max_concurrent(&self) -> usize;
    pub fn available_slots(&self) -> usize;
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

pub struct Fallback;               // ::recover() | ::new(h) | ::new_async(h), .on(pred)
impl Decorator for Fallback;

pub struct ResilienceRegistry;     // ::from_config(&Layered) | ::from_sources(v) | ::from_map(&flat)
                                   // .circuit_breaker(n) .rate_limiter(n) .bulkhead(n)
                                   // .time_limiter(n) .timeout(n) .register_*(n, v) .*_names()
pub fn parse_duration(raw: &str) -> Result<Duration, RegistryError>;
pub enum RegistryError { NotFound, InvalidDuration, InvalidValue, Config(ConfigError) }

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

The pyfly-parity layer ports pyfly's test suites: failure-rate window
trip/partial-window/sliding semantics and the half-open probe budget
(`tests/resilience/test_resilience_tuning.py`), registry duration
parsing, config materialisation, relaxed binding, unknown-name errors,
and direct construction (`test_resilience_registry.py`), and the
fallback behaviors — success bypass, handler receives the error,
predicate filtering, async handlers (`test_fallback.py`).
