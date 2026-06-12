//! The task scheduler: triggers, tasks, and the [`Scheduler`] runner.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use futures::future::BoxFuture;
use tokio::sync::watch;

use crate::cron::{parse_cron, CronError, CronExpr};

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
    /// where a panic skips the `lastRun` update.
    fn finished(&mut self, at: DateTime<Local>) {
        let _ = at;
    }
}

/// Fires according to a parsed [`CronExpr`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronTrigger {
    /// The parsed cron expression driving this trigger.
    pub expr: CronExpr,
}

impl Trigger for CronTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        self.expr.next(now)
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
}

/// Fires `delay` after the previous run finished; the first dispatch is
/// immediate. The last finish time is private state fed by
/// [`Trigger::finished`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedDelayTrigger {
    /// Pause between the end of one run and the start of the next.
    pub delay: Duration,
    last_run: Option<DateTime<Local>>,
}

impl FixedDelayTrigger {
    /// Returns a trigger that has never run (first dispatch is immediate).
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            last_run: None,
        }
    }
}

impl Trigger for FixedDelayTrigger {
    fn next(&mut self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        match self.last_run {
            None => Some(now),
            Some(last) => last.checked_add_signed(chrono::Duration::from_std(self.delay).ok()?),
        }
    }

    fn finished(&mut self, at: DateTime<Local>) {
        self.last_run = Some(at);
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
        }
    }
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task").field("name", &self.name).finish()
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
    stop: watch::Sender<bool>,
}

impl Scheduler {
    /// Returns an empty scheduler.
    pub fn new() -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            tasks: Mutex::new(Vec::new()),
            stop,
        }
    }

    /// Adds a task — must be called before [`Scheduler::start`].
    pub fn register(&self, task: Task) {
        self.tasks
            .lock()
            .expect("scheduler tasks poisoned")
            .push(task);
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
            handles.push(tokio::spawn(run_task(self.stop.subscribe(), task)));
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
async fn run_task(mut stop: watch::Receiver<bool>, mut task: Task) {
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
        invoke(&mut task).await;
    }
}

/// Runs the task body once, recovering from panics so a panicking task does
/// not stop the scheduler (the panic is logged and the schedule continues).
async fn invoke(task: &mut Task) {
    // Spawning the run isolates panics: a panic surfaces as a JoinError
    // instead of unwinding through the dispatch loop — Go's `recover()`.
    match tokio::spawn((task.run)()).await {
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
}
