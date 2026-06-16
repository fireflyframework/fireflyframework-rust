// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-scheduling — Cron + FixedRate + FixedDelay task scheduler.
//!
//! This crate is the framework's task scheduler — a [`Scheduler`] runner
//! that owns Cron, FixedRate, and FixedDelay triggers, runs each task on its
//! own tokio task with panic recovery, and shuts down gracefully on
//! [`Scheduler::stop`]. It ports the Go `scheduling` module (itself modeled
//! on Spring `@Scheduled`) with the same 5-field cron semantics, extended to
//! pyfly parity: 6-field (seconds-first) cron, `?`, `@hourly`-style macros,
//! IANA time zones, initial delays, and ShedLock-style distributed locks.
//!
//! # Cron syntax
//!
//! Canonical 5-field `minute hour day-of-month month day-of-week`, plus the
//! pyfly-parity extensions: Spring 6-field with a leading **seconds** field,
//! the Quartz `?` placeholder, and the `@hourly` / `@daily` / `@weekly` /
//! `@monthly` / `@yearly` macros. Each field accepts a literal (`0`), a
//! list (`0,15,30,45`), a range (`9-17`), a wildcard (`*`), and a step
//! (`*/15`, `9-17/2`). When **both** day-of-month and day-of-week are
//! restricted, the rule fires when **either** matches (Vixie cron
//! behaviour).
//!
//! # Triggers
//!
//! | Trigger              | Behaviour                                                        |
//! |----------------------|------------------------------------------------------------------|
//! | [`CronTrigger`]      | Fires when the **local** wall clock matches the parsed expression|
//! | [`ZonedCronTrigger`] | Fires per the expression in an IANA time zone (default UTC)      |
//! | [`FixedRateTrigger`] | Fires every period from a fixed start anchor (slips on slow runs)|
//! | [`FixedDelayTrigger`]| Fires a delay after the previous run finished                    |
//!
//! # Distributed locks (pyfly / ShedLock parity)
//!
//! A [`Task`] may declare a lock name + TTL ([`Task::with_lock`]); before
//! each tick the scheduler acquires it from its [`DistributedLock`] provider
//! ([`Scheduler::with_lock`]) and **skips the tick** when it is held
//! elsewhere — so in a cluster only one instance runs the job. Providers:
//! [`LocalLock`] (always acquires — the default), [`InProcessLock`]
//! (in-process mutual exclusion with TTL self-heal), [`RedisLock`]
//! (`SET NX PX` + owner-token Lua release), and [`PostgresAdvisoryLock`]
//! (`pg_try_advisory_lock` on a held session).
//!
//! # Async method execution (Spring `@Async`)
//!
//! [`TaskExecutor`] is the Rust rendering of Spring's `TaskExecutor` / `@Async`:
//! [`TaskExecutor::spawn`] hands a `Send + 'static` future off to its own tokio
//! task and returns an awaitable [`TaskHandle`], with concurrency optionally
//! bounded by a semaphore (the pool size). A process-global registry
//! ([`register_task_executor`] / [`task_executor`]) lets a starter install the
//! application executor once, which the `#[async_method]` macro targets;
//! unregistered, [`task_executor`] yields a default unbounded executor.
//!
//! # Quick start
//!
//! ```no_run
//! use std::{sync::Arc, time::Duration};
//! use firefly_scheduling::Scheduler;
//!
//! # async fn demo() {
//! let scheduler = Arc::new(Scheduler::new());
//! scheduler
//!     .cron("nightly-rollup", "0 2 * * *", || async { Ok(()) })
//!     .unwrap();
//! scheduler
//!     .cron_in_zone("ny-open", "30 9 * * 1-5", "America/New_York", || async { Ok(()) })
//!     .unwrap();
//! scheduler.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });
//! scheduler.fixed_delay("cleanup", Duration::from_secs(300), || async { Ok(()) });
//! scheduler.fixed_rate_with_initial_delay(
//!     "warmup",
//!     Duration::from_secs(60),
//!     Duration::from_secs(10),
//!     || async { Ok(()) },
//! );
//!
//! let snapshot = scheduler.tasks(); // admin/actuator descriptor feed
//! assert_eq!(snapshot.len(), 5);
//!
//! let handle = Arc::clone(&scheduler);
//! tokio::spawn(async move {
//!     tokio::signal::ctrl_c().await.ok();
//!     handle.stop();
//! });
//! scheduler.start().await; // blocks until stop()
//! # }
//! ```

mod cron;
mod discovery;
mod executor;
mod lock;
mod postgres_lock;
mod redis_lock;
mod scheduler;

// Re-export `inventory` so `#[scheduled]`-generated `ScheduledRegistration`
// thunks submit through `firefly_scheduling::inventory`.
pub use inventory;

pub use cron::{parse_cron, CronError, CronExpr};
pub use discovery::{
    discovered_scheduled_bean_count, discovered_scheduled_count, register_discovered_scheduled,
    register_discovered_scheduled_beans, BeanScheduledRegistration, ScheduledRegistration,
};
pub use executor::{
    register_task_executor, task_executor, TaskExecutor, TaskHandle, TaskJoinError,
};
pub use lock::{DistributedLock, InProcessLock, LocalLock, LockError};
pub use postgres_lock::PostgresAdvisoryLock;
pub use redis_lock::RedisLock;
pub use scheduler::{
    CronTrigger, FixedDelayTrigger, FixedRateTrigger, ScheduleError, Scheduler, Task,
    TaskDescriptor, TaskError, TaskFn, TaskFuture, TaskLock, Trigger, TriggerDescriptor,
    ZonedCronTrigger,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.14";
