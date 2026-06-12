# `firefly-plugins`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-platform-plugins` · **Go module:** `plugins`

## Overview

`firefly-plugins` ships the framework's **plugin lifecycle SPI** — a typed
`Plugin` trait and a composite `Registry` that starts every plugin in
registration order and stops them in reverse on shutdown.

Rust's static-binary model, like Go's, does not support hot reload out of
the box. The Java port uses PF4J; the .NET port uses
`McMaster.NETCore.Plugins`. This crate focuses on the **lifecycle
contract** — services that need dynamic loading integrate a loader (e.g.
`libloading`) at the application entry point and feed the discovered
values into the same `Registry`.

## Public surface

```rust,ignore
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self) -> Result<(), BoxError>;
    async fn stop(&self) -> Result<(), BoxError>;
}

pub struct Registry { /* ... */ }
impl Registry {
    pub fn new() -> Self;
    pub fn register(&self, plugin: Arc<dyn Plugin>); // re-registering by name overwrites
    pub async fn start_all(&self) -> Result<(), PluginError>; // ordered; rolls back already-started on first failure
    pub async fn stop_all(&self) -> Result<(), PluginError>;  // reverse order; joins errors
    pub fn names(&self) -> Vec<String>;
}

pub enum PluginError { Start { .. }, Stop { .. }, Aggregate(Vec<PluginError>) }
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
```

Error messages mirror the Go wrapping format — `plugin "name" start: <cause>`
— and aggregated errors join their messages with `\n` exactly like Go's
`errors.Join`.

## Quick start

```rust
use std::sync::Arc;

use firefly_plugins::{BoxError, Plugin, Registry};

struct SchedulerPlugin;

#[async_trait::async_trait]
impl Plugin for SchedulerPlugin {
    fn name(&self) -> &str {
        "scheduler"
    }

    async fn start(&self) -> Result<(), BoxError> {
        println!("scheduler starting");
        Ok(())
    }

    async fn stop(&self) -> Result<(), BoxError> {
        println!("scheduler stopping");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), firefly_plugins::PluginError> {
    let registry = Registry::new();
    registry.register(Arc::new(SchedulerPlugin));

    registry.start_all().await?;
    // ... application runs ...
    registry.stop_all().await?;
    Ok(())
}
```

`firefly-starter-application` exposes a pre-wired `Registry` ready to
receive plugins.

## Testing

```bash
cargo test -p firefly-plugins
```

Covers ordered start, reverse-order stop, rollback when a downstream
start fails, replace-by-name registration, error wrapping/joining parity
with the Go module, and `Send`/`Sync` bounds.
