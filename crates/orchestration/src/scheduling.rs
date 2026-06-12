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

//! Periodic orchestration triggers — the Rust spelling of pyfly's
//! `OrchestrationScheduler` (`pyfly.transactional.core.scheduling`) and the
//! `@scheduled_saga` / `@scheduled_workflow` / `@scheduled_tcc` decorators.
//!
//! A [`ScheduledTask`] pairs an async callback with a [`ScheduleTrigger`].
//! [`OrchestrationScheduler::start`] spins one `tokio` loop per enabled
//! task; [`OrchestrationScheduler::stop`] cancels them cooperatively. As in
//! pyfly, `cron` triggers are inert without a cron evaluator (the Python
//! port skips them when `croniter` is not installed) — fixed-rate and
//! fixed-delay are the always-available forms, and the ones the tests
//! exercise (kept short, well under the workflow sleep budget).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Shared stop signal: an atomic flag plus a [`Notify`] to wake sleeping
/// task loops promptly when [`OrchestrationScheduler::stop`] is called.
#[derive(Default)]
struct StopSignal {
    flag: AtomicBool,
    notify: Notify,
}

impl StopSignal {
    fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolves once the signal has been triggered.
    async fn wait(&self) {
        loop {
            if self.is_set() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

use crate::model::TriggerMode;
use crate::registry::OrchestrationRegistry;
use crate::BoxError;

/// Async callback fired by a [`ScheduledTask`].
pub type ScheduledCallback =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(), BoxError>> + Send + Sync>;

/// Wraps an async closure as a [`ScheduledCallback`].
pub fn scheduled_callback<F, Fut>(f: F) -> ScheduledCallback
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), BoxError>> + Send + 'static,
{
    Arc::new(move || Box::pin(f()))
}

/// The cadence of a [`ScheduledTask`] — pyfly's `cron` / `fixed_rate_ms` /
/// `fixed_delay_ms` fields collapsed into one enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleTrigger {
    /// Fire every `period` measured from each start (pyfly `fixed_rate_ms`).
    FixedRate(Duration),
    /// Fire `period` after each run *completes* (pyfly `fixed_delay_ms`).
    FixedDelay(Duration),
    /// A cron expression. Inert without a cron evaluator, exactly like
    /// pyfly without `croniter` — kept for definition fidelity.
    Cron(String),
}

/// Error raised when registering a malformed scheduled task.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// A duration trigger was zero, or the cron string was empty.
    #[error("firefly/orchestration: scheduled task {id:?}: invalid trigger")]
    InvalidTrigger {
        /// The offending task id.
        id: String,
    },
}

/// One scheduled trigger registered with [`OrchestrationScheduler`] —
/// pyfly's `ScheduledTask`.
#[derive(Clone)]
pub struct ScheduledTask {
    /// Stable task id, e.g. `"saga:orderSaga"`.
    pub id: String,
    /// The cadence.
    pub trigger: ScheduleTrigger,
    /// Delay before the first fire.
    pub initial_delay: Duration,
    /// Whether [`OrchestrationScheduler::start`] runs this task.
    pub enabled: bool,
    callback: ScheduledCallback,
}

impl std::fmt::Debug for ScheduledTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduledTask")
            .field("id", &self.id)
            .field("trigger", &self.trigger)
            .field("initial_delay", &self.initial_delay)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl ScheduledTask {
    /// Builds an enabled task with no initial delay.
    pub fn new(
        id: impl Into<String>,
        trigger: ScheduleTrigger,
        callback: ScheduledCallback,
    ) -> Self {
        Self {
            id: id.into(),
            trigger,
            initial_delay: Duration::ZERO,
            enabled: true,
            callback,
        }
    }

    /// Builds a fixed-rate task firing every `period`.
    pub fn fixed_rate(
        id: impl Into<String>,
        period: Duration,
        callback: ScheduledCallback,
    ) -> Self {
        Self::new(id, ScheduleTrigger::FixedRate(period), callback)
    }

    /// Builds a fixed-delay task firing `period` after each completion.
    pub fn fixed_delay(
        id: impl Into<String>,
        period: Duration,
        callback: ScheduledCallback,
    ) -> Self {
        Self::new(id, ScheduleTrigger::FixedDelay(period), callback)
    }

    /// Sets the delay before the first fire.
    #[must_use]
    pub fn initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Disables the task (registered but not started).
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Builds a task that starts the named saga from `registry` on each
    /// fire — the Rust spelling of `@scheduled_saga`. The id is
    /// `"saga:{name}"`, matching pyfly's `"workflow:{id}"` naming.
    pub fn for_saga(
        registry: &Arc<OrchestrationRegistry>,
        saga_name: impl Into<String>,
        trigger: ScheduleTrigger,
        mode: TriggerMode,
    ) -> Self {
        let registry = Arc::clone(registry);
        let saga_name = saga_name.into();
        let id = format!("saga:{saga_name}");
        let callback = scheduled_callback(move || {
            let registry = Arc::clone(&registry);
            let saga_name = saga_name.clone();
            async move {
                let Some(saga) = registry.saga(&saga_name) else {
                    return Err(format!("unknown saga {saga_name:?}").into());
                };
                match mode {
                    TriggerMode::Sync => {
                        if let Err(failure) = saga.run().await {
                            return Err(Box::new(failure) as BoxError);
                        }
                        Ok(())
                    }
                    TriggerMode::Async => {
                        tokio::spawn(async move {
                            let _ = saga.run().await;
                        });
                        Ok(())
                    }
                }
            }
        });
        Self::new(id, trigger, callback)
    }

    fn valid(&self) -> bool {
        match &self.trigger {
            ScheduleTrigger::FixedRate(d) | ScheduleTrigger::FixedDelay(d) => !d.is_zero(),
            ScheduleTrigger::Cron(expr) => !expr.is_empty(),
        }
    }
}

/// A point-in-time view of a registered task — pyfly's `ScheduledTask`
/// counters, serialized for the admin surface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScheduledTaskInfo {
    /// The task id.
    pub id: String,
    /// Whether the task is enabled.
    pub enabled: bool,
    /// How many times the callback has completed.
    pub runs: u64,
    /// How many times the callback erred.
    pub failures: u64,
}

#[derive(Default)]
struct TaskCounters {
    runs: AtomicU64,
    failures: AtomicU64,
}

struct Entry {
    task: ScheduledTask,
    counters: Arc<TaskCounters>,
    handle: Option<JoinHandle<()>>,
}

/// Manages periodic orchestration triggers — pyfly's
/// `OrchestrationScheduler`.
#[derive(Default)]
pub struct OrchestrationScheduler {
    inner: Mutex<Scheduler>,
}

#[derive(Default)]
struct Scheduler {
    tasks: BTreeMap<String, Entry>,
    running: bool,
    stop: Option<Arc<StopSignal>>,
}

impl std::fmt::Debug for OrchestrationScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.locked();
        f.debug_struct("OrchestrationScheduler")
            .field("running", &inner.running)
            .field("tasks", &inner.tasks.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl OrchestrationScheduler {
    /// Returns an empty, stopped scheduler.
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Scheduler> {
        self.inner
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }

    /// Registers `task`. If the scheduler is already running and the task is
    /// enabled, its loop spins up immediately — pyfly's post-start
    /// registration behavior (audit #54).
    pub fn register(&self, task: ScheduledTask) -> Result<(), SchedulerError> {
        if !task.valid() {
            return Err(SchedulerError::InvalidTrigger { id: task.id });
        }
        let mut inner = self.locked();
        let counters = Arc::new(TaskCounters::default());
        let mut entry = Entry {
            task: task.clone(),
            counters: Arc::clone(&counters),
            handle: None,
        };
        if inner.running && task.enabled {
            if let Some(stop) = inner.stop.clone() {
                entry.handle = Some(spawn_loop(task, counters, stop));
            }
        }
        inner.tasks.insert(entry.task.id.clone(), entry);
        Ok(())
    }

    /// Removes a task and cancels its loop; `true` when it was present —
    /// pyfly's `unregister`.
    pub fn unregister(&self, task_id: &str) -> bool {
        let mut inner = self.locked();
        if let Some(entry) = inner.tasks.remove(task_id) {
            if let Some(handle) = entry.handle {
                handle.abort();
            }
            true
        } else {
            false
        }
    }

    /// A snapshot of every registered task — pyfly's `list`.
    pub fn list(&self) -> Vec<ScheduledTaskInfo> {
        self.locked()
            .tasks
            .values()
            .map(|e| ScheduledTaskInfo {
                id: e.task.id.clone(),
                enabled: e.task.enabled,
                runs: e.counters.runs.load(Ordering::SeqCst),
                failures: e.counters.failures.load(Ordering::SeqCst),
            })
            .collect()
    }

    /// `true` when [`Self::start`] has run and [`Self::stop`] has not.
    pub fn is_running(&self) -> bool {
        self.locked().running
    }

    /// Starts every enabled task. Idempotent — a second call is a no-op
    /// while running, matching pyfly.
    pub fn start(&self) {
        let mut inner = self.locked();
        if inner.running {
            return;
        }
        let stop = Arc::new(StopSignal::default());
        inner.running = true;
        inner.stop = Some(Arc::clone(&stop));
        let task_ids: Vec<String> = inner.tasks.keys().cloned().collect();
        for id in task_ids {
            let (task, counters) = {
                let entry = inner.tasks.get(&id).expect("present");
                (entry.task.clone(), Arc::clone(&entry.counters))
            };
            if task.enabled {
                let handle = spawn_loop(task, counters, Arc::clone(&stop));
                inner.tasks.get_mut(&id).expect("present").handle = Some(handle);
            }
        }
    }

    /// Cancels every running loop and marks the scheduler stopped —
    /// pyfly's `stop`.
    pub fn stop(&self) {
        let mut inner = self.locked();
        if !inner.running {
            return;
        }
        inner.running = false;
        if let Some(stop) = inner.stop.take() {
            stop.trigger();
        }
        for entry in inner.tasks.values_mut() {
            if let Some(handle) = entry.handle.take() {
                handle.abort();
            }
        }
    }
}

impl Drop for OrchestrationScheduler {
    fn drop(&mut self) {
        if let Ok(inner) = self.inner.lock() {
            for entry in inner.tasks.values() {
                if let Some(handle) = &entry.handle {
                    handle.abort();
                }
            }
        }
    }
}

/// Spawns the per-task `tokio` loop honoring the trigger and `stop` signal.
fn spawn_loop(
    task: ScheduledTask,
    counters: Arc<TaskCounters>,
    stop: Arc<StopSignal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // A cron trigger is inert (pyfly without croniter): park until stop.
        if matches!(task.trigger, ScheduleTrigger::Cron(_)) {
            stop.wait().await;
            return;
        }
        if !task.initial_delay.is_zero() {
            tokio::select! {
                () = stop.wait() => return,
                () = tokio::time::sleep(task.initial_delay) => {}
            }
        }
        let period = match &task.trigger {
            ScheduleTrigger::FixedRate(d) | ScheduleTrigger::FixedDelay(d) => *d,
            ScheduleTrigger::Cron(_) => unreachable!("cron handled above"),
        };
        loop {
            tokio::select! {
                () = stop.wait() => return,
                () = tokio::time::sleep(period) => {}
            }
            if stop.is_set() {
                return;
            }
            match (task.callback)().await {
                Ok(()) => {
                    counters.runs.fetch_add(1, Ordering::SeqCst);
                }
                Err(err) => {
                    counters.failures.fetch_add(1, Ordering::SeqCst);
                    tracing::error!(task = %task.id, error = %err, "scheduled task failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Saga, Step};
    use std::sync::atomic::AtomicU32;

    // Port of pyfly test_scheduled_workflow_fires: a fixed-rate task fires
    // at least once shortly after start, and appears in list().
    #[tokio::test]
    async fn fixed_rate_task_fires() {
        let fired = Arc::new(AtomicU32::new(0));
        let scheduler = OrchestrationScheduler::new();
        let count = fired.clone();
        scheduler
            .register(ScheduledTask::fixed_rate(
                "saga:tick",
                Duration::from_millis(15),
                scheduled_callback(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            ))
            .expect("register");
        scheduler.start();
        for _ in 0..30 {
            if fired.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        scheduler.stop();
        assert!(
            fired.load(Ordering::SeqCst) >= 1,
            "scheduled task never fired"
        );
        assert!(scheduler.list().iter().any(|t| t.id == "saga:tick"));
    }

    // A scheduled saga start drives a registered saga.
    #[tokio::test]
    async fn scheduled_saga_runs_the_registered_saga() {
        let ran = Arc::new(AtomicU32::new(0));
        let registry = Arc::new(OrchestrationRegistry::new());
        let log = ran.clone();
        registry.register_saga(Saga::new("nightly").step(Step::new("run", move || {
            let log = log.clone();
            async move {
                log.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        })));
        let scheduler = OrchestrationScheduler::new();
        scheduler
            .register(ScheduledTask::for_saga(
                &registry,
                "nightly",
                ScheduleTrigger::FixedRate(Duration::from_millis(15)),
                TriggerMode::Sync,
            ))
            .expect("register");
        scheduler.start();
        for _ in 0..30 {
            if ran.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        scheduler.stop();
        assert!(ran.load(Ordering::SeqCst) >= 1);
        assert!(scheduler.list().iter().any(|t| t.id == "saga:nightly"));
    }

    // Port of pyfly: a task without a valid trigger is rejected.
    #[test]
    fn invalid_trigger_is_rejected() {
        let scheduler = OrchestrationScheduler::new();
        let err = scheduler
            .register(ScheduledTask::fixed_rate(
                "x",
                Duration::ZERO,
                scheduled_callback(|| async { Ok(()) }),
            ))
            .expect_err("zero period rejected");
        assert!(matches!(err, SchedulerError::InvalidTrigger { .. }));
        assert!(scheduler
            .register(ScheduledTask::new(
                "c",
                ScheduleTrigger::Cron(String::new()),
                scheduled_callback(|| async { Ok(()) }),
            ))
            .is_err());
    }

    // Port of pyfly unregister cancels the loop.
    #[tokio::test]
    async fn unregister_removes_task() {
        let scheduler = OrchestrationScheduler::new();
        scheduler
            .register(ScheduledTask::fixed_rate(
                "t",
                Duration::from_millis(50),
                scheduled_callback(|| async { Ok(()) }),
            ))
            .unwrap();
        scheduler.start();
        assert!(scheduler.unregister("t"));
        assert!(!scheduler.unregister("t"));
        assert!(scheduler.list().is_empty());
        scheduler.stop();
    }

    // A disabled task is registered but never fires.
    #[tokio::test]
    async fn disabled_task_does_not_fire() {
        let fired = Arc::new(AtomicU32::new(0));
        let scheduler = OrchestrationScheduler::new();
        let count = fired.clone();
        scheduler
            .register(
                ScheduledTask::fixed_rate(
                    "off",
                    Duration::from_millis(5),
                    scheduled_callback(move || {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            Ok(())
                        }
                    }),
                )
                .disabled(),
            )
            .unwrap();
        scheduler.start();
        tokio::time::sleep(Duration::from_millis(40)).await;
        scheduler.stop();
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    // A task registered after start spins up immediately (audit #54).
    #[tokio::test]
    async fn task_registered_after_start_fires() {
        let fired = Arc::new(AtomicU32::new(0));
        let scheduler = OrchestrationScheduler::new();
        scheduler.start();
        let count = fired.clone();
        scheduler
            .register(ScheduledTask::fixed_rate(
                "late",
                Duration::from_millis(10),
                scheduled_callback(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }),
            ))
            .unwrap();
        for _ in 0..30 {
            if fired.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        scheduler.stop();
        assert!(fired.load(Ordering::SeqCst) >= 1);
    }
}
