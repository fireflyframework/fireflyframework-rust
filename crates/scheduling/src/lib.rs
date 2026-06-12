//! firefly-scheduling — Cron + FixedRate + FixedDelay task scheduler.
//!
//! This crate is the framework's task scheduler — a [`Scheduler`] runner
//! that owns Cron, FixedRate, and FixedDelay triggers, runs each task on its
//! own tokio task with panic recovery, and shuts down gracefully on
//! [`Scheduler::stop`]. It ports the Go `scheduling` module (itself modeled
//! on Spring `@Scheduled`) with the same 5-field cron semantics.
//!
//! # Cron syntax
//!
//! 5-field, no macros (yet): `minute hour day-of-month month day-of-week`.
//! Each field accepts a literal (`0`), a list (`0,15,30,45`), a range
//! (`9-17`), a wildcard (`*`), and a step (`*/15`, `9-17/2`). When **both**
//! day-of-month and day-of-week are restricted, the rule fires when
//! **either** matches (Vixie cron behaviour).
//!
//! # Triggers
//!
//! | Trigger              | Behaviour                                                        |
//! |----------------------|------------------------------------------------------------------|
//! | [`CronTrigger`]      | Fires when the wall clock matches the parsed expression          |
//! | [`FixedRateTrigger`] | Fires every period from a fixed start anchor (slips on slow runs)|
//! | [`FixedDelayTrigger`]| Fires a delay after the previous run finished                    |
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
//! scheduler.fixed_rate("metrics-emit", Duration::from_secs(30), || async { Ok(()) });
//! scheduler.fixed_delay("cleanup", Duration::from_secs(300), || async { Ok(()) });
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
mod scheduler;

pub use cron::{parse_cron, CronError, CronExpr};
pub use scheduler::{
    CronTrigger, FixedDelayTrigger, FixedRateTrigger, Scheduler, Task, TaskError, TaskFn,
    TaskFuture, Trigger,
};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
