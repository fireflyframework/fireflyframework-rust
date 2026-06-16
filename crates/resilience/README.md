# `firefly-resilience`

> **Tier:** Platform · **Status:** Full

## Overview

`firefly-resilience` provides **composable resilience decorators** that
wrap around any async Rust operation:

| Primitive        | Failure mode it shields against              | Error variant                          |
|------------------|----------------------------------------------|----------------------------------------|
| `CircuitBreaker` | Cascading failure of a slow / failing dep    | `ResilienceError::CircuitOpen`         |
| `RateLimiter`    | Outbound rate cap (token bucket)             | `ResilienceError::RateLimited`         |
| `Bulkhead`       | Resource exhaustion via runaway concurrency  | `ResilienceError::BulkheadFull` (or block) |
| `Timeout`        | Stuck calls                                  | `ResilienceError::Timeout`             |
| `Retry`          | Transient failures (re-run with backoff)     | the operation's own error after exhaustion |

`Chain` composes them into a single guarded call. Error messages use
stable sentinels (`firefly/resilience: circuit open`, …) so logs and
dashboards stay consistent. Cancellation is expressed the idiomatic Rust
way: dropping the future aborts the in-flight call.

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
// — or use the err.is_circuit_open() predicate
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

### Failure-rate window mode

Setting `failure_rate_threshold` switches the breaker from
consecutive-failure counting to a count-based failure-rate window: the
outcomes of the last `window_size` calls are kept in a ring buffer, and
the breaker opens once the window is **full** and the failure fraction
reaches the threshold.

```rust,ignore
let cb = CircuitBreaker::new(CircuitConfig {
    failure_rate_threshold: Some(0.5), // open at ≥ 50% failures…
    window_size: 10,                   // …over the last 10 calls (full window required)
    half_open_max_calls: 2,            // 2 successful probes close the circuit
    ..CircuitConfig::default()
});
```

For manual instrumentation the breaker also exposes low-level hooks:

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

Per-call deadline. When the budget is exceeded the operation's future is
cancelled outright (dropped), so no work continues past the deadline.

```rust,ignore
let to = Timeout::new(Duration::from_secs(2));
let result = to.execute(|| async { slow_call().await }).await;
// err.is_timeout() on budget exceeded
```

## Retry

`Retry` is the declarative retry combinator. It re-runs a **re-runnable**
async closure (`Fn() -> Future`) up to `max_attempts` times while the failure
is retryable, sleeping `delay * backoff^attempt` (capped at `max_delay`, with
optional ±`jitter`) between attempts, then surfacing the last error. Unlike the
other primitives the operation is `Fn` (not `FnOnce`) because each attempt must
produce a fresh future.

```rust,ignore
use firefly_resilience::{Retry, ResilienceError, retry};

// Fluent builder:
let policy = Retry::new()
    .max_attempts(5)                          // default 3
    .delay(Duration::from_millis(100))        // base delay (default 0)
    .backoff(2.0)                             // exponential factor (default 1.0)
    .max_delay(Duration::from_secs(2))        // optional cap
    .jitter(0.1)                              // ±10% (default 0)
    .retry_on(|e| !e.is_circuit_open());      // which errors trigger a retry

let out = policy.execute(|| async { charge(amount).await }).await;

// The `retry(n)` free function offers a terse call shape:
let out = retry(3).delay(Duration::from_millis(50)).execute(|| async {
    fetch().await
}).await;
```

`retry_on` decides which failures trigger a retry; errors it rejects propagate
immediately without consuming further attempts. By default **every** error is
retried. The backoff schedule is exact — `delay_for(attempt)` returns the
deterministic per-attempt wait — and jitter is drawn from an injectable sampler
(`with_jitter_fn`) so retry timing is fully deterministic under test.

### Composing retry with a `Chain`

Because retry must re-run the guarded call, it wraps a re-runnable closure
rather than the single-shot `Operation` a `Chain` decorator receives. To retry
a whole guarded chain, wrap the chain's execution in `Retry::execute` — each
attempt drives a fresh pass through the chain:

```rust,ignore
let chain = Chain::new().with(Timeout::new(Duration::from_secs(1)));
let out = retry(3)
    .execute(|| chain.execute(|| async { call().await }))
    .await;
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

## Fallback

`Fallback` is the graceful-degradation decorator. It forwards successes
untouched; when the inner operation — or an inner decorator's
short-circuit sentinel — fails with an error matched by the `on`
predicate, the handler runs and its result replaces the operation's.
`Chain` operations are unit-valued, so a static fallback value is
expressed as `Fallback::recover()` ("swallow and `Ok(())`"); for
value-returning calls plain `Result` combinators remain idiomatic.

```rust,ignore
let chain = Chain::new()
    .with(Fallback::new(|err| { serve_cached(); Ok(()) })  // recovery handler
        .on(ResilienceError::is_timeout))                  // only fall back on timeouts
    .with(Timeout::new(Duration::from_secs(2)));
// async handlers: Fallback::new_async(|err| async move { ... })
```

### ResilienceRegistry

`ResilienceRegistry` materialises named breakers / limiters / bulkheads / time-limiters / **retries** from `firefly.resilience.*` configuration keys, consuming
the flat dot-keyed map that `firefly-config` sources produce:

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
    retry:
      payment-api:
        max-attempts: 5            # default 3
        delay: 100ms               # default 0s
        backoff: 2.0               # default 1.0
        max-delay: 2s              # optional cap
        jitter: 0.1                # default 0.0
```

```rust,ignore
let registry = ResilienceRegistry::from_config(&layered)?; // or from_sources / from_map
let cb = registry.circuit_breaker("payment-api")?;  // Arc<CircuitBreaker>
let rl = registry.rate_limiter("search-api")?;      // Arc<RateLimiter>
let bh = registry.bulkhead("db-pool")?;             // Arc<Bulkhead>
let d  = registry.time_limiter("slow-report")?;     // Duration
let to = registry.timeout("slow-report")?;          // ready-made Timeout decorator
let rt = registry.retry("payment-api")?;            // ready-made Retry policy
```

Unknown names return `RegistryError::NotFound` with a descriptive
message (`No bulkhead named 'x'. Available: ['alpha', 'beta']`).
Kebab-case and snake_case bind interchangeably (relaxed binding,
matching `firefly-config`'s merge normalization) for sections,
properties, and instance names alike. Durations accept the common forms
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
    pub failure_rate_threshold: Option<f64>,  // Some(r) → count-based window mode
    pub window_size: usize,                   // 0 → 10 (ring-buffer length)
    pub half_open_max_calls: usize,           // 0 → 1 (probe budget)
}
impl Default for CircuitConfig;    // 5 failures / 1 s window / 30 s open / consecutive mode
impl CircuitBreaker {
    pub fn new(cfg: CircuitConfig) -> Self;
    pub async fn execute<T, F, Fut>(&self, op: F) -> Result<T, ResilienceError>;
    pub fn state(&self) -> CircuitState;
    // manual hooks
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
    pub fn rate(&self) -> f64;     // refill rate (tokens/sec)
    pub fn burst(&self) -> usize;  // bucket capacity (max tokens)
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

pub struct RetryConfig {
    pub max_attempts: usize,        // < 1 clamped to 1; default 3
    pub delay: Duration,            // base delay; default ZERO
    pub backoff: f64,               // delay * backoff^attempt; default 1.0
    pub max_delay: Option<Duration>,// per-attempt cap; default None
    pub jitter: f64,                // ±jitter * wait, clamped 0..=1; default 0
}
impl Default for RetryConfig;       // sensible defaults
impl Retry {
    pub fn new() -> Self;                                  // default policy
    pub fn from_config(cfg: RetryConfig) -> Self;
    pub fn config(&self) -> RetryConfig;
    pub fn max_attempts(self, n: usize) -> Self;           // fluent setters
    pub fn delay(self, d: Duration) -> Self;
    pub fn backoff(self, b: f64) -> Self;
    pub fn max_delay(self, d: Duration) -> Self;
    pub fn jitter(self, j: f64) -> Self;
    pub fn retry_on(self, pred) -> Self;                   // which errors retry
    pub fn with_jitter_fn(self, sampler) -> Self;          // deterministic tests
    pub fn delay_for(&self, attempt: usize) -> Duration;   // deterministic schedule
    pub async fn execute<T, F: Fn, Fut>(&self, op: F) -> Result<T, ResilienceError>;
}
pub fn retry(max_attempts: usize) -> Retry;                // terse builder entry point

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
                                   // .time_limiter(n) .timeout(n) .retry(n)
                                   // .register_*(n, v) .*_names()
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
outermost) — plus stable sentinel messages, `Send`/`Sync` bounds, and
nested chains.

A dedicated tuning layer exercises failure-rate window
trip/partial-window/sliding semantics and the half-open probe budget;
registry duration parsing, config materialisation, relaxed binding,
unknown-name errors, and direct construction; the fallback behaviors —
success bypass, handler receives the error, predicate filtering, async
handlers — and the retry combinator: first-try success,
retry-until-success, attempt-exhaustion-resurfaces-last-error,
single-attempt-no-retry, non-retryable-error-propagates-immediately, the
`delay * backoff^attempt` schedule, `max_delay` capping, jittered-wait
bounds and zero-floor, and chain composition.
