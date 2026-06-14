# `firefly-lifecycle`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-lifecycle` provides the canonical **application orchestrator** every
Firefly Rust service uses. It owns:

* Ordered start hooks (`on_start`).
* Long-running server tasks (`on_server`) — multiple servers allowed, each
  gets its own tokio task and a `ShutdownSignal` to drain on.
* Reverse-ordered stop hooks (`on_stop`).
* Signal trap (ctrl-c / `SIGINT` + `SIGTERM` on Unix) via `tokio::signal`.
* Programmatic shutdown (`ShutdownHandle`) — so tests never need real signals.
* Drain budget (default 30 s) granted to server drain + stop hooks.
* Failure rollback — a start hook error triggers the stop hooks for cleanup.

```rust,ignore
let app = Application::new("orders")
    .on_start(broker_start)
    .on_start(scheduler_start)
    .on_server("api", api_server)        // e.g. axum on :8080
    .on_server("actuator", actuator)     // e.g. axum on :8081
    .on_stop(broker_stop)
    .on_stop(scheduler_stop);

app.run().await?;
```

## Why a separate crate?

A concise one-line bootstrap is only possible when the framework owns the
entire lifecycle. Idiomatic async Rust typically scatters this across
`main.rs` (a `tokio::select!` over `ctrl_c`, graceful shutdown plumbing,
drain timeouts, manual cleanup ordering). `firefly-lifecycle` lifts that into
one declarative composition so every service handles `SIGTERM` the same way
on day one.

## Public surface

```rust,ignore
pub type HookError  = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type HookResult = Result<(), HookError>;

pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Application { /* builder */ }
impl Application {
    pub fn new(name: impl Into<String>) -> Self;
    pub fn with_drain_timeout(self, d: Duration) -> Self;
    pub fn with_signal_trap(self, enabled: bool) -> Self;
    pub fn shutdown_handle(&self) -> ShutdownHandle;
    pub fn on_start(self, hook) -> Self;                 // async fn() -> HookResult
    pub fn on_stop(self, hook) -> Self;                  // async fn() -> HookResult
    pub fn on_server(self, name, server) -> Self;        // async fn(ShutdownSignal) -> HookResult
    pub async fn run(self) -> Result<(), LifecycleError>;
}

pub struct ShutdownHandle;            // Clone; .shutdown() triggers a graceful stop
pub struct ShutdownSignal;            // Clone; .wait().await resolves at drain start

pub enum LifecycleError {             // joined-error contract
    Cancelled,                        // a clean, handle-driven stop
    StartHook { index, source },      // "start hook %d: %w"
    StopHook  { index, source },      // "stop hook %d: %w"
    Server    { name,  source },      // "http %s: %w"
    DrainTimedOut { pending },        // drain budget elapsed
    Joined(Vec<LifecycleError>),      // aggregated errors; Display joins with '\n'
}
```

## Lifecycle diagram

```
run()
   │
   ├─ for each on_start: run hook (in registration order)
   │     │
   │     ├─ on error: rollback by running on_stop hooks; return joined err
   │
   ├─ for each on_server: spawn server task (receives a ShutdownSignal)
   │
   ├─ block on:
   │     │  ShutdownHandle::shutdown()        → trigger = Cancelled
   │     │  OS signal (ctrl-c / SIGTERM)      → trigger = none
   │     │  first server task exit            → trigger = its error, if any
   │
   ├─ derive drain deadline (now + drain budget)
   ├─ fire ShutdownSignal; await server tasks until the deadline
   ├─ run on_stop hooks in REVERSE order under the remaining budget
   └─ return joined errors (trigger ⊕ servers ⊕ stops)
```

## Quick start

```rust
use firefly_lifecycle::{Application, HookResult, ShutdownSignal};
use std::time::Duration;

async fn connect_broker() -> HookResult {
    Ok(())
}

async fn flush_broker() -> HookResult {
    Ok(())
}

async fn api_server(shutdown: ShutdownSignal) -> HookResult {
    // Real services pass `shutdown.wait()` to e.g.
    // `axum::serve(listener, router).with_graceful_shutdown(shutdown.wait())`.
    shutdown.wait().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Application::new("orders")
        .with_drain_timeout(Duration::from_secs(30))
        .on_start(connect_broker)
        .on_server("api", api_server)
        .on_stop(flush_broker);

    // Programmatic stop — hand it to an admin endpoint, or call it from tests.
    let handle = app.shutdown_handle();
    handle.shutdown(); // remove this line in a real service: run until SIGTERM

    let err = app.run().await.expect_err("handle stop reports Cancelled");
    assert!(err.is_cancelled()); // a handle-driven stop is a clean stop
    Ok(())
}
```

`firefly-starter-core`'s `Core::new_application()` returns an `Application`
pre-configured for the service.

## Design notes

* Hooks are async closures or `async fn`s returning `HookResult`; servers are
  `async fn(ShutdownSignal)`, each driven on its own tokio task.
* A graceful stop is requested with `ShutdownHandle::shutdown()`, and a clean,
  handle-driven stop is reported as `LifecycleError::is_cancelled()`.
* Multiple errors are aggregated into `LifecycleError::Joined`, whose `Display`
  joins entries with `\n`.
* Logging flows through the global `tracing` subscriber.
* The signal trap is toggled with `with_signal_trap(bool)` and is fixed to
  ctrl-c + `SIGTERM`.
* Drain budgets are enforced with a hard `tokio::time::timeout` per phase, yet
  every stop hook is still polled at least once, so prompt hooks always run.

## Testing

```bash
cargo test -p firefly-lifecycle
```

Covers ordered start + reverse-ordered stop, start-hook failure rollback,
server task lifecycle (start + drain + error short-circuit + clean-exit
shutdown), joined stop-hook errors, drain-timeout reporting for both stop
hooks and server tasks, zero-drain normalisation, handle clone/idempotency,
joined error display, source chains, and Send + Sync bounds.
