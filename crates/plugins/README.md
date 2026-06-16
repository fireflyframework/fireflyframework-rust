# `firefly-plugins`

> **Tier:** Platform · **Status:** Stable

## Overview

`firefly-plugins` ships the framework's **plugin lifecycle SPI** — a typed
`Plugin` trait and a composite `Registry` that starts every plugin in
registration order and stops them in reverse on shutdown.

Rust's static-binary model does not support hot reload out of the box, so
this crate focuses on the **lifecycle contract** — services that need
dynamic loading integrate a loader (e.g. `libloading`) at the application
entry point and feed the discovered values into the same `Registry`.

## Public surface

```rust,ignore
#[async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn depends_on(&self) -> Vec<String> { Vec::new() } // default: no dependencies
    async fn start(&self) -> Result<(), BoxError>;
    async fn stop(&self) -> Result<(), BoxError>;
}

pub struct Registry { /* ... */ }
impl Registry {
    pub fn new() -> Self;
    pub fn register(&self, plugin: Arc<dyn Plugin>); // re-registering by name overwrites
    pub async fn start_all(&self) -> Result<(), PluginError>; // dependency (topological) order; rolls back on first failure
    pub async fn stop_all(&self) -> Result<(), PluginError>;  // reverse order; joins errors
    pub fn names(&self) -> Vec<String>;
    pub fn state(&self, name: &str) -> Option<PluginState>;   // per-plugin lifecycle state
}

pub enum PluginError { Start { .. }, Stop { .. }, Resolution(ResolutionError), Aggregate(Vec<PluginError>) }
pub enum ResolutionError { MissingDependency { plugin, missing }, Cycle }
pub enum PluginState { Loaded, Started, Stopped, Failed }
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

// dependency-aware manager + extension points
pub struct PluginManager { /* ... */ }       // start_plugin/stop_plugin cascade, descriptors, shared registry
pub struct PluginDescriptor { /* id, depends_on, state, loaded_at, last_state_change, failed_reason */ }
pub struct ExtensionRegistry { /* ... */ }   // TypeId-keyed extension points
pub struct ExtensionPoint { /* ... */ }
pub fn extension_point<T: ?Sized + 'static>() -> ExtensionPoint;
```

Error messages use a consistent wrapping format — `plugin "name" start: <cause>`
— and aggregated errors join their messages with `\n`.

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

## Dependency-aware lifecycle

The platform tier adds dependency-aware lifecycle on top of the base
`Registry` — all backward compatible (the original registration-order
semantics are unchanged when no plugin declares a dependency).

### `Plugin::depends_on`

```rust,ignore
fn depends_on(&self) -> Vec<String> { Vec::new() } // default: no deps
```

A new default method on the `Plugin` trait. When plugins declare
dependencies, `Registry::start_all` orders them with a **Kahn topological
sort** (dependencies start first); `stop_all` stops dependents first
(reverse). Ties break by registration order, so a graph with no edges
degrades to plain registration order — existing behaviour is untouched.
Missing dependencies and cycles fail fast with `PluginError::Resolution`:

```rust,ignore
pub enum ResolutionError {
    MissingDependency { plugin: String, missing: String }, // "plugin "b" depends on missing plugin "a""
    Cycle,                                                  // "plugin dependency cycle detected"
}
```

### Per-plugin state

```rust,ignore
pub enum PluginState { Loaded, Started, Stopped, Failed } // as_str(): "LOADED"/"STARTED"/...
impl Registry { pub fn state(&self, name: &str) -> Option<PluginState>; }
```

A plugin is `Loaded` on registration, `Started` after a successful
`start_all`, `Stopped` after `stop_all`, and `Failed` if a lifecycle hook
errors during the most recent sweep.

### `ExtensionRegistry` + `extension_point`

A `TypeId`-keyed registry of extension points and the extensions
contributed to them. Each point is keyed by an id carrying the interface's
`TypeId`, and contributions are validated to register under that type.

```rust,ignore
let point = extension_point::<dyn Formatter>();
reg.register_extension_point("formatters", point).await;
reg.register("formatters", Arc::new(JsonFormatter)).await?;   // validated
let all = reg.get("formatters").await;      // priority-sorted
let top = reg.get_extension("formatters").await?;              // highest priority
let typed = reg.get_as::<JsonFormatter>("formatters").await;   // downcast helper
```

Contributions for ids with **no** declared point type remain accepted
(lenient, backward-compatible). Extensions sort highest-priority first,
ties in insertion order.

### `PluginManager`

A dependency-aware manager (richer than `Registry`): per-plugin
`start_plugin`/`stop_plugin` with transitive cascade (dependencies start
first; dependents stop first), `start_all` /
`stop_all` / `remove` / `unload_all`, `PluginDescriptor` tracking
(`loaded_at`, `last_state_change`, `failed_reason`), and a shared
`ExtensionRegistry`. Plugins already in the target state are skipped so
their hooks never double-run.

```rust,ignore
let mgr = PluginManager::new();
mgr.add(Arc::new(plugin_a)).await;          // depends_on: []
mgr.add(Arc::new(plugin_b)).await;          // depends_on: ["a"]
mgr.start_plugin("b").await?;               // starts a, then b
let desc = mgr.get_plugin("b").await.unwrap();
assert_eq!(desc.state, PluginState::Started);
```

## Testing

```bash
cargo test -p firefly-plugins
```

Covers ordered start, reverse-order stop, rollback when a downstream
start fails, replace-by-name registration, error wrapping/joining, and
`Send`/`Sync` bounds — plus the dependency-aware surface: Kahn
topological ordering with missing-dependency and cycle rejection,
per-plugin `PluginState` transitions, `ExtensionRegistry` type validation
/ priority ordering / lenient unknown points, and the `PluginManager`
lifecycle cascade.
