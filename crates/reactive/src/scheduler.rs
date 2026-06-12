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

//! Execution context for reactive pipelines — the Rust analog of
//! Reactor's `reactor.core.scheduler.Scheduler`.
//!
//! A [`Scheduler`] decides *where* work runs. Reactor offers a small set
//! of canonical schedulers; this module mirrors the three that map
//! cleanly onto the Tokio runtime:
//!
//! | Reactor                     | firefly-reactive            | Tokio mechanism        |
//! |-----------------------------|-----------------------------|------------------------|
//! | `Schedulers.immediate()`    | [`Scheduler::Immediate`]    | run inline, no hop     |
//! | `Schedulers.parallel()`     | [`Scheduler::Parallel`]     | `tokio::spawn`         |
//! | `Schedulers.boundedElastic()` | [`Scheduler::BoundedElastic`] | `spawn_blocking`   |
//!
//! Schedulers are attached to a pipeline with
//! [`Mono::subscribe_on`](crate::Mono::subscribe_on) /
//! [`Mono::publish_on`](crate::Mono::publish_on) (and the [`Flux`](crate::Flux)
//! equivalents). `subscribe_on` affects where the *source* runs;
//! `publish_on` switches the thread for everything *downstream* of the
//! call by hopping items through a channel.

use std::future::Future;

use firefly_kernel::FireflyError;

/// Where a reactive pipeline executes its work.
///
/// Cloneable and cheap to pass around — it carries no state beyond the
/// variant tag. See the [module docs](self) for the mapping to Reactor.
///
/// ```
/// use firefly_reactive::Scheduler;
///
/// let s = Scheduler::Parallel;
/// assert_eq!(s, Scheduler::Parallel);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheduler {
    /// Run inline on the current task with no thread hop. The Rust
    /// analog of `Schedulers.immediate()`.
    #[default]
    Immediate,
    /// Offload onto the Tokio runtime's worker pool via
    /// [`tokio::spawn`]. The analog of `Schedulers.parallel()` — for
    /// CPU-light, latency-sensitive async work.
    Parallel,
    /// Offload onto the blocking pool via [`tokio::task::spawn_blocking`].
    /// The analog of `Schedulers.boundedElastic()` — for blocking or
    /// long-running synchronous work that must not stall async workers.
    BoundedElastic,
}

impl Scheduler {
    /// Runs `f` (a future producing a [`FireflyResult`]) on this
    /// scheduler and awaits the outcome.
    ///
    /// On [`Immediate`](Scheduler::Immediate) the future is awaited
    /// inline. On [`Parallel`](Scheduler::Parallel) it is spawned onto a
    /// worker; on [`BoundedElastic`](Scheduler::BoundedElastic) it is
    /// driven on a dedicated blocking thread via a current-thread
    /// runtime so that even blocking work cannot starve the async pool.
    ///
    /// A panic inside the spawned work is converted into a 500
    /// [`FireflyError`] rather than aborting the process.
    pub(crate) async fn run<F, T>(self, f: F) -> Result<T, FireflyError>
    where
        F: Future<Output = Result<T, FireflyError>> + Send + 'static,
        T: Send + 'static,
    {
        match self {
            Scheduler::Immediate => f.await,
            Scheduler::Parallel => match tokio::spawn(f).await {
                Ok(v) => v,
                Err(e) => Err(join_error("parallel", e)),
            },
            Scheduler::BoundedElastic => {
                let handle = tokio::task::spawn_blocking(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            FireflyError::internal(format!(
                                "bounded-elastic runtime build failed: {e}"
                            ))
                        })?;
                    rt.block_on(f)
                });
                match handle.await {
                    Ok(v) => v,
                    Err(e) => Err(join_error("bounded-elastic", e)),
                }
            }
        }
    }
}

/// Converts a Tokio [`JoinError`](tokio::task::JoinError) into a
/// `FireflyError`, preserving whether the task panicked or was
/// cancelled.
fn join_error(scheduler: &str, e: tokio::task::JoinError) -> FireflyError {
    if e.is_panic() {
        FireflyError::internal(format!("{scheduler} scheduler task panicked"))
    } else {
        FireflyError::internal(format!("{scheduler} scheduler task cancelled"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn immediate_runs_inline() {
        let out = Scheduler::Immediate
            .run(async { Ok::<_, FireflyError>(7) })
            .await;
        assert_eq!(out.unwrap(), 7);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_runs_on_worker() {
        let out = Scheduler::Parallel
            .run(async { Ok::<_, FireflyError>(9) })
            .await;
        assert_eq!(out.unwrap(), 9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_elastic_runs_blocking() {
        let out = Scheduler::BoundedElastic
            .run(async { Ok::<_, FireflyError>(11) })
            .await;
        assert_eq!(out.unwrap(), 11);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn parallel_propagates_error() {
        let out = Scheduler::Parallel
            .run(async { Err::<i32, _>(FireflyError::internal("boom")) })
            .await;
        assert_eq!(out.unwrap_err().status, 500);
    }

    #[test]
    fn default_is_immediate() {
        assert_eq!(Scheduler::default(), Scheduler::Immediate);
    }
}
