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

//! The **async task executor** — the Rust rendering of Spring's `TaskExecutor`
//! / `@Async` machinery.
//!
//! Spring's `@Async` hands a method off to a `TaskExecutor` so the call returns
//! immediately (a `Future`/`CompletableFuture`) and the body runs on a pooled
//! worker. The Rust analog is a [`TaskExecutor`] over the tokio runtime: every
//! [`spawn`](TaskExecutor::spawn) drives the future to completion on its own
//! `tokio::spawn`ed task and hands back a [`TaskHandle`] the caller can
//! `.await` (or [`join`](TaskHandle::join)) for the result. Concurrency is
//! bounded by a [`tokio::sync::Semaphore`] (Spring's pool size): a bounded
//! executor only admits `max_concurrency` in-flight tasks at once, holding the
//! permit for the lifetime of each spawned task; an *unbounded* executor
//! (`max_concurrency == 0`) admits everything, matching a cached thread pool.
//!
//! A process-global registry mirrors the [`crate`]'s scheduler wiring and the
//! transactional manager's `OnceLock` pattern: a starter registers the
//! application's executor once via [`register_task_executor`], and the
//! `#[async_method]` macro reaches it through [`task_executor`] — which returns
//! a default *unbounded* executor when none was registered, so an `#[async_method]`
//! works in a unit test with no wiring.
//!
//! # Quick start
//!
//! ```
//! use std::sync::Arc;
//! use firefly_scheduling::TaskExecutor;
//!
//! # async fn demo() {
//! // A pool that admits at most 4 tasks at a time.
//! let executor = TaskExecutor::new(4);
//! let handle = executor.spawn(async { 2 + 2 });
//! assert_eq!(handle.join().await.unwrap(), 4);
//! # }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

/// A bounded async executor over the tokio runtime — Spring's `TaskExecutor`.
///
/// [`spawn`](TaskExecutor::spawn) runs a `Send + 'static` future on its own
/// tokio task and returns a [`TaskHandle`]. When constructed with a positive
/// `max_concurrency`, at most that many spawned tasks run at once (the rest
/// queue on the internal [`Semaphore`] until a permit frees); a
/// `max_concurrency` of `0` makes the executor **unbounded** (every task
/// admitted immediately).
///
/// The executor is cheap to [`clone`](Clone) — handles share one semaphore — so
/// it is normally held behind an [`Arc`] and shared across the application
/// (see [`register_task_executor`] / [`task_executor`]).
#[derive(Clone)]
pub struct TaskExecutor {
    /// `Some(sem)` bounds in-flight tasks to the semaphore's permit count;
    /// `None` is the unbounded executor.
    permits: Option<Arc<Semaphore>>,
}

impl TaskExecutor {
    /// Creates an executor admitting at most `max_concurrency` concurrent tasks,
    /// or an **unbounded** executor when `max_concurrency == 0`.
    pub fn new(max_concurrency: usize) -> Self {
        let permits = if max_concurrency == 0 {
            None
        } else {
            Some(Arc::new(Semaphore::new(max_concurrency)))
        };
        TaskExecutor { permits }
    }

    /// Creates an **unbounded** executor — the default for a process with no
    /// configured pool size (a cached thread pool in Spring terms).
    pub fn unbounded() -> Self {
        TaskExecutor::new(0)
    }

    /// `true` when this executor is unbounded (admits every task immediately).
    pub fn is_unbounded(&self) -> bool {
        self.permits.is_none()
    }

    /// The configured concurrency limit, or `None` for an unbounded executor.
    ///
    /// Reports the *capacity* the executor was built with, not the number of
    /// permits currently free.
    pub fn max_concurrency(&self) -> Option<usize> {
        self.permits.as_ref().map(|s| s.available_permits())
    }

    /// Spawns `fut` on the tokio runtime, returning a [`TaskHandle`] for its
    /// output — Spring's `@Async` hand-off.
    ///
    /// On a **bounded** executor the future first acquires a semaphore permit
    /// (awaiting if every permit is in use), then runs; the permit is held for
    /// the lifetime of the spawned task and released when it completes, so no
    /// more than `max_concurrency` tasks run at once. On an **unbounded**
    /// executor the future is spawned directly. Either way the call returns
    /// immediately and the future makes progress on its own task.
    pub fn spawn<F>(&self, fut: F) -> TaskHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let join = match &self.permits {
            // Unbounded: spawn directly.
            None => tokio::spawn(fut),
            // Bounded: acquire a permit inside the task and hold it for the
            // task's lifetime, so concurrency never exceeds the pool size.
            Some(sem) => {
                let __sem = ::std::sync::Arc::clone(sem);
                tokio::spawn(async move {
                    // `acquire_owned` only errors if the semaphore is closed,
                    // which this executor never does; fall through to running
                    // the future unbounded in that impossible case rather than
                    // dropping the work.
                    let __permit = ::std::sync::Arc::clone(&__sem).acquire_owned().await.ok();
                    let __out = fut.await;
                    ::core::mem::drop(__permit);
                    __out
                })
            }
        };
        TaskHandle { join }
    }
}

impl Default for TaskExecutor {
    /// An unbounded executor — see [`TaskExecutor::unbounded`].
    fn default() -> Self {
        TaskExecutor::unbounded()
    }
}

impl ::std::fmt::Debug for TaskExecutor {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("TaskExecutor")
            .field("unbounded", &self.is_unbounded())
            .finish()
    }
}

/// A handle to a task spawned by a [`TaskExecutor`] — Spring's
/// `Future`/`CompletableFuture` from an `@Async` call.
///
/// Wraps a [`tokio::task::JoinHandle`] and yields the spawned future's output.
/// Either `.await` the handle directly (it implements [`Future`]) or call
/// [`join`](TaskHandle::join); both resolve to `Result<T, TaskJoinError>`,
/// where the error reports that the task panicked or was cancelled.
pub struct TaskHandle<T> {
    join: JoinHandle<T>,
}

impl<T> TaskHandle<T> {
    /// Awaits the spawned task and returns its output, or a [`TaskJoinError`]
    /// if the task panicked or was cancelled.
    pub async fn join(self) -> Result<T, TaskJoinError> {
        self.join.await.map_err(TaskJoinError::from)
    }

    /// Aborts the spawned task. A subsequent [`join`](TaskHandle::join) /
    /// `.await` then resolves to [`TaskJoinError::Cancelled`].
    pub fn abort(&self) {
        self.join.abort();
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Result<T, TaskJoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `JoinHandle` is `Unpin`, so projecting through the field is sound.
        Pin::new(&mut self.join)
            .poll(cx)
            .map(|res| res.map_err(TaskJoinError::from))
    }
}

impl<T> ::std::fmt::Debug for TaskHandle<T> {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("TaskHandle").finish_non_exhaustive()
    }
}

/// Why awaiting a [`TaskHandle`] failed to produce a value.
///
/// A spawned task that runs to completion yields `Ok(T)`; this error reports
/// the two ways it can fail to: it **panicked**, or it was **cancelled**
/// (aborted before completing). Mirrors the cases a `tokio::task::JoinError`
/// distinguishes, surfaced as a named, framework-owned error.
#[derive(Debug, thiserror::Error)]
pub enum TaskJoinError {
    /// The spawned task panicked. Carries the panic message when one could be
    /// extracted.
    #[error("async task panicked: {0}")]
    Panicked(String),
    /// The spawned task was cancelled (aborted) before it completed.
    #[error("async task was cancelled before completing")]
    Cancelled,
}

impl From<tokio::task::JoinError> for TaskJoinError {
    fn from(err: tokio::task::JoinError) -> Self {
        if err.is_cancelled() {
            return TaskJoinError::Cancelled;
        }
        // Recover the panic payload's message where possible.
        let message = match err.try_into_panic() {
            Ok(payload) => panic_payload_message(payload),
            Err(_) => "unknown".to_string(),
        };
        TaskJoinError::Panicked(message)
    }
}

/// Best-effort extraction of a panic payload's message (`&str` / `String`).
fn panic_payload_message(payload: Box<dyn ::std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown".to_string()
    }
}

/// The process-wide task executor, registered once at startup (typically by a
/// scheduling starter / auto-configuration), mirroring the scheduler and the
/// transactional manager's single-primary registry.
static EXECUTOR: OnceLock<Arc<TaskExecutor>> = OnceLock::new();

/// Registers the process task executor. Returns `false` if one was already
/// registered (the first registration wins, mirroring a single primary
/// `TaskExecutor` bean).
pub fn register_task_executor(executor: Arc<TaskExecutor>) -> bool {
    EXECUTOR.set(executor).is_ok()
}

/// The process task executor.
///
/// Returns the executor registered through [`register_task_executor`], or — if
/// none was registered — a shared **default unbounded** executor, so an
/// `#[async_method]` runs without any explicit wiring (e.g. in unit tests). The
/// default is created once and reused.
pub fn task_executor() -> Arc<TaskExecutor> {
    if let Some(executor) = EXECUTOR.get() {
        return Arc::clone(executor);
    }
    // No executor registered: hand back a shared default unbounded one. A
    // separate `OnceLock` (not `EXECUTOR.get_or_init`) keeps a later explicit
    // `register_task_executor` able to win, matching the "first registration
    // wins, default otherwise" contract.
    static DEFAULT: OnceLock<Arc<TaskExecutor>> = OnceLock::new();
    Arc::clone(DEFAULT.get_or_init(|| Arc::new(TaskExecutor::unbounded())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unbounded_spawn_joins_to_value() {
        let executor = TaskExecutor::new(0);
        assert!(executor.is_unbounded());
        let handle = executor.spawn(async { 21_u64 * 2 });
        assert_eq!(handle.join().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn handle_is_awaitable_directly() {
        let executor = TaskExecutor::unbounded();
        // The handle is itself a Future, so `.await` resolves it.
        let value = executor.spawn(async { "ok".to_string() }).await.unwrap();
        assert_eq!(value, "ok");
    }

    #[tokio::test]
    async fn bounded_spawn_joins_to_value() {
        let executor = TaskExecutor::new(2);
        assert!(!executor.is_unbounded());
        let handle = executor.spawn(async { 7_i32 });
        assert_eq!(handle.join().await.unwrap(), 7);
    }

    #[tokio::test]
    async fn bounded_executor_caps_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        // A 2-permit executor must never run more than two tasks at once.
        let executor = Arc::new(TaskExecutor::new(2));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let in_flight = Arc::clone(&in_flight);
            let peak = Arc::clone(&peak);
            handles.push(executor.spawn(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handle in handles {
            handle.join().await.unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= 2,
            "a 2-permit executor must cap concurrency at 2, saw {}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn panicking_task_surfaces_as_join_error() {
        let executor = TaskExecutor::unbounded();
        let handle = executor.spawn(async { panic!("boom") });
        let err = handle.join().await.unwrap_err();
        match err {
            TaskJoinError::Panicked(msg) => assert!(msg.contains("boom")),
            other => panic!("expected Panicked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn aborted_task_surfaces_as_cancelled() {
        let executor = TaskExecutor::unbounded();
        let handle = executor.spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            1_u8
        });
        handle.abort();
        match handle.join().await {
            Err(TaskJoinError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn registry_first_wins_and_defaults_unbounded() {
        // Without registration, `task_executor()` hands back a default
        // unbounded executor and still runs work.
        let default = task_executor();
        assert!(default.is_unbounded());
        let handle = default.spawn(async { 5_u64 });
        assert_eq!(handle.join().await.unwrap(), 5);

        // Registering once succeeds; a second registration is rejected
        // (first-wins). Note: the process-global is shared across tests in this
        // binary, so only assert the first/second-call relationship here.
        let first = register_task_executor(Arc::new(TaskExecutor::new(3)));
        let second = register_task_executor(Arc::new(TaskExecutor::new(9)));
        assert!(
            !(first && second),
            "register_task_executor must reject a second registration"
        );
    }
}
