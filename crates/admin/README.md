# `firefly-admin`

> **Tier:** Platform · **Status:** Full

## Overview

`firefly-admin` is an embedded, **Spring-Boot-Admin-style management
dashboard**: a self-contained single-page app mounted at a configurable path,
backed by a JSON API and live SSE streams. It is modeled on
[`firefly-actuator`]'s `mount()` idiom.

`mount(cfg, deps)` returns an axum `Router` exposing, under the configured
base path (default `/admin`):

| Surface | What it serves |
|---------|----------------|
| `GET {base}` + `{base}/static/*` | The vendored SPA shell + assets (served verbatim, `Cache-Control: no-cache`) |
| `GET {base}/api/overview` | App info + health summary |
| `GET {base}/api/health` | Composite health; 200 UP/DEGRADED, 503 DOWN |
| `GET {base}/api/env` / `config` | Environment / configuration view |
| `GET/POST {base}/api/loggers[/{name}]` | Runtime log-level read/write over a `tracing_subscriber` reload handle |
| `GET {base}/api/metrics[/{name}]` | Meter listing + Micrometer detail |
| `GET {base}/api/scheduled` | Scheduled-task listing |
| `GET {base}/api/caches` + `POST …/{name}/evict` | Cache listing + eviction |
| `GET {base}/api/cqrs` | CQRS handler listing |
| `GET {base}/api/beans` + `/api/beans/{name}` + `/api/beans/graph` | DI bean listing / detail / dependency graph (backed by the optional `firefly-container`) |
| `GET {base}/api/transactions` | Saga / workflow / TCC definition listing |
| `GET {base}/api/traces` | HTTP-trace ring buffer (cap 500) |
| `GET {base}/api/logfile` + `POST …/clear` | In-memory log ring buffer (cap 2000) |
| `GET {base}/api/runtime` / `server` | tokio worker/task counts + process RSS |
| `GET {base}/api/views[/{id}]` | Custom `AdminView` plugin listing + payloads |
| `GET {base}/api/sse/*` | Live streams: `beans` / `health` / `metrics` / `traces` / `logfile` / `runtime` / `server` |
| `GET/POST/DELETE {base}/api/instances` | Multi-instance server mode (when enabled) |

Bind the dashboard on a separate admin port, or guard it with
`require_auth`, so it never leaks onto the public network.

## Why a separate crate?

A "single pane of glass" is the canonical operator experience — live health,
metrics trends, log tailing, request traces, and runtime-level logger tweaks,
without redeploying. `firefly-admin` delivers exactly that, serving a vendored
SPA over a stable `/admin/api` contract.

The crate adapts the dashboard idiom to idiomatic Rust:

- **Explicit wiring via `AdminDeps`.** Every collaborator is handed in through
  the `AdminDeps` struct (constructor injection) rather than discovered from an
  ambient container. Required pieces (health, metrics, a `TraceBuffer`, a
  `LogBuffer`) are always present; the rest are `Option`s the endpoints
  degrade gracefully around.
- **`tracing`-backed log viewer.** The log viewer is fed by `LogBuffer`, which
  is *both* a ring buffer and a `tracing_subscriber::Layer`; logger-level
  mutation rewrites an `EnvFilter` directive through a `reload::Handle` (via
  `firefly-actuator`'s `LoggersState`).
- **DI introspection via `firefly-container`.** The Beans view
  (`/api/beans`, `/api/beans/{name}`, `/api/beans/graph`, the `/api/sse/beans`
  stream, and the overview `beans`/`wiring` blocks) is backed by the optional
  `AdminDeps::container` — when wired it reports each registered bean's
  name/type/scope/stereotype/primary plus its `initialized` flag and
  resolution count, sourced from `Container::beans()` / `bean_stats()`.
  Reflection-only fields (constructor `dependencies`/dependency-graph `edges`,
  `conditions`, `creation_time_ms`, lifecycle methods) have no zero-cost Rust
  analogue and carry empty/`null` defaults, so the bean graph is nodes-only.
  The `runtime` view reports tokio task/worker counts + process RSS via
  `sysinfo`.

## Public surface

```rust,ignore
pub fn mount(cfg: AdminConfig, deps: AdminDeps) -> axum::Router;

pub struct AdminConfig {              // firefly.admin.*
    pub enabled: bool,                // default true
    pub path: String,                 // default "/admin"
    pub title: String,                // default "Firefly Admin"
    pub theme: String,                // default "auto"
    pub require_auth: bool,           // default false
    pub allowed_roles: Vec<String>,   // default ["ADMIN"]
    pub refresh_interval: u64,        // default 5000 (ms)
}
pub struct AdminServerConfig { /* firefly.admin.server.*: instances[], poll/timeouts */ }
pub struct AdminClientConfig { /* firefly.admin.client.*: url, auto_register */ }

pub struct AdminDeps {
    pub app_name: String,
    pub app_version: String,
    pub health: Arc<HealthComposite>,            // firefly-actuator
    pub metrics: Arc<MetricRegistry>,            // firefly-actuator
    pub traces: Arc<TraceBuffer>,
    pub logs: LogBuffer,
    pub scheduler: Option<Arc<Scheduler>>,       // firefly-scheduling
    pub bus: Option<Arc<Bus>>,                   // firefly-cqrs
    pub orchestration: Option<Arc<OrchestrationRegistry>>, // firefly-orchestration
    pub cache: Option<Arc<dyn CacheOps>>,        // firefly-actuator
    pub loggers: Option<Arc<LoggersState>>,      // firefly-actuator reload handle
    pub instances: Option<Arc<InstanceRegistry>>,// Some ⇒ server mode
    pub container: Option<Arc<Container>>,       // firefly-container, drives the Beans view
    pub views: Vec<Arc<dyn AdminView>>,
}
impl AdminDeps { pub fn new(app_name, app_version, health, metrics, traces, logs) -> Self; }

// HTTP-trace ring buffer + tower layer (skips /admin & /actuator, cap 500)
pub struct TraceBuffer { /* … */ }
pub struct TraceLayer { /* … */ }     // apply to the *application* router

// In-memory log ring buffer + tracing Layer (cap 2000, monotonic ids)
pub struct LogBuffer { /* … */ }

// Custom view plugin point
#[async_trait] pub trait AdminView {
    fn view_id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn icon(&self) -> &str;
    async fn data(&self) -> serde_json::Value;
}
pub struct AdminViewRegistry { /* … */ }

// Server / client mode
pub struct InstanceRegistry { /* register / deregister / discover_static */ }
pub struct InstanceInfo { /* name, url, status, last_checked, metadata */ }
pub struct AdminClient { /* register()/deregister(), start/stop lifecycle hooks */ }
```

## Quick start

```rust,no_run
use std::sync::Arc;
use firefly_actuator::{HealthComposite, IndicatorFn, HealthResult, MetricRegistry};
use firefly_admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer, TraceLayer};
use tracing_subscriber::prelude::*;

# async fn demo() {
// 1. Capture logs: LogBuffer is a tracing Layer.
let logs = LogBuffer::new();
tracing_subscriber::registry().with(logs.clone()).init();

// 2. Capture HTTP traces on the application router.
let traces = Arc::new(TraceBuffer::new());

// 3. Wire the deps and mount.
let health = Arc::new(HealthComposite::new());
health.add(IndicatorFn::new("db", || async { HealthResult::up() }));

let deps = AdminDeps::new(
    "orders", "1.4.0",
    health,
    Arc::new(MetricRegistry::new()),
    Arc::clone(&traces),
    logs,
);
let admin = mount(AdminConfig::default(), deps);

let app: axum::Router = axum::Router::new()
    // … your routes …
    .layer(TraceLayer::new(traces))   // /admin & /actuator paths are skipped
    .merge(admin);
# let _ = app;
# }
```

## Live streams (SSE)

Each `/api/sse/*` route returns an `axum::response::Sse` driven by a
`tokio::time::interval`. The `health` stream pushes only on status change;
`traces` and `logfile` are incremental — they track a cursor (insertion count
/ monotonic id) and push only new rows.

## Auth guard

When `require_auth` is set, every `/api/*` route is wrapped with a guard that
reads the request-scoped `firefly_security::Authentication` (populated by the
bearer layer) and returns:

- `401` `{"error":"Authentication required"}` when no principal is present;
- `403` `{"error":"Forbidden"}` when the principal lacks an `allowed_roles`
  role;
- otherwise the request proceeds.

The SPA shell and static assets stay public so the dashboard can boot and then
surface 401s from the API.

## Server / client mode

- **Server mode** — set `AdminDeps::instances` to an `InstanceRegistry`
  (optionally seeded from `AdminServerConfig::instances` via
  `discover_static`). The `/api/instances` `GET` / `POST` / `DELETE` routes
  appear, and `settings.serverMode` reports `true`.
- **Client mode** — construct an `AdminClient` from `AdminClientConfig` and
  hook `register_hook()` / `deregister_hook()` into
  `firefly_lifecycle::Application::on_start` / `on_stop`. With `auto_register`
  on, the app POSTs `{name, url}` to the remote admin server on start and
  DELETEs it on stop; both swallow their own errors so a down admin server
  never blocks startup.

[`firefly-actuator`]: ../actuator/README.md
