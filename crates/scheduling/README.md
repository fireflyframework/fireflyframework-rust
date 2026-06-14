# `firefly-scheduling`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-scheduling` is the framework's **task scheduler** — a `Scheduler`
runner that owns Cron, FixedRate, and FixedDelay triggers, runs each task
on its own tokio task with panic recovery, and shuts down gracefully when
`stop()` is called. It also provides 6-field (seconds-first) cron, IANA
time zones, initial delays, distributed locks, and a task descriptor
snapshot for the actuator.

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

Canonical 5-field `minute hour day-of-month month day-of-week`, plus these
extensions: a **6-field** form with a leading seconds field
(`sec min hour dom month dow`), the Quartz-style `?` placeholder (treated as
`*`), and the `@hourly` / `@daily` / `@weekly` / `@monthly` / `@yearly`
macros (with the conventional `@midnight` / `@annually` aliases).
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
| `CronTrigger`       | Fires when the **local** wall clock matches the parsed expression     |
| `ZonedCronTrigger`  | Fires per the expression in an IANA time zone (`utc()` = default)      |
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

## Advanced scheduling

Beyond the core triggers, the scheduler offers cron introspection helpers,
zoned cron, distributed locks, initial delays, and an actuator snapshot:

```rust,ignore
// Cron extras: prev / next_n / seconds_until_next, Display canonical form.
impl CronExpr {
    pub fn prev<Tz: TimeZone>(&self, before: DateTime<Tz>) -> Option<DateTime<Tz>>;
    pub fn next_n<Tz: TimeZone>(&self, n: usize, after: DateTime<Tz>) -> Vec<DateTime<Tz>>;
    pub fn seconds_until_next<Tz: TimeZone>(&self, after: DateTime<Tz>) -> Option<f64>;
}

// IANA-zone cron — evaluate an expression in a named time zone.
pub struct ZonedCronTrigger { pub expr: CronExpr, pub zone: chrono_tz::Tz }
impl ZonedCronTrigger {
    pub fn new(expr, zone) -> Self;
    pub fn utc(expr) -> Self;                       // zoneless default = UTC
    pub fn in_zone(expr, "America/New_York") -> Result<Self, ScheduleError>;
}

// Distributed locks — a ShedLock-style DistributedLock protocol.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    async fn try_acquire(&self, name: &str, ttl: Duration) -> Result<bool, LockError>;
    async fn release(&self, name: &str) -> Result<(), LockError>;
}
pub struct LocalLock;             // always acquires (single-instance default)
pub struct InProcessLock;         // in-process mutual exclusion + TTL self-heal
pub struct RedisLock;             // SET NX PX + owner-token Lua release
pub struct PostgresAdvisoryLock;  // pg_try_advisory_lock on a held session

// Task-level lock guard, checked before every tick (held ⇒ tick skipped).
impl Task { pub fn with_lock(self, name, ttl) -> Self }
pub struct TaskLock { pub name: String, pub ttl: Duration } // DEFAULT_TTL = 60s

// Scheduler additions.
impl Scheduler {
    pub fn with_lock(lock: Arc<dyn DistributedLock>) -> Self;
    pub fn cron_in_zone(&self, name, expr, zone, run) -> Result<(), ScheduleError>;
    pub fn fixed_rate_with_initial_delay(&self, name, period, initial_delay, run);
    pub fn fixed_delay_with_initial_delay(&self, name, delay, initial_delay, run);
    pub fn tasks(&self) -> Vec<TaskDescriptor>; // actuator/admin snapshot
}
```

`Scheduler::tasks()` returns registration-time `TaskDescriptor`s that
serialize in the actuator `/actuator/scheduledtasks` shape
(`{"type":"fixedRate","interval":30000,"initialDelay":1000}`, durations as
integer milliseconds), and remains available after `start()` drains the
run queue.

Behavior and defaults:

* Scheduling is configured through explicit builders — `Task::with_lock`,
  `Scheduler::cron_in_zone`, `*_with_initial_delay` — rather than
  attribute scanning.
* The unzoned `CronTrigger` evaluates against the **local** wall clock;
  for a UTC default use `ZonedCronTrigger::utc`.
* `DistributedLock` returns `Result<bool, LockError>`; the scheduler
  treats backend errors as "not acquired", logs, and skips the tick. The
  lock is always released after the body finishes, including on panic, and
  the TTL is the crash-safety valve.
* `RedisLock` uses the `SET key token NX PX ms` plus owner-token
  compare-and-delete Lua protocol with the `firefly:schedlock:` prefix;
  tests run against an in-process fake RESP server, not a real Redis.
* `PostgresAdvisoryLock` uses `tokio-postgres` and derives the signed
  64-bit advisory key from SHA-256. Its acquire / contend / release
  round-trip test is **env-gated** on `FIREFLY_TEST_POSTGRES_URL`
  (fallback `DATABASE_URL` / `POSTGRES_URL`): it skips with a one-line
  notice when unset and runs a genuine `pg_try_advisory_lock` round-trip
  against a live Postgres when set.
* The `FieldCount` error message reads "want 5 fields" even though 6-field
  expressions are accepted — logs and tests depend on it.

## Design notes

* Each task runs on its own tokio task; a spawned-task join means a
  panicking task is logged and the scheduler keeps running.
* Cancellation is the explicit `stop()` call (a `watch` channel under the
  hood); `start()` resolves once every in-flight run completes — the run
  queue drains before shutdown returns.
* The `Trigger` trait exposes an explicit `finished(at)` hook with a no-op
  default, used to update `FixedDelayTrigger`'s last-run timestamp.
* Logging goes through the `tracing` facade — install any subscriber to
  capture `scheduler: task error` / `scheduler: task panic` records.
* The cron grammar enforces standard field bounds and Vixie either-match
  day-of-month / day-of-week semantics.

## Testing

```bash
cargo test -p firefly-scheduling
```

Covers cron parsing (literal, list, range, step, invalid), `next`
evaluation (weekday windows, year rollover, unsatisfiable expressions),
FixedRate timing, FixedDelay timing (delay-after-finish), and panic
recovery (a panicking task does not stop the scheduler). Further tests
cover cron timezone evaluation, 5- and 6-field expressions, the
distributed-lock protocol, the Redis lock (against an in-process fake RESP
server on a `TcpListener`), and the Postgres advisory lock (key derivation
+ no-op release offline; the real acquire / contend / release round trip is
env-gated on `FIREFLY_TEST_POSTGRES_URL`, skipping when unset).
