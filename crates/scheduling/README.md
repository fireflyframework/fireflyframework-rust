# `firefly-scheduling`

> **Tier:** Platform · **Status:** Full · **Java original:** Spring `@Scheduled` · **Go module:** `scheduling`

## Overview

`firefly-scheduling` is the framework's **task scheduler** — a `Scheduler`
runner that owns Cron, FixedRate, and FixedDelay triggers, runs each task
on its own tokio task with panic recovery, and shuts down gracefully when
`stop()` is called.

```rust,no_run
use std::{sync::Arc, time::Duration};
use firefly_scheduling::Scheduler;

# async fn demo() {
let s = Arc::new(Scheduler::new());
s.cron("nightly-rollup", "0 2 * * *", || async { Ok(()) }).unwrap();
s.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });
s.fixed_delay("cleanup", Duration::from_secs(300), || async { Ok(()) });

let handle = Arc::clone(&s);
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    handle.stop();
});
s.start().await; // blocks until stop(); in-flight runs finish first
# }
```

## Cron syntax

5-field, no macros (yet). `minute hour day-of-month month day-of-week`.
Each field accepts:

* A literal: `0`, `15`, `30`
* A list: `0,15,30,45`
* A range: `9-17`
* A wildcard: `*`
* A step: `*/15`, `9-17/2`

Day-of-month + day-of-week semantics: when **both** are restricted,
the rule fires when **either** matches (Vixie cron behaviour).

## Triggers

| Trigger             | Behaviour                                                            |
|---------------------|----------------------------------------------------------------------|
| `CronTrigger`       | Fires when the wall clock matches the parsed expression               |
| `FixedRateTrigger`  | Fires every period from a fixed start anchor (slips on slow runs)     |
| `FixedDelayTrigger` | Fires the delay after the previous run finished                       |

## Public surface

```rust,ignore
pub trait Trigger: Send {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>>;
    fn finished(&mut self, at: DateTime<Local>) {} // FixedDelay's lastRun hook
}

pub struct CronExpr { pub minute, hour, day_of_month, month, day_of_week: Vec<u32> }
pub fn parse_cron(expr: &str) -> Result<CronExpr, CronError>;
impl CronExpr { pub fn next<Tz: TimeZone>(&self, from: DateTime<Tz>) -> Option<DateTime<Tz>> }

pub struct CronTrigger       { pub expr: CronExpr }
pub struct FixedRateTrigger  { pub start: DateTime<Local>, pub period: Duration }
pub struct FixedDelayTrigger { pub delay: Duration, /* private last_run */ }

pub struct Task { pub name: String, pub trigger: Box<dyn Trigger>, pub run: TaskFn }

pub struct Scheduler { /* … */ }
impl Scheduler {
    pub fn new() -> Self;
    pub fn register(&self, task: Task);
    pub fn cron(&self, name, expr, run) -> Result<(), CronError>;
    pub fn fixed_rate(&self, name, period, run);
    pub fn fixed_delay(&self, name, delay, run);
    pub async fn start(&self); // blocks until stop()
    pub fn stop(&self);
}
```

## Design notes (vs the Go port)

* Goroutines become tokio tasks; Go's `recover()` becomes a spawned-task
  join so a panicking task is logged and the scheduler keeps running.
* Go's `context.Context` cancellation becomes the explicit `stop()` call
  (a `watch` channel under the hood); `start()` resolves once every
  in-flight run completes — the `sync.WaitGroup` drain.
* Go downcasts the trigger to update `FixedDelayTrigger.lastRun`; the Rust
  trait gains an explicit `finished(at)` hook with a no-op default instead.
* The Go `WithLogger(*slog.Logger)` builder is replaced by the `tracing`
  facade — install any subscriber to capture `scheduler: task error` /
  `scheduler: task panic` records.
* The cron grammar, field bounds, Vixie either-match day semantics, and
  error message wording are ported verbatim.

## Testing

```bash
cargo test -p firefly-scheduling
```

Covers cron parsing (literal, list, range, step, invalid), `next`
evaluation (weekday windows, year rollover, unsatisfiable expressions),
FixedRate timing, FixedDelay timing (delay-after-finish), and panic
recovery (a panicking task does not stop the scheduler).
