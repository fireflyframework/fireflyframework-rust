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

//! The task scheduler: triggers, tasks, and the [`Scheduler`] runner.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use futures::future::BoxFuture;
use tokio::sync::watch;

use crate::cron::{parse_cron, CronError, CronExpr};
use crate::lock::{DistributedLock, LocalLock};

/// Error returned by the zone-aware scheduling helpers.
///
/// Wraps [`CronError`] for malformed expressions and adds the
/// unknown-IANA-zone case introduced by the pyfly-parity layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    /// The cron expression failed to parse.
    #[error(transparent)]
    Cron(#[from] CronError),
    /// The IANA time-zone name was not recognized by `chrono-tz`.
    #[error("scheduling: unknown IANA time zone {0:?}")]
    UnknownZone(String),
}

/// Boxed error returned by a task run.
///
/// The Go port's `func(ctx) error`; any error a run yields is logged at
/// `warn` level and the schedule continues.
pub type TaskError = Box<dyn std::error::Error + Send + Sync>;

/// The future one task invocation produces.
pub type TaskFuture = BoxFuture<'static, Result<(), TaskError>>;

/// A task body: a cloneable factory invoked once per trigger firing.
pub type TaskFn = Arc<dyn Fn() -> TaskFuture + Send + Sync>;

/// Trigger reports the next time a [`Task`] should run, given the current
/// wall-clock time.
pub trait Trigger: Send {
    /// Returns the next firing time at or after `now`, or `None` when the
    /// trigger can never fire again (the task loop then exits with a warning).
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>>;

    /// Notifies the trigger that a run finished at `at`.
    ///
    /// Default is a no-op; [`FixedDelayTrigger`] records the finish time —
    /// the Rust replacement for the Go port's type assertion on
    /// `*FixedDelayTrigger`. Not called when the run panicked, matching Go,
    /// where a panic skips the `lastRun` update. It **is** called when a
    /// tick is skipped because a distributed lock was held elsewhere, so a
    /// fixed-delay task waits its delay instead of retrying immediately.
    fn finished(&mut self, at: DateTime<Local>) {
        let _ = at;
    }

    /// Describes the trigger for the [`Scheduler::tasks`] snapshot (the
    /// `/actuator/scheduledtasks` feed). Defaults to
    /// [`TriggerDescriptor::Custom`] so existing third-party triggers keep
    /// compiling.
    fn descriptor(&self) -> TriggerDescriptor {
        TriggerDescriptor::Custom
    }
}

/// Serializes a `Duration` as integer milliseconds (Spring actuator shape).
fn ser_millis<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Serializes an `Option<Duration>` as integer milliseconds.
fn ser_opt_millis<S: serde::Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match d {
        Some(d) => ser_millis(d, s),
        None => s.serialize_none(),
    }
}

/// How a task fires — the trigger half of a [`TaskDescriptor`].
///
/// Serializes in the Spring `/actuator/scheduledtasks` shape pyfly emits:
/// `{"type":"cron","expression":…}` /
/// `{"type":"fixedRate","interval":ms,"initialDelay":ms}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TriggerDescriptor {
    /// A [`CronTrigger`] or [`ZonedCronTrigger`]: the canonical expression
    /// plus the IANA zone name when one is set.
    #[serde(rename_all = "camelCase")]
    Cron {
        /// Canonical rendering of the parsed expression.
        expression: String,
        /// IANA zone name (`"America/New_York"`), `None` for the legacy
        /// local-time [`CronTrigger`].
        #[serde(skip_serializing_if = "Option::is_none")]
        zone: Option<String>,
    },
    /// A [`FixedRateTrigger`].
    #[serde(rename_all = "camelCase")]
    FixedRate {
        /// Period between firings.
        #[serde(serialize_with = "ser_millis")]
        interval: Duration,
        /// Delay before the first firing, when one was requested.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "ser_opt_millis"
        )]
        initial_delay: Option<Duration>,
    },
    /// A [`FixedDelayTrigger`].
    #[serde(rename_all = "camelCase")]
    FixedDelay {
        /// Pause between the end of one run and the start of the next.
        #[serde(serialize_with = "ser_millis")]
        interval: Duration,
        /// Delay before the first firing, when one was requested.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "ser_opt_millis"
        )]
        initial_delay: Option<Duration>,
    },
    /// Any third-party [`Trigger`] that does not override
    /// [`Trigger::descriptor`].
    Custom,
}

/// Immutable snapshot of one registered task, returned by
/// [`Scheduler::tasks`] for admin/actuator consumption.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDescriptor {
    /// Task name as registered.
    pub name: String,
    /// How the task fires.
    pub trigger: TriggerDescriptor,
    /// Distributed-lock name, when the task is lock-guarded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_name: Option<String>,
    /// Distributed-lock TTL, when the task is lock-guarded.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_millis"
    )]
    pub lock_ttl: Option<Duration>,
}

/// Fires according to a parsed [`CronExpr`], evaluated in the system's
/// **local** time (the Go-port behaviour, kept for compatibility).
///
/// For pyfly parity — where an unzoned cron is evaluated in **UTC** and a
/// `zone=` is an IANA name — use [`ZonedCronTrigger`] (or
/// [`CronTrigger::zoned`] to convert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronTrigger {
    /// The parsed cron expression driving this trigger.
    pub expr: CronExpr,
}

impl CronTrigger {
    /// Upgrades this trigger to evaluate in an IANA time zone.
    pub fn zoned(self, zone: chrono_tz::Tz) -> ZonedCronTrigger {
        ZonedCronTrigger::new(self.expr, zone)
    }
}

impl Trigger for CronTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        self.expr.next(now)
    }

    fn descriptor(&self) -> TriggerDescriptor {
        TriggerDescriptor::Cron {
            expression: self.expr.to_string(),
            zone: None,
        }
    }
}

/// Fires according to a parsed [`CronExpr`] evaluated in an IANA time zone
/// — pyfly's `CronExpression(expr, zone=…)` / Spring `@Scheduled(zone=…)`.
///
/// pyfly's default when no zone is given is **UTC** ([`ZonedCronTrigger::utc`]);
/// the legacy [`CronTrigger`] keeps the Go port's local-time evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZonedCronTrigger {
    /// The parsed cron expression driving this trigger.
    pub expr: CronExpr,
    /// IANA zone the calendar fields are evaluated in.
    pub zone: chrono_tz::Tz,
}

impl ZonedCronTrigger {
    /// Builds a trigger evaluating `expr` in `zone`.
    pub fn new(expr: CronExpr, zone: chrono_tz::Tz) -> Self {
        Self { expr, zone }
    }

    /// Builds a trigger evaluating `expr` in UTC — pyfly's zoneless default.
    pub fn utc(expr: CronExpr) -> Self {
        Self::new(expr, chrono_tz::Tz::UTC)
    }

    /// Builds a trigger from an IANA zone name like `"America/New_York"`.
    pub fn in_zone(expr: CronExpr, zone: &str) -> Result<Self, ScheduleError> {
        let tz: chrono_tz::Tz = zone
            .parse()
            .map_err(|_| ScheduleError::UnknownZone(zone.to_string()))?;
        Ok(Self::new(expr, tz))
    }
}

impl Trigger for ZonedCronTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        self.expr
            .next(now.with_timezone(&self.zone))
            .map(|t| t.with_timezone(&Local))
    }

    fn descriptor(&self) -> TriggerDescriptor {
        TriggerDescriptor::Cron {
            expression: self.expr.to_string(),
            zone: Some(self.zone.name().to_string()),
        }
    }
}

/// Fires every `period` from a fixed `start` anchor — the schedule slips
/// when a previous run is slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedRateTrigger {
    /// Anchor the period grid hangs from.
    pub start: DateTime<Local>,
    /// Interval between firings.
    pub period: Duration,
}

impl Trigger for FixedRateTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        if self.period.is_zero() {
            return now.checked_add_signed(chrono::Duration::seconds(1));
        }
        if now < self.start {
            return Some(self.start);
        }
        let elapsed = (now - self.start).num_nanoseconds()?;
        let period = i64::try_from(self.period.as_nanos()).ok()?;
        let steps = elapsed / period + 1;
        self.start
            .checked_add_signed(chrono::Duration::nanoseconds(steps.checked_mul(period)?))
    }

    fn descriptor(&self) -> TriggerDescriptor {
        // An initial delay is encoded into `start`, so it is not visible
        // here; Scheduler::fixed_rate_with_initial_delay records it on the
        // TaskDescriptor itself.
        TriggerDescriptor::FixedRate {
            interval: self.period,
            initial_delay: None,
        }
    }
}

/// Fires `delay` after the previous run finished; the first dispatch is
/// immediate (or after the optional initial delay). The last finish time is
/// private state fed by [`Trigger::finished`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedDelayTrigger {
    /// Pause between the end of one run and the start of the next.
    pub delay: Duration,
    last_run: Option<DateTime<Local>>,
    initial_delay: Option<Duration>,
}

impl FixedDelayTrigger {
    /// Returns a trigger that has never run (first dispatch is immediate).
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            last_run: None,
            initial_delay: None,
        }
    }

    /// Returns a trigger whose **first** dispatch waits `initial_delay`
    /// (subsequent dispatches wait `delay` after each finish) — pyfly's
    /// `@scheduled(fixed_delay=…, initial_delay=…)`.
    pub fn with_initial_delay(delay: Duration, initial_delay: Duration) -> Self {
        Self {
            delay,
            last_run: None,
            initial_delay: Some(initial_delay),
        }
    }
}

impl Trigger for FixedDelayTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        match self.last_run {
            None => match self.initial_delay {
                None => Some(now),
                Some(d) => now.checked_add_signed(chrono::Duration::from_std(d).ok()?),
            },
            Some(last) => last.checked_add_signed(chrono::Duration::from_std(self.delay).ok()?),
        }
    }

    fn finished(&mut self, at: DateTime<Local>) {
        self.last_run = Some(at);
    }

    fn descriptor(&self) -> TriggerDescriptor {
        TriggerDescriptor::FixedDelay {
            interval: self.delay,
            initial_delay: self.initial_delay,
        }
    }
}

/// Distributed-lock requirement attached to a [`Task`] — pyfly's
/// `@scheduled(lock=…, lock_ttl=…)` metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLock {
    /// Lock name acquired before each tick.
    pub name: String,
    /// TTL handed to the lock backend — the crash-safety valve.
    pub ttl: Duration,
}

impl TaskLock {
    /// Default TTL when none is given — pyfly's `lock_ttl: float = 60.0`.
    pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

    /// A lock requirement with the default 60-second TTL.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ttl: Self::DEFAULT_TTL,
        }
    }

    /// A lock requirement with an explicit TTL.
    pub fn with_ttl(name: impl Into<String>, ttl: Duration) -> Self {
        Self {
            name: name.into(),
            ttl,
        }
    }
}

/// Task is the unit of scheduled work.
pub struct Task {
    /// Human-readable name used in log records.
    pub name: String,
    /// Decides when the task fires next.
    pub trigger: Box<dyn Trigger>,
    /// The body executed on each firing.
    pub run: TaskFn,
    /// Optional distributed-lock guard, checked before every tick. When the
    /// lock is held elsewhere the tick is **skipped** — pyfly/ShedLock
    /// semantics. `None` (the [`Task::new`] default) runs unconditionally.
    pub lock: Option<TaskLock>,
}

impl Task {
    /// Builds a task from a name, a trigger, and an async run closure.
    pub fn new<F, Fut>(name: impl Into<String>, trigger: impl Trigger + 'static, run: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            trigger: Box::new(trigger),
            run: Arc::new(move || Box::pin(run())),
            lock: None,
        }
    }

    /// Guards every tick with the named distributed lock (acquired from the
    /// scheduler's lock provider, see [`Scheduler::with_lock`]).
    pub fn with_lock(mut self, name: impl Into<String>, ttl: Duration) -> Self {
        self.lock = Some(TaskLock::with_ttl(name, ttl));
        self
    }
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("name", &self.name)
            .field("lock", &self.lock)
            .finish()
    }
}

/// Builds the registration-time snapshot for a task.
fn describe(task: &Task) -> TaskDescriptor {
    TaskDescriptor {
        name: task.name.clone(),
        trigger: task.trigger.descriptor(),
        lock_name: task.lock.as_ref().map(|l| l.name.clone()),
        lock_ttl: task.lock.as_ref().map(|l| l.ttl),
    }
}

/// Scheduler runs registered [`Task`]s, each on its own tokio task with
/// panic recovery.
///
/// Use [`Scheduler::new`], register tasks, then await [`Scheduler::start`];
/// call [`Scheduler::stop`] (from another task or a signal handler) to shut
/// down gracefully — in-flight runs complete before `start` returns.
///
/// Where the Go port carried an `slog.Logger` (`WithLogger`), this port logs
/// through the `tracing` facade.
pub struct Scheduler {
    tasks: Mutex<Vec<Task>>,
    descriptors: Mutex<Vec<TaskDescriptor>>,
    lock: Arc<dyn DistributedLock>,
    stop: watch::Sender<bool>,
}

impl Scheduler {
    /// Returns an empty scheduler whose lock provider is the no-op
    /// [`LocalLock`] — lock-guarded tasks always acquire (single-instance
    /// behaviour, pyfly's default).
    pub fn new() -> Self {
        Self::with_lock(Arc::new(LocalLock))
    }

    /// Returns an empty scheduler coordinating lock-guarded tasks through
    /// the given [`DistributedLock`] provider (e.g.
    /// [`InProcessLock`](crate::InProcessLock),
    /// [`RedisLock`](crate::RedisLock),
    /// [`PostgresAdvisoryLock`](crate::PostgresAdvisoryLock)).
    pub fn with_lock(lock: Arc<dyn DistributedLock>) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            tasks: Mutex::new(Vec::new()),
            descriptors: Mutex::new(Vec::new()),
            lock,
            stop,
        }
    }

    /// Adds a task — must be called before [`Scheduler::start`].
    pub fn register(&self, task: Task) {
        let descriptor = describe(&task);
        self.register_described(task, descriptor);
    }

    /// Registers a task with an explicit descriptor (used by the helpers
    /// that know more than the trigger does, e.g. fixed-rate initial delay).
    fn register_described(&self, task: Task, descriptor: TaskDescriptor) {
        self.descriptors
            .lock()
            .expect("scheduler descriptors poisoned")
            .push(descriptor);
        self.tasks
            .lock()
            .expect("scheduler tasks poisoned")
            .push(task);
    }

    /// Returns a snapshot of every registered task — name, trigger, and
    /// lock metadata — for admin/actuator consumption (the
    /// `/actuator/scheduledtasks` feed). The snapshot is taken at
    /// registration time, so it remains available after [`Scheduler::start`]
    /// has drained the run queue.
    pub fn tasks(&self) -> Vec<TaskDescriptor> {
        self.descriptors
            .lock()
            .expect("scheduler descriptors poisoned")
            .clone()
    }

    /// Convenience for registering a [`CronTrigger`] task; fails when the
    /// expression does not parse.
    pub fn cron<F, Fut>(&self, name: impl Into<String>, expr: &str, run: F) -> Result<(), CronError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        let expr = parse_cron(expr)?;
        self.register(Task::new(name, CronTrigger { expr }, run));
        Ok(())
    }

    /// Registers a fixed-rate task anchored at now.
    pub fn fixed_rate<F, Fut>(&self, name: impl Into<String>, period: Duration, run: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        self.register(Task::new(
            name,
            FixedRateTrigger {
                start: Local::now(),
                period,
            },
            run,
        ));
    }

    /// Registers a task that runs `delay` after each previous finish.
    pub fn fixed_delay<F, Fut>(&self, name: impl Into<String>, delay: Duration, run: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        self.register(Task::new(name, FixedDelayTrigger::new(delay), run));
    }

    /// Registers a cron task evaluated in an IANA time zone — pyfly's
    /// `@scheduled(cron=…, zone=…)`. Fails when the expression does not
    /// parse or the zone name is unknown.
    pub fn cron_in_zone<F, Fut>(
        &self,
        name: impl Into<String>,
        expr: &str,
        zone: &str,
        run: F,
    ) -> Result<(), ScheduleError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        let expr = parse_cron(expr)?;
        let trigger = ZonedCronTrigger::in_zone(expr, zone)?;
        self.register(Task::new(name, trigger, run));
        Ok(())
    }

    /// Registers a fixed-rate task whose first firing waits `initial_delay`
    /// — pyfly's `@scheduled(fixed_rate=…, initial_delay=…)`. The period
    /// grid is anchored at `now + initial_delay`.
    pub fn fixed_rate_with_initial_delay<F, Fut>(
        &self,
        name: impl Into<String>,
        period: Duration,
        initial_delay: Duration,
        run: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        let offset = chrono::Duration::from_std(initial_delay)
            .unwrap_or_else(|_| chrono::Duration::days(365_000));
        let start = Local::now()
            .checked_add_signed(offset)
            .unwrap_or_else(Local::now);
        let task = Task::new(name, FixedRateTrigger { start, period }, run);
        let mut descriptor = describe(&task);
        descriptor.trigger = TriggerDescriptor::FixedRate {
            interval: period,
            initial_delay: Some(initial_delay),
        };
        self.register_described(task, descriptor);
    }

    /// Registers a fixed-delay task whose first firing waits
    /// `initial_delay` instead of dispatching immediately — pyfly's
    /// `@scheduled(fixed_delay=…, initial_delay=…)`.
    pub fn fixed_delay_with_initial_delay<F, Fut>(
        &self,
        name: impl Into<String>,
        delay: Duration,
        initial_delay: Duration,
        run: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
    {
        self.register(Task::new(
            name,
            FixedDelayTrigger::with_initial_delay(delay, initial_delay),
            run,
        ));
    }

    /// Begins dispatching tasks. Resolves after [`Scheduler::stop`] is
    /// called and every in-flight run has completed.
    ///
    /// The Go port blocked on a `context.Context` and returned `ctx.Err()`;
    /// here cancellation is the explicit [`Scheduler::stop`] call (or
    /// dropping the scheduler, which stops orphaned task loops at their next
    /// checkpoint).
    pub async fn start(&self) {
        let tasks: Vec<Task> = {
            let mut guard = self.tasks.lock().expect("scheduler tasks poisoned");
            std::mem::take(&mut *guard)
        };
        let mut handles = Vec::with_capacity(tasks.len());
        for task in tasks {
            handles.push(tokio::spawn(run_task(
                self.stop.subscribe(),
                task,
                Arc::clone(&self.lock),
            )));
        }
        let mut stopped = self.stop.subscribe();
        let _ = stopped.wait_for(|s| *s).await;
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Signals registered tasks to exit. Safe to call multiple times, and
    /// effective even when called before [`Scheduler::start`].
    pub fn stop(&self) {
        // send_replace updates the value even with no live receivers, so a
        // stop issued before start() still takes effect.
        self.stop.send_replace(true);
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self.tasks.lock().map(|t| t.len()).unwrap_or(0);
        f.debug_struct("Scheduler")
            .field("pending_tasks", &pending)
            .finish()
    }
}

/// One task's dispatch loop: sleep until the trigger's next firing, run,
/// repeat — exiting when the stop signal lands or the trigger dries up.
async fn run_task(mut stop: watch::Receiver<bool>, mut task: Task, lock: Arc<dyn DistributedLock>) {
    loop {
        let now = Local::now();
        let Some(next) = task.trigger.next(now) else {
            tracing::warn!(task = %task.name, "scheduler: task has no next trigger");
            return;
        };
        let wait = (next - now).to_std().unwrap_or(Duration::ZERO);
        tokio::select! {
            // Ok means stop flagged; Err means the Scheduler was dropped —
            // both end the loop.
            _ = stop.wait_for(|s| *s) => return,
            _ = tokio::time::sleep(wait) => {}
        }
        invoke(&mut task, &lock).await;
    }
}

/// Runs the task body once, recovering from panics so a panicking task does
/// not stop the scheduler (the panic is logged and the schedule continues).
///
/// When the task declares a [`TaskLock`], the lock is acquired first; if it
/// is held elsewhere (or the backend errors) the tick is **skipped**, and
/// the lock is always released once the body completes — pyfly's
/// `try/finally`, with the TTL as the crash safety valve.
async fn invoke(task: &mut Task, lock: &Arc<dyn DistributedLock>) {
    if let Some(spec) = task.lock.clone() {
        match lock.try_acquire(&spec.name, spec.ttl).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    task = %task.name,
                    lock = %spec.name,
                    "scheduler: tick skipped — lock held elsewhere"
                );
                // Mark the tick finished so FixedDelay waits its delay
                // instead of re-contending immediately.
                task.trigger.finished(Local::now());
                return;
            }
            Err(err) => {
                tracing::warn!(
                    task = %task.name,
                    lock = %spec.name,
                    err = %err,
                    "scheduler: lock acquire failed; skipping tick"
                );
                task.trigger.finished(Local::now());
                return;
            }
        }
    }
    // Spawning the run isolates panics: a panic surfaces as a JoinError
    // instead of unwinding through the dispatch loop — Go's `recover()`.
    let joined = tokio::spawn((task.run)()).await;
    // Release before result handling — held even when the body panicked
    // (the run ended either way; pyfly releases in `finally`).
    if let Some(spec) = &task.lock {
        if let Err(err) = lock.release(&spec.name).await {
            tracing::warn!(
                task = %task.name,
                lock = %spec.name,
                err = %err,
                "scheduler: lock release failed (TTL will reap it)"
            );
        }
    }
    match joined {
        Ok(result) => {
            task.trigger.finished(Local::now());
            if let Err(err) = result {
                tracing::warn!(task = %task.name, err = %err, "scheduler: task error");
            }
        }
        Err(join_err) => {
            tracing::error!(
                task = %task.name,
                panic = %panic_message(join_err),
                "scheduler: task panic"
            );
        }
    }
}

/// Extracts a printable message from a panicked task's join error.
fn panic_message(err: tokio::task::JoinError) -> String {
    match err.try_into_panic() {
        Ok(payload) => {
            if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_string()
            }
        }
        Err(err) => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// Spawns a stop call after `after`, then awaits start — the Rust stand-in
    /// for Go's `context.WithTimeout` around `Start(ctx)`.
    async fn run_for(s: &Arc<Scheduler>, after: Duration) {
        let stopper = Arc::clone(s);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            stopper.stop();
        });
        s.start().await;
    }

    // Port of Go TestSchedulerFixedRate.
    #[tokio::test]
    async fn scheduler_fixed_rate() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_rate("ping", Duration::from_millis(30), move || {
            let h = Arc::clone(&h);
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        run_for(&s, Duration::from_millis(100)).await;
        assert!(
            hits.load(Ordering::SeqCst) >= 2,
            "hits: {}",
            hits.load(Ordering::SeqCst)
        );
    }

    // Port of Go TestSchedulerFixedDelay.
    #[tokio::test]
    async fn scheduler_fixed_delay() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_delay("slow", Duration::from_millis(10), move || {
            let h = Arc::clone(&h);
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                Ok(())
            }
        });
        run_for(&s, Duration::from_millis(80)).await;
        let h = hits.load(Ordering::SeqCst);
        assert!((2..=5).contains(&h), "hits: {h} (expected 2-5)");
    }

    // Port of Go TestSchedulerPanicRecovers.
    #[tokio::test]
    async fn scheduler_panic_recovers() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_rate("boom", Duration::from_millis(10), move || {
            let h = Arc::clone(&h);
            async move {
                let n = h.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    panic!("oops");
                }
                Ok(())
            }
        });
        run_for(&s, Duration::from_millis(60)).await;
        assert!(
            hits.load(Ordering::SeqCst) >= 2,
            "hits: {}",
            hits.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn scheduler_logs_task_errors_and_continues() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_rate("flaky", Duration::from_millis(10), move || {
            let h = Arc::clone(&h);
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                Err::<(), TaskError>("synthetic failure".into())
            }
        });
        run_for(&s, Duration::from_millis(60)).await;
        assert!(hits.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn scheduler_stop_before_start_returns_immediately() {
        let s = Scheduler::new();
        s.stop();
        s.stop(); // Safe to call multiple times.
        tokio::time::timeout(Duration::from_millis(200), s.start())
            .await
            .expect("start should return immediately after stop");
    }

    #[test]
    fn scheduler_cron_rejects_invalid_expression() {
        let s = Scheduler::new();
        let err = s
            .cron("bad", "60 * * * *", || async { Ok(()) })
            .unwrap_err();
        assert!(matches!(err, CronError::BadValue { .. }));
    }

    #[test]
    fn fixed_rate_trigger_math() {
        let now = local(2026, 5, 4, 9, 0, 0);

        // Zero period degrades to one-second polling, as in Go.
        let mut t = FixedRateTrigger {
            start: now,
            period: Duration::ZERO,
        };
        assert_eq!(t.next(now).unwrap(), now + chrono::Duration::seconds(1));

        // Before the anchor, the anchor itself is next.
        let start = now + chrono::Duration::seconds(5);
        let mut t = FixedRateTrigger {
            start,
            period: Duration::from_secs(10),
        };
        assert_eq!(t.next(now).unwrap(), start);

        // 25ms past the anchor with a 10ms period → the 30ms grid point.
        let start = now - chrono::Duration::milliseconds(25);
        let mut t = FixedRateTrigger {
            start,
            period: Duration::from_millis(10),
        };
        assert_eq!(
            t.next(now).unwrap(),
            start + chrono::Duration::milliseconds(30)
        );

        // Exactly on the anchor → one full period ahead.
        let mut t = FixedRateTrigger {
            start: now,
            period: Duration::from_secs(30),
        };
        assert_eq!(t.next(now).unwrap(), now + chrono::Duration::seconds(30));
    }

    #[test]
    fn fixed_delay_trigger_math() {
        let now = local(2026, 5, 4, 9, 0, 0);
        let mut t = FixedDelayTrigger::new(Duration::from_secs(10));

        // Never run → immediate dispatch.
        assert_eq!(t.next(now).unwrap(), now);

        // After a finish, next is finish + delay.
        let finished_at = now + chrono::Duration::seconds(3);
        t.finished(finished_at);
        assert_eq!(
            t.next(now).unwrap(),
            finished_at + chrono::Duration::seconds(10)
        );
    }

    #[test]
    fn cron_trigger_delegates_to_expr() {
        let expr = parse_cron("0 9 * * *").unwrap();
        let mut t = CronTrigger { expr };
        let from = local(2026, 5, 4, 8, 30, 0);
        let next = t.next(from).unwrap();
        assert_eq!(next, local(2026, 5, 4, 9, 0, 0));
    }

    #[test]
    fn public_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_send<T: Send>() {}
        assert_send_sync::<Scheduler>();
        assert_send_sync::<CronExpr>();
        assert_send_sync::<CronError>();
        assert_send::<Task>();
        assert_send::<Box<dyn Trigger>>();
    }

    // ---- pyfly parity: zones, initial delays, locks, descriptors ----

    /// A lock that always acquires and records releases — pyfly's
    /// `_AllowLock`.
    #[derive(Default)]
    struct AllowLock {
        released: Mutex<Vec<String>>,
        acquired: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::DistributedLock for AllowLock {
        async fn try_acquire(&self, name: &str, _ttl: Duration) -> Result<bool, crate::LockError> {
            self.acquired.lock().unwrap().push(name.to_string());
            Ok(true)
        }

        async fn release(&self, name: &str) -> Result<(), crate::LockError> {
            self.released.lock().unwrap().push(name.to_string());
            Ok(())
        }
    }

    /// A lock that always denies and records releases — pyfly's `_DenyLock`.
    #[derive(Default)]
    struct DenyLock {
        released: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::DistributedLock for DenyLock {
        async fn try_acquire(&self, _name: &str, _ttl: Duration) -> Result<bool, crate::LockError> {
            Ok(false)
        }

        async fn release(&self, name: &str) -> Result<(), crate::LockError> {
            self.released.lock().unwrap().push(name.to_string());
            Ok(())
        }
    }

    // Port of pyfly test_invoke_skips_when_lock_denied.
    #[tokio::test]
    async fn invoke_skips_when_lock_denied() {
        let lock = Arc::new(DenyLock::default());
        let s = Arc::new(Scheduler::with_lock(Arc::clone(&lock) as Arc<_>));
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.register(
            Task::new(
                "guarded",
                FixedDelayTrigger::new(Duration::from_millis(5)),
                {
                    move || {
                        let h = Arc::clone(&h);
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }
                    }
                },
            )
            .with_lock("L", Duration::from_secs(5)),
        );
        run_for(&s, Duration::from_millis(50)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0); // ticks skipped (held elsewhere)
        assert!(lock.released.lock().unwrap().is_empty()); // never acquired -> never released
    }

    // Port of pyfly test_invoke_runs_and_releases_when_acquired.
    #[tokio::test]
    async fn invoke_runs_and_releases_when_acquired() {
        let lock = Arc::new(AllowLock::default());
        let s = Arc::new(Scheduler::with_lock(Arc::clone(&lock) as Arc<_>));
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.register(
            Task::new(
                "guarded",
                FixedDelayTrigger::new(Duration::from_millis(10)),
                {
                    move || {
                        let h = Arc::clone(&h);
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }
                    }
                },
            )
            .with_lock("L", Duration::from_secs(5)),
        );
        run_for(&s, Duration::from_millis(50)).await;
        assert!(hits.load(Ordering::SeqCst) >= 1);
        let acquired = lock.acquired.lock().unwrap().clone();
        let released = lock.released.lock().unwrap().clone();
        assert!(acquired.iter().all(|n| n == "L"));
        assert_eq!(acquired.len(), released.len()); // every acquire released
        assert!(released.contains(&"L".to_string()));
    }

    // Port of pyfly test_invoke_releases_even_on_failure.
    #[tokio::test]
    async fn invoke_releases_even_on_failure() {
        let lock = Arc::new(AllowLock::default());
        let s = Arc::new(Scheduler::with_lock(Arc::clone(&lock) as Arc<_>));
        s.register(
            Task::new(
                "failing",
                FixedDelayTrigger::new(Duration::from_millis(10)),
                || async { Err::<(), TaskError>("boom".into()) },
            )
            .with_lock("L", Duration::from_secs(5)),
        );
        run_for(&s, Duration::from_millis(40)).await;
        assert!(lock.released.lock().unwrap().contains(&"L".to_string())); // released in "finally"
    }

    // A panicking body must still release the lock.
    #[tokio::test]
    async fn invoke_releases_even_on_panic() {
        let lock = Arc::new(AllowLock::default());
        let s = Arc::new(Scheduler::with_lock(Arc::clone(&lock) as Arc<_>));
        s.register(
            Task::new(
                "panicking",
                FixedDelayTrigger::new(Duration::from_millis(10)),
                || async { panic!("oops") },
            )
            .with_lock("L", Duration::from_secs(5)),
        );
        run_for(&s, Duration::from_millis(40)).await;
        assert!(lock.released.lock().unwrap().contains(&"L".to_string()));
    }

    // Unlocked tasks never consult the provider.
    #[tokio::test]
    async fn unlocked_task_skips_lock_provider() {
        let lock = Arc::new(AllowLock::default());
        let s = Arc::new(Scheduler::with_lock(Arc::clone(&lock) as Arc<_>));
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_rate("free", Duration::from_millis(10), move || {
            let h = Arc::clone(&h);
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        run_for(&s, Duration::from_millis(40)).await;
        assert!(hits.load(Ordering::SeqCst) >= 1);
        assert!(lock.acquired.lock().unwrap().is_empty());
    }

    #[test]
    fn fixed_delay_initial_delay_math() {
        let now = local(2026, 5, 4, 9, 0, 0);
        let mut t =
            FixedDelayTrigger::with_initial_delay(Duration::from_secs(10), Duration::from_secs(3));
        // Never run → first dispatch waits the initial delay.
        assert_eq!(t.next(now).unwrap(), now + chrono::Duration::seconds(3));
        // After a finish, the regular delay applies.
        t.finished(now + chrono::Duration::seconds(4));
        assert_eq!(
            t.next(now).unwrap(),
            now + chrono::Duration::seconds(4) + chrono::Duration::seconds(10)
        );
    }

    #[tokio::test]
    async fn fixed_rate_initial_delay_defers_first_run() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        // Initial delay far beyond the test window: must never fire.
        s.fixed_rate_with_initial_delay(
            "deferred",
            Duration::from_millis(5),
            Duration::from_secs(60),
            move || {
                let h = Arc::clone(&h);
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );
        run_for(&s, Duration::from_millis(40)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn fixed_delay_initial_delay_defers_first_run() {
        let s = Arc::new(Scheduler::new());
        let hits = Arc::new(AtomicU32::new(0));
        let h = Arc::clone(&hits);
        s.fixed_delay_with_initial_delay(
            "deferred",
            Duration::from_millis(5),
            Duration::from_secs(60),
            move || {
                let h = Arc::clone(&h);
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );
        run_for(&s, Duration::from_millis(40)).await;
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn zoned_cron_trigger_evaluates_in_zone() {
        let ny: chrono_tz::Tz = "America/New_York".parse().unwrap();
        let expr = parse_cron("0 0 * * *").unwrap();
        let mut zoned = ZonedCronTrigger::new(expr.clone(), ny);
        let now = Local::now();
        // Identical instant to evaluating the expression in the zone and
        // converting back — independent of the machine's local zone.
        let expected = expr
            .next(now.with_timezone(&ny))
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(zoned.next(now).unwrap(), expected);
        // pyfly's zoneless default is UTC.
        let utc_trigger = ZonedCronTrigger::utc(parse_cron("0 0 * * *").unwrap());
        assert_eq!(utc_trigger.zone, chrono_tz::Tz::UTC);
        // CronTrigger::zoned upgrades the legacy trigger.
        let upgraded = CronTrigger {
            expr: parse_cron("0 0 * * *").unwrap(),
        }
        .zoned(ny);
        assert_eq!(upgraded.zone, ny);
    }

    #[test]
    fn cron_in_zone_rejects_unknown_zone_and_bad_expr() {
        let s = Scheduler::new();
        let err = s
            .cron_in_zone("bad-zone", "0 0 * * *", "Mars/Olympus_Mons", || async {
                Ok(())
            })
            .unwrap_err();
        assert_eq!(err, ScheduleError::UnknownZone("Mars/Olympus_Mons".into()));

        let err = s
            .cron_in_zone("bad-expr", "60 * * * *", "UTC", || async { Ok(()) })
            .unwrap_err();
        assert!(matches!(err, ScheduleError::Cron(_)));
    }

    #[tokio::test]
    async fn cron_in_zone_registers_and_describes() {
        let s = Scheduler::new();
        s.cron_in_zone("ny-open", "30 9 * * 1-5", "America/New_York", || async {
            Ok(())
        })
        .unwrap();
        let tasks = s.tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].trigger,
            TriggerDescriptor::Cron {
                expression: "30 9 * * 1-5".into(),
                zone: Some("America/New_York".into()),
            }
        );
    }

    #[tokio::test]
    async fn tasks_snapshot_covers_all_trigger_kinds_and_survives_start() {
        let s = Arc::new(Scheduler::new());
        s.cron("nightly", "0 2 * * *", || async { Ok(()) }).unwrap();
        s.fixed_rate("emit", Duration::from_secs(30), || async { Ok(()) });
        s.fixed_delay_with_initial_delay(
            "cleanup",
            Duration::from_secs(300),
            Duration::from_secs(5),
            || async { Ok(()) },
        );
        s.register(
            Task::new(
                "guarded",
                FixedDelayTrigger::new(Duration::from_secs(60)),
                || async { Ok(()) },
            )
            .with_lock("jobs:guarded", Duration::from_secs(30)),
        );

        let tasks = s.tasks();
        assert_eq!(tasks.len(), 4);
        assert_eq!(
            tasks[0].trigger,
            TriggerDescriptor::Cron {
                expression: "0 2 * * *".into(),
                zone: None
            }
        );
        assert_eq!(
            tasks[1].trigger,
            TriggerDescriptor::FixedRate {
                interval: Duration::from_secs(30),
                initial_delay: None
            }
        );
        assert_eq!(
            tasks[2].trigger,
            TriggerDescriptor::FixedDelay {
                interval: Duration::from_secs(300),
                initial_delay: Some(Duration::from_secs(5))
            }
        );
        assert_eq!(tasks[3].lock_name.as_deref(), Some("jobs:guarded"));
        assert_eq!(tasks[3].lock_ttl, Some(Duration::from_secs(30)));

        // The snapshot survives start() draining the run queue.
        run_for(&s, Duration::from_millis(20)).await;
        assert_eq!(s.tasks().len(), 4);
    }

    #[test]
    fn fixed_rate_initial_delay_descriptor_records_delay() {
        let s = Scheduler::new();
        s.fixed_rate_with_initial_delay(
            "warmup",
            Duration::from_secs(60),
            Duration::from_secs(10),
            || async { Ok(()) },
        );
        assert_eq!(
            s.tasks()[0].trigger,
            TriggerDescriptor::FixedRate {
                interval: Duration::from_secs(60),
                initial_delay: Some(Duration::from_secs(10)),
            }
        );
    }

    #[test]
    fn descriptors_serialize_in_spring_actuator_shape() {
        let s = Scheduler::new();
        s.fixed_rate_with_initial_delay(
            "warmup",
            Duration::from_secs(30),
            Duration::from_secs(1),
            || async { Ok(()) },
        );
        s.register(
            Task::new(
                "guarded",
                FixedDelayTrigger::new(Duration::from_secs(60)),
                || async { Ok(()) },
            )
            .with_lock("L", TaskLock::DEFAULT_TTL),
        );
        let json = serde_json::to_value(s.tasks()).unwrap();
        assert_eq!(
            json[0],
            serde_json::json!({
                "name": "warmup",
                "trigger": {"type": "fixedRate", "interval": 30_000, "initialDelay": 1_000},
            })
        );
        assert_eq!(
            json[1],
            serde_json::json!({
                "name": "guarded",
                "trigger": {"type": "fixedDelay", "interval": 60_000},
                "lockName": "L",
                "lockTtl": 60_000,
            })
        );
    }

    #[test]
    fn task_lock_defaults_match_pyfly() {
        // pyfly: lock_ttl defaults to 60 seconds.
        assert_eq!(TaskLock::DEFAULT_TTL, Duration::from_secs(60));
        assert_eq!(TaskLock::new("j").ttl, Duration::from_secs(60));
        let t = TaskLock::with_ttl("j", Duration::from_secs(5));
        assert_eq!((t.name.as_str(), t.ttl), ("j", Duration::from_secs(5)));
    }

    #[test]
    fn custom_triggers_describe_as_custom() {
        struct Never;
        impl Trigger for Never {
            fn next(&mut self, _now: DateTime<Local>) -> Option<DateTime<Local>> {
                None
            }
        }
        let s = Scheduler::new();
        s.register(Task::new("custom", Never, || async { Ok(()) }));
        assert_eq!(s.tasks()[0].trigger, TriggerDescriptor::Custom);
    }
}
