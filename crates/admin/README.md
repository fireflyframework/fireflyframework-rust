# `firefly-admin`

> **Tier:** Platform · **Status:** Full · **Spring original:** Spring Boot Admin · **Python original:** `pyfly.admin`

## Overview

`firefly-admin` is an embedded, **Spring-Boot-Admin-style management
dashboard**: a self-contained single-page app mounted at a configurable path,
backed by a JSON API and live SSE streams. It is the Rust port of pyfly's
`pyfly.admin` package, modeled on [`firefly-actuator`]'s `mount()` idiom.

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
| `GET {base}/api/transactions` | Saga / workflow / TCC definition listing |
| `GET {base}/api/traces` | HTTP-trace ring buffer (cap 500) |
| `GET {base}/api/logfile` + `POST …/clear` | In-memory log ring buffer (cap 2000) |
| `GET {base}/api/runtime` / `server` | tokio worker/task counts + process RSS |
| `GET {base}/api/views[/{id}]` | Custom `AdminView` plugin listing + payloads |
| `GET {base}/api/sse/*` | Live streams: `health` / `metrics` / `traces` / `logfile` / `runtime` / `server` |
| `GET/POST/DELETE {base}/api/instances` | Multi-instance server mode (when enabled) |

Bind the dashboard on a separate admin port, or guard it with
`require_auth`, so it never leaks onto the public network.

## Why a separate crate?

Spring Boot Admin is the canonical "single pane of glass" every operator
expects — live health, metrics trends, log tailing, request traces, and
runtime-level logger tweaks, without redeploying. pyfly ships the same
dashboard; `firefly-admin` brings it to the Rust port with a byte-identical
`/admin/api` contract, so the **vendored SPA assets are reused verbatim** —
the JavaScript never had to change.

Three adaptations from pyfly:

- **Explicit wiring instead of a DI container.** pyfly discovers its
  providers' collaborators from the application container; Rust has none, so
  every collaborator is handed in through the `AdminDeps` struct (constructor
  injection). Required pieces (health, metrics, a `TraceBuffer`, a
  `LogBuffer`) are always present; the rest are `Option`s the endpoints
  degrade gracefully around.
- **`tracing` instead of Python `logging`.** The log viewer is fed by
  `LogBuffer`, which is *both* a ring buffer and a
  `tracing_subscriber::Layer`; logger-level mutation rewrites an `EnvFilter`
  directive through a `reload::Handle` (via `firefly-actuator`'s
  `LoggersState`).
- **Omitted Python-runtime introspection.** Bean / bean-graph / conditions /
  autowired views (DI-container reflection), Python GC/thread stats, and ASGI
  server specifics have no Rust analogue and are dropped. `GET /api/views`
  returns the `AdminView`-driven plugin list in their place; `runtime`
  reports tokio task/worker counts + process RSS via `sysinfo`.

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
/ monotonic id) and push only new rows, matching pyfly's `last_count` /
`last_id` semantics.

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

## Mapping from pyfly

| pyfly | firefly-admin |
|-------|---------------|
| `AdminProperties` / `AdminServerProperties` / `AdminClientProperties` | `AdminConfig` / `AdminServerConfig` / `AdminClientConfig` |
| `AdminRouteBuilder.build_routes()` | `mount(cfg, deps)` |
| `AdminViewExtension` / `AdminViewRegistry` | `AdminView` trait / `AdminViewRegistry` |
| `TraceCollectorFilter` | `TraceBuffer` + `TraceLayer` |
| `AdminLogHandler` | `LogBuffer` (a `tracing` Layer) |
| `InstanceRegistry` / `StaticDiscovery` | `InstanceRegistry` (+ `discover_static`) |
| `AdminClientRegistration` | `AdminClient` |
| bean / conditions / autowired views | *omitted (no DI container)* |
| Python GC/thread runtime, ASGI server info | tokio task/worker + RSS runtime, axum/tokio server info |

[`firefly-actuator`]: ../actuator/README.md
