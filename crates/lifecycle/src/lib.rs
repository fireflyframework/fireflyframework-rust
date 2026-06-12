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

//! firefly-lifecycle — Application run orchestrator with signal trap + drain.
//!
//! This crate is the Rust port of the Go `lifecycle` module (Java original:
//! `SpringApplication.run()`, .NET analog: `IHost` + `IHostedService`). It
//! provides the canonical **application orchestrator** every Firefly Rust
//! service uses. It owns:
//!
//! * Ordered start hooks ([`Application::on_start`]).
//! * Long-running server tasks ([`Application::on_server`]) — multiple servers
//!   allowed, each gets its own tokio task and a [`ShutdownSignal`] to drain on.
//! * Reverse-ordered stop hooks ([`Application::on_stop`]).
//! * Signal trap (`SIGINT` / ctrl-c + `SIGTERM` on Unix) via `tokio::signal`.
//! * Programmatic shutdown via [`ShutdownHandle`] — the Rust analog of
//!   cancelling the `context.Context` passed to Go's `Run`, so tests never
//!   need real signals.
//! * Drain budget (default 30 s, [`DRAIN_TIMEOUT`]) granted to server drain +
//!   stop hooks.
//! * Failure rollback — a start hook error triggers the stop hooks before the
//!   error is returned.
//!
//! # Lifecycle diagram
//!
//! ```text
//! run()
//!    │
//!    ├─ for each on_start: run hook (in registration order)
//!    │     │
//!    │     ├─ on error: rollback by running on_stop hooks; return joined err
//!    │
//!    ├─ for each on_server: spawn server task (receives a ShutdownSignal)
//!    │
//!    ├─ block on:
//!    │     │  ShutdownHandle::shutdown()        → trigger = Cancelled
//!    │     │  OS signal (ctrl-c / SIGTERM)      → trigger = none
//!    │     │  first server task exit            → trigger = its error, if any
//!    │
//!    ├─ derive drain deadline (now + drain budget)
//!    ├─ fire ShutdownSignal; await server tasks until the deadline
//!    ├─ run on_stop hooks in REVERSE order under the remaining budget
//!    └─ return joined errors (trigger ⊕ servers ⊕ stops)
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use firefly_lifecycle::{Application, HookResult, ShutdownSignal};
//!
//! async fn connect_broker() -> HookResult {
//!     Ok(())
//! }
//!
//! async fn flush_broker() -> HookResult {
//!     Ok(())
//! }
//!
//! async fn api_server(shutdown: ShutdownSignal) -> HookResult {
//!     // Real services pass `shutdown.wait()` to e.g.
//!     // `axum::serve(listener, router).with_graceful_shutdown(shutdown.wait())`.
//!     shutdown.wait().await;
//!     Ok(())
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let app = Application::new("orders")
//!         .on_start(connect_broker)
//!         .on_server("api", api_server)
//!         .on_stop(flush_broker);
//!
//!     let handle = app.shutdown_handle(); // programmatic stop, e.g. from tests
//!     let _ = handle;                     // or hand it to an admin endpoint
//!     app.run().await?;
//!     Ok(())
//! }
//! ```
//!
//! # Differences from the Go port
//!
//! * Go's `OnHTTP(addr, handler)` is generalised into
//!   [`Application::on_server`]: a named long-running task that receives a
//!   [`ShutdownSignal`] and is expected to return when it fires. This keeps
//!   the crate dependency-light and lets any server (axum, gRPC, a consumer
//!   loop) participate in the same start / drain contract.
//! * Go's `WithLogger(*slog.Logger)` has no equivalent — lifecycle events are
//!   emitted through the global [`tracing`] subscriber.
//! * Go's `Signals []os.Signal` field becomes
//!   [`Application::with_signal_trap`]; the trapped set is fixed to
//!   ctrl-c (`SIGINT`) + `SIGTERM`, the Go defaults.
//! * Go's advisory `context.Context` drain deadline becomes a hard
//!   `tokio::time::timeout`: a stop hook that outlives the budget is
//!   cancelled and reported as a [`LifecycleError::StopHook`] error. Every
//!   stop hook is still polled at least once, so prompt hooks always run.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// Boxed error type returned by start hooks, stop hooks, and server tasks —
/// the Rust spelling of Go's plain `error` return.
pub type HookError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result alias for hooks and server tasks. A hook is a single start or stop
/// step; `Ok(())` means the step succeeded.
pub type HookResult = Result<(), HookError>;

type BoxFuture = Pin<Box<dyn Future<Output = HookResult> + Send + 'static>>;
type Hook = Box<dyn FnOnce() -> BoxFuture + Send + 'static>;
type ServerFn = Box<dyn FnOnce(ShutdownSignal) -> BoxFuture + Send + 'static>;

/// The canonical default drain budget — services have 30 s to drain before
/// the runner force-cancels everything. Mirrors Go's `DrainTimeout`.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

fn fmt_joined(errs: &[LifecycleError]) -> String {
    errs.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Error returned by [`Application::run`]. Mirrors the joined-error contract
/// of the Go port: every failing phase contributes one entry, and
/// [`LifecycleError::Joined`] plays the role of Go's `errors.Join` (its
/// `Display` joins the parts with `\n`, exactly like Go).
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// Shutdown was requested programmatically through a [`ShutdownHandle`].
    /// This is the analog of Go's `context.Canceled` trigger and is the
    /// normal outcome of a handle-driven stop — use
    /// [`LifecycleError::is_cancelled`] to treat it as clean.
    #[error("context cancelled")]
    Cancelled,
    /// A start hook failed. `index` is the registration index;
    /// formats as Go's `start hook %d: %w`.
    #[error("start hook {index}: {source}")]
    StartHook {
        /// Registration index of the failing start hook.
        index: usize,
        /// The error the hook returned.
        #[source]
        source: HookError,
    },
    /// A stop hook failed (or was cancelled by the drain deadline).
    /// `index` is the registration index; formats as Go's `stop hook %d: %w`.
    #[error("stop hook {index}: {source}")]
    StopHook {
        /// Registration index of the failing stop hook.
        index: usize,
        /// The error the hook returned, or `tokio::time::error::Elapsed`
        /// when the drain budget expired before the hook completed.
        #[source]
        source: HookError,
    },
    /// A server task returned an error. Plays the role of Go's
    /// `http %s: %w` listen/serve failure.
    #[error("server {name}: {source}")]
    Server {
        /// The name the server was registered under.
        name: String,
        /// The error the server task returned.
        #[source]
        source: HookError,
    },
    /// The drain budget elapsed while server tasks were still running.
    /// Plays the role of Go's `http shutdown %s: context deadline exceeded`.
    #[error("drain timeout elapsed with {pending} server task(s) still running")]
    DrainTimedOut {
        /// Number of server tasks that had not finished when the budget
        /// expired.
        pending: usize,
    },
    /// Several phases failed; the analog of Go's `errors.Join`.
    #[error("{}", fmt_joined(.0))]
    Joined(Vec<LifecycleError>),
}

impl LifecycleError {
    /// Joins a list of errors into zero (`None`), one, or a
    /// [`LifecycleError::Joined`] — the analog of Go's `errors.Join`.
    fn join(mut errs: Vec<LifecycleError>) -> Option<LifecycleError> {
        match errs.len() {
            0 => None,
            1 => Some(errs.remove(0)),
            _ => Some(LifecycleError::Joined(errs)),
        }
    }

    /// Reports whether this error is — or joins — [`LifecycleError::Cancelled`],
    /// the analog of Go's `errors.Is(err, context.Canceled)`.
    pub fn is_cancelled(&self) -> bool {
        match self {
            LifecycleError::Cancelled => true,
            LifecycleError::Joined(errs) => errs.iter().any(LifecycleError::is_cancelled),
            _ => false,
        }
    }

    /// Returns the flat list of leaf errors: a [`LifecycleError::Joined`]
    /// yields its (recursively flattened) parts, any other variant yields
    /// itself. The analog of unwrapping Go's joined error tree.
    pub fn flattened(&self) -> Vec<&LifecycleError> {
        match self {
            LifecycleError::Joined(errs) => errs.iter().flat_map(Self::flattened).collect(),
            other => vec![other],
        }
    }
}

/// Cloneable handle that triggers a graceful shutdown of a running
/// [`Application`] — the Rust analog of cancelling the `context.Context`
/// passed to Go's `Run`. Obtain one with [`Application::shutdown_handle`]
/// *before* calling [`Application::run`].
#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    tx: Arc<watch::Sender<bool>>,
}

impl ShutdownHandle {
    /// Requests a graceful shutdown. Idempotent — extra calls (from this or
    /// any cloned handle) are no-ops. Calling it before `run` makes the run
    /// execute its start hooks and immediately proceed to the drain phase,
    /// just as Go's `Run` does with an already-cancelled context.
    pub fn shutdown(&self) {
        // send_replace (not send): it stores the value even while no receiver
        // exists yet, so a shutdown triggered before `run` subscribes is
        // never lost.
        self.tx.send_replace(true);
    }
}

/// Drain notification handed to every [`Application::on_server`] task. The
/// server must return (cleanly) soon after [`ShutdownSignal::wait`] resolves;
/// the runner awaits it under the drain budget.
#[derive(Clone, Debug)]
pub struct ShutdownSignal {
    rx: watch::Receiver<bool>,
}

impl ShutdownSignal {
    /// Resolves when the application begins draining (or when the runner has
    /// already gone away). Designed to be passed to graceful-shutdown
    /// adapters, e.g. axum's `with_graceful_shutdown(signal.wait())`.
    pub async fn wait(mut self) {
        // An Err means the runner dropped the sender — shut down either way.
        let _ = self.rx.wait_for(|fired| *fired).await;
    }

    /// Reports whether the drain phase has already begun.
    pub fn is_shutdown(&self) -> bool {
        *self.rx.borrow()
    }
}

/// The runtime composition root. Build it with [`Application::new`] and the
/// chained `on_*` / `with_*` methods, then call [`Application::run`].
///
/// Mirrors the Go `lifecycle.Application`:
///
/// * start hooks run in registration order;
/// * server tasks each get their own tokio task;
/// * stop hooks run in **reverse** registration order under the drain budget;
/// * a start-hook failure rolls back by running the stop hooks.
pub struct Application {
    name: String,
    drain: Duration,
    trap_signals: bool,
    starts: Vec<Hook>,
    stops: Vec<Hook>,
    servers: Vec<(String, ServerFn)>,
    shutdown_tx: Arc<watch::Sender<bool>>,
}

impl fmt::Debug for Application {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Application")
            .field("name", &self.name)
            .field("drain", &self.drain)
            .field("trap_signals", &self.trap_signals)
            .field("starts", &self.starts.len())
            .field("stops", &self.stops.len())
            .field("servers", &self.servers.len())
            .finish()
    }
}

impl Application {
    /// Returns an `Application` with sensible defaults: a 30 s drain budget
    /// ([`DRAIN_TIMEOUT`]) and the OS signal trap enabled.
    pub fn new(name: impl Into<String>) -> Self {
        let (tx, _rx) = watch::channel(false);
        Application {
            name: name.into(),
            drain: DRAIN_TIMEOUT,
            trap_signals: true,
            starts: Vec::new(),
            stops: Vec::new(),
            servers: Vec::new(),
            shutdown_tx: Arc::new(tx),
        }
    }

    /// The application name, used in lifecycle log events.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configured drain budget.
    pub fn drain_timeout(&self) -> Duration {
        self.drain
    }

    /// Overrides the drain budget granted to server drain + stop hooks.
    /// A zero duration is normalised back to [`DRAIN_TIMEOUT`] at run time,
    /// mirroring the Go port's `Drain <= 0` guard.
    pub fn with_drain_timeout(mut self, drain: Duration) -> Self {
        self.drain = drain;
        self
    }

    /// Enables or disables the OS signal trap (ctrl-c / `SIGTERM`). Enabled
    /// by default — disable it when embedding the runner inside a larger
    /// process that owns signal handling. The Rust spelling of Go's
    /// `Signals []os.Signal` field.
    pub fn with_signal_trap(mut self, enabled: bool) -> Self {
        self.trap_signals = enabled;
        self
    }

    /// Returns a cloneable [`ShutdownHandle`] that stops this application
    /// programmatically. Grab it before [`Application::run`] consumes the
    /// builder.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            tx: Arc::clone(&self.shutdown_tx),
        }
    }

    /// Appends a start hook. Hooks run in registration order.
    pub fn on_start<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = HookResult> + Send + 'static,
    {
        self.starts.push(Box::new(move || Box::pin(hook())));
        self
    }

    /// Appends a stop hook. Stop hooks run in REVERSE registration order.
    pub fn on_stop<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = HookResult> + Send + 'static,
    {
        self.stops.push(Box::new(move || Box::pin(hook())));
        self
    }

    /// Registers a named long-running server task — the generalisation of
    /// Go's `OnHTTP(addr, handler)`. Calling this multiple times is allowed
    /// (multiple admin / public ports). Each task is spawned after the start
    /// hooks succeed and runs until either the application shuts down (its
    /// [`ShutdownSignal`] fires — return `Ok` promptly) or the task itself
    /// exits: the first task to exit triggers an application-wide shutdown,
    /// carrying its error, if any, exactly like a Go `ListenAndServe`
    /// failure.
    pub fn on_server<F, Fut>(mut self, name: impl Into<String>, server: F) -> Self
    where
        F: FnOnce(ShutdownSignal) -> Fut + Send + 'static,
        Fut: Future<Output = HookResult> + Send + 'static,
    {
        self.servers.push((
            name.into(),
            Box::new(move |signal| Box::pin(server(signal))),
        ));
        self
    }

    /// The canonical entry point. Runs every start hook in registration
    /// order, spawns the server tasks, then blocks until a [`ShutdownHandle`]
    /// fires, an OS signal arrives, or a server task exits; then drains the
    /// server tasks and runs every stop hook in reverse order under the
    /// drain budget.
    ///
    /// Returns the joined error from any phase. `Ok(())` iff every phase
    /// succeeded — note that a handle-triggered stop reports
    /// [`LifecycleError::Cancelled`] (Go returns `context.Canceled` there),
    /// while a signal-triggered or clean-server-exit stop reports nothing.
    pub async fn run(self) -> Result<(), LifecycleError> {
        let Application {
            name,
            drain,
            trap_signals,
            starts,
            mut stops,
            servers,
            shutdown_tx,
        } = self;
        let drain = effective_drain(drain);
        tracing::info!(name = %name, "application starting");

        // 1. Start hooks (ordered). On failure, roll back by running the stop
        //    hooks with no drain deadline (Go uses context.Background() here).
        for (index, hook) in starts.into_iter().enumerate() {
            if let Err(source) = hook().await {
                tracing::error!(index, error = %source, "start hook failed");
                let mut errs = vec![LifecycleError::StartHook { index, source }];
                errs.extend(run_stops(std::mem::take(&mut stops), None).await);
                return Err(LifecycleError::join(errs).expect("at least the start error"));
            }
        }

        // 2. Server tasks — each gets its own tokio task; the first exit
        //    short-circuits the run (an error exit carries its error).
        let total_servers = servers.len();
        let (result_tx, mut result_rx) =
            mpsc::channel::<(String, HookResult)>(total_servers.max(1));
        let (drain_tx, drain_rx) = watch::channel(false);
        for (server_name, server) in servers {
            let tx = result_tx.clone();
            let signal = ShutdownSignal {
                rx: drain_rx.clone(),
            };
            tokio::spawn(async move {
                tracing::info!(server = %server_name, "server starting");
                let result = server(signal).await;
                let _ = tx.send((server_name, result)).await;
            });
        }
        // `result_tx` stays alive in this scope so `recv()` only resolves
        // when a server task actually exits.

        // 3. Block until a shutdown trigger.
        let mut shutdown_rx = shutdown_tx.subscribe();
        let mut finished_servers = 0usize;
        let mut trigger: Option<LifecycleError> = None;
        tokio::select! {
            _ = shutdown_rx.wait_for(|fired| *fired) => {
                tracing::info!("shutdown requested, shutting down");
                trigger = Some(LifecycleError::Cancelled);
            }
            _ = wait_for_os_signal(trap_signals) => {
                tracing::info!("signal received, shutting down");
            }
            exited = result_rx.recv() => {
                match exited {
                    Some((server_name, Err(source))) => {
                        finished_servers += 1;
                        tracing::error!(server = %server_name, error = %source, "server failed");
                        trigger = Some(LifecycleError::Server { name: server_name, source });
                    }
                    Some((server_name, Ok(()))) => {
                        finished_servers += 1;
                        tracing::info!(server = %server_name, "server exited, shutting down");
                    }
                    None => {}
                }
            }
        }

        // 4. Drain: fire the server shutdown signal, await the remaining
        //    server tasks, then run the stop hooks — all under one budget.
        let deadline = Instant::now() + drain;
        let _ = drain_tx.send(true);
        let mut errs = Vec::new();
        if let Some(trigger) = trigger {
            errs.push(trigger);
        }
        while finished_servers < total_servers {
            match tokio::time::timeout_at(deadline, result_rx.recv()).await {
                Ok(Some((server_name, result))) => {
                    finished_servers += 1;
                    if let Err(source) = result {
                        tracing::error!(server = %server_name, error = %source, "server failed during drain");
                        errs.push(LifecycleError::Server {
                            name: server_name,
                            source,
                        });
                    }
                }
                Ok(None) => break,
                Err(_elapsed) => {
                    let pending = total_servers - finished_servers;
                    tracing::error!(pending, "drain timeout elapsed before all servers stopped");
                    errs.push(LifecycleError::DrainTimedOut { pending });
                    break;
                }
            }
        }
        errs.extend(run_stops(stops, Some(deadline)).await);

        tracing::info!(name = %name, "application stopped");
        match LifecycleError::join(errs) {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }
}

/// Normalises the drain budget: a zero duration falls back to
/// [`DRAIN_TIMEOUT`], mirroring the Go port's `Drain <= 0` guard in `Run`.
fn effective_drain(drain: Duration) -> Duration {
    if drain.is_zero() {
        DRAIN_TIMEOUT
    } else {
        drain
    }
}

/// Runs the stop hooks in REVERSE registration order, optionally bounded by
/// the drain deadline. Every hook is polled at least once even after the
/// deadline (so prompt hooks always run); a hook still pending at the
/// deadline is cancelled and reported as a timed-out [`LifecycleError::StopHook`].
async fn run_stops(stops: Vec<Hook>, deadline: Option<Instant>) -> Vec<LifecycleError> {
    let mut errs = Vec::new();
    for (index, hook) in stops.into_iter().enumerate().rev() {
        let result = match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, hook()).await {
                Ok(result) => result,
                Err(elapsed) => Err(Box::new(elapsed) as HookError),
            },
            None => hook().await,
        };
        if let Err(source) = result {
            tracing::error!(index, error = %source, "stop hook failed");
            errs.push(LifecycleError::StopHook { index, source });
        }
    }
    errs
}

/// Resolves when a trapped OS signal arrives (ctrl-c / `SIGINT`, plus
/// `SIGTERM` on Unix); pends forever when trapping is disabled or the
/// handlers cannot be installed.
async fn wait_for_os_signal(trap: bool) {
    if !trap {
        std::future::pending::<()>().await;
    }
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        let terminate = async {
            match term.as_mut() {
                Some(term) => {
                    term.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = wait_ctrl_c() => {}
            _ = terminate => {}
        }
    }
    #[cfg(not(unix))]
    {
        wait_ctrl_c().await;
    }
}

/// Resolves on ctrl-c; pends forever if the handler cannot be installed
/// (so the runner falls back to the other shutdown triggers).
async fn wait_ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Mutex;

    fn push(order: &Arc<Mutex<Vec<&'static str>>>, step: &'static str) {
        order.lock().unwrap().push(step);
    }

    // ---- ports of the Go test suite ----------------------------------------

    /// Go: TestRunInvokesStartAndStopInOrder.
    #[tokio::test]
    async fn run_invokes_start_and_stop_in_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let (o1, o2, o3, o4) = (order.clone(), order.clone(), order.clone(), order.clone());

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_start(move || async move {
                push(&o1, "start1");
                Ok(())
            })
            .on_start(move || async move {
                push(&o2, "start2");
                Ok(())
            })
            .on_stop(move || async move {
                push(&o3, "stop1");
                Ok(())
            })
            .on_stop(move || async move {
                push(&o4, "stop2");
                Ok(())
            });

        let handle = app.shutdown_handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            handle.shutdown();
        });

        let err = app.run().await.expect_err("handle stop reports Cancelled");
        assert!(err.is_cancelled(), "run: {err}");
        assert_eq!(
            *order.lock().unwrap(),
            vec!["start1", "start2", "stop2", "stop1"]
        );
    }

    /// Go: TestStartHookFailureRunsStops.
    #[tokio::test]
    async fn start_hook_failure_runs_stops() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_in_hook = stopped.clone();

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_start(|| async { Err::<(), HookError>("boom".into()) })
            .on_stop(move || async move {
                stopped_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            });

        let err = app.run().await.expect_err("expected error");
        assert!(
            stopped.load(Ordering::SeqCst),
            "stop hook should run when start fails"
        );
        assert_eq!(err.to_string(), "start hook 0: boom");
    }

    /// Go: TestHTTPServerStartsAndDrains (generalised to a server task).
    #[tokio::test]
    async fn server_task_starts_and_drains() {
        let drained = Arc::new(AtomicBool::new(false));
        let drained_in_server = drained.clone();

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_server("api", move |shutdown| async move {
                shutdown.wait().await;
                drained_in_server.store(true, Ordering::SeqCst);
                Ok(())
            });

        let handle = app.shutdown_handle();
        let run = tokio::spawn(app.run());

        // Give the server a moment to fail-fast or stabilise.
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.shutdown();

        let err = run
            .await
            .unwrap()
            .expect_err("handle stop reports Cancelled");
        assert!(err.is_cancelled(), "run: {err}");
        assert!(
            drained.load(Ordering::SeqCst),
            "server should observe drain"
        );
    }

    /// Go: TestStopHookErrorsJoined.
    #[tokio::test]
    async fn stop_hook_errors_joined() {
        let calls = Arc::new(AtomicU32::new(0));
        let (c1, c2) = (calls.clone(), calls.clone());

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_stop(move || async move {
                c1.fetch_add(1, Ordering::SeqCst);
                Err::<(), HookError>("a".into())
            })
            .on_stop(move || async move {
                c2.fetch_add(1, Ordering::SeqCst);
                Err::<(), HookError>("b".into())
            });

        let handle = app.shutdown_handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            handle.shutdown();
        });

        let err = app.run().await.expect_err("expected joined error");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let parts: Vec<String> = err.flattened().iter().map(ToString::to_string).collect();
        assert_eq!(
            parts,
            vec!["context cancelled", "stop hook 1: b", "stop hook 0: a"]
        );
        assert!(err.is_cancelled());
    }

    // ---- Rust-specific coverage ---------------------------------------------

    #[tokio::test]
    async fn pre_triggered_shutdown_still_runs_hooks() {
        let started = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let (started_in_hook, stopped_in_hook) = (started.clone(), stopped.clone());

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_start(move || async move {
                started_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            })
            .on_stop(move || async move {
                stopped_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            });

        // Shut down before run, like Go running with an already-cancelled ctx.
        app.shutdown_handle().shutdown();

        let err = app.run().await.expect_err("expected Cancelled");
        assert!(err.is_cancelled());
        assert!(started.load(Ordering::SeqCst));
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn server_error_short_circuits_run() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_in_hook = stopped.clone();

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_server("api", |_shutdown| async {
                Err::<(), HookError>("listen tcp :8080: address already in use".into())
            })
            .on_stop(move || async move {
                stopped_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            });

        let err = app.run().await.expect_err("expected server error");
        assert!(!err.is_cancelled());
        assert_eq!(
            err.to_string(),
            "server api: listen tcp :8080: address already in use"
        );
        assert!(stopped.load(Ordering::SeqCst), "stop hooks still drain");
    }

    #[tokio::test]
    async fn server_clean_exit_stops_application_without_error() {
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_in_hook = stopped.clone();

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_secs(1))
            .on_server("one-shot", |_shutdown| async { Ok(()) })
            .on_stop(move || async move {
                stopped_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            });

        // Like Go: a server goroutine returning nil triggers shutdown with a
        // nil trigger, so the whole run succeeds.
        app.run().await.expect("clean server exit is not an error");
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn start_failure_skips_servers() {
        let served = Arc::new(AtomicBool::new(false));
        let served_in_server = served.clone();

        let app = Application::new("svc")
            .on_start(|| async { Err::<(), HookError>("boom".into()) })
            .on_server("api", move |_shutdown| async move {
                served_in_server.store(true, Ordering::SeqCst);
                Ok(())
            });

        app.run().await.expect_err("expected start error");
        assert!(
            !served.load(Ordering::SeqCst),
            "servers must not start after a start-hook failure"
        );
    }

    #[tokio::test]
    async fn drain_timeout_cancels_slow_stop_hook_but_polls_prompt_hooks() {
        let prompt_ran = Arc::new(AtomicBool::new(false));
        let prompt_in_hook = prompt_ran.clone();

        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_millis(30))
            // Registered first → runs LAST (reverse order), after the budget
            // is exhausted; it is prompt, so its single poll completes it.
            .on_stop(move || async move {
                prompt_in_hook.store(true, Ordering::SeqCst);
                Ok(())
            })
            // Registered last → runs FIRST and blows the budget.
            .on_stop(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            });

        app.shutdown_handle().shutdown();
        let err = app.run().await.expect_err("expected timeout error");
        let parts: Vec<String> = err.flattened().iter().map(ToString::to_string).collect();
        assert!(err.is_cancelled());
        assert!(
            parts.iter().any(|p| p.starts_with("stop hook 1: ")),
            "slow hook reported: {parts:?}"
        );
        assert!(
            prompt_ran.load(Ordering::SeqCst),
            "prompt hook still runs after the deadline"
        );
    }

    #[tokio::test]
    async fn drain_timeout_reports_pending_server() {
        let app = Application::new("svc")
            .with_drain_timeout(Duration::from_millis(30))
            .on_server("stuck", |_shutdown| async {
                tokio::time::sleep(Duration::from_secs(100)).await;
                Ok(())
            });

        app.shutdown_handle().shutdown();
        let err = app.run().await.expect_err("expected drain timeout");
        let parts: Vec<String> = err.flattened().iter().map(ToString::to_string).collect();
        assert!(parts
            .iter()
            .any(|p| p == "drain timeout elapsed with 1 server task(s) still running"));
    }

    #[test]
    fn zero_drain_normalises_to_default() {
        // Mirrors the Go port's `if a.Drain <= 0 { a.Drain = DrainTimeout }`
        // guard at the top of Run.
        assert_eq!(effective_drain(Duration::ZERO), DRAIN_TIMEOUT);
        assert_eq!(
            effective_drain(Duration::from_millis(5)),
            Duration::from_millis(5)
        );
    }

    #[test]
    fn defaults() {
        let app = Application::new("orders");
        assert_eq!(app.name(), "orders");
        assert_eq!(app.drain_timeout(), DRAIN_TIMEOUT);
        assert_eq!(DRAIN_TIMEOUT, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn shutdown_handle_is_clonable_and_idempotent() {
        let app = Application::new("svc").with_drain_timeout(Duration::from_secs(1));
        let handle = app.shutdown_handle();
        let clone = handle.clone();
        handle.shutdown();
        handle.shutdown();
        clone.shutdown();
        let err = app.run().await.expect_err("expected Cancelled");
        assert!(err.is_cancelled());
    }

    #[test]
    fn error_display_matches_go_formats() {
        let start = LifecycleError::StartHook {
            index: 2,
            source: "boom".into(),
        };
        assert_eq!(start.to_string(), "start hook 2: boom");

        let stop = LifecycleError::StopHook {
            index: 0,
            source: "a".into(),
        };
        assert_eq!(stop.to_string(), "stop hook 0: a");

        let server = LifecycleError::Server {
            name: ":8080".into(),
            source: "denied".into(),
        };
        assert_eq!(server.to_string(), "server :8080: denied");

        assert_eq!(LifecycleError::Cancelled.to_string(), "context cancelled");

        // errors.Join in Go renders parts newline-separated.
        let joined = LifecycleError::Joined(vec![
            LifecycleError::Cancelled,
            LifecycleError::StopHook {
                index: 1,
                source: "b".into(),
            },
        ]);
        assert_eq!(joined.to_string(), "context cancelled\nstop hook 1: b");
    }

    #[test]
    fn join_helper_flattens_like_errors_join() {
        assert!(LifecycleError::join(Vec::new()).is_none());

        let single = LifecycleError::join(vec![LifecycleError::Cancelled]).unwrap();
        assert!(matches!(single, LifecycleError::Cancelled));
        assert_eq!(single.flattened().len(), 1);

        let joined = LifecycleError::join(vec![
            LifecycleError::Cancelled,
            LifecycleError::DrainTimedOut { pending: 1 },
        ])
        .unwrap();
        assert_eq!(joined.flattened().len(), 2);
        assert!(joined.is_cancelled());
    }

    #[test]
    fn source_chain_exposes_hook_error() {
        let err = LifecycleError::StartHook {
            index: 0,
            source: "boom".into(),
        };
        let source = std::error::Error::source(&err).expect("source");
        assert_eq!(source.to_string(), "boom");
    }

    #[test]
    fn bounds_application_send_and_handle_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send::<Application>();
        assert_send_sync::<ShutdownHandle>();
        assert_send_sync::<ShutdownSignal>();
        assert_send_sync::<LifecycleError>();
    }
}
