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

//! # firefly-admin
//!
//! An embedded, **Spring-Boot-Admin-style management dashboard** — a
//! self-contained single-page app mounted at a configurable path, backed by a
//! JSON API and live SSE streams. The Rust port of pyfly's `pyfly.admin`
//! package, modeled on [`firefly-actuator`](firefly_actuator)'s
//! [`mount`] idiom.
//!
//! | Surface | What it serves |
//! |---------|----------------|
//! | `GET {base}` + `{base}/static/*` | The vendored SPA shell + assets (served verbatim, `no-cache`) |
//! | `GET {base}/api/overview` | App info + health summary |
//! | `GET {base}/api/health` | Composite health (503 when DOWN) |
//! | `GET {base}/api/env` / `config` | Environment / config view |
//! | `GET/POST {base}/api/loggers[/{n}]` | Runtime log-level read/write over a `tracing_subscriber` reload handle |
//! | `GET {base}/api/metrics[/{n}]` | Meter listing + Micrometer detail |
//! | `GET {base}/api/scheduled` | Scheduled-task listing (from [`Scheduler::tasks`](firefly_scheduling::Scheduler::tasks)) |
//! | `GET {base}/api/caches` + `POST …/{name}/evict` | Cache listing + eviction |
//! | `GET {base}/api/cqrs` | CQRS handler listing (from [`Bus::handler_names`](firefly_cqrs::Bus::handler_names)) |
//! | `GET {base}/api/transactions` | Saga / workflow / TCC definition listing |
//! | `GET {base}/api/traces` | HTTP-trace ring buffer (fed by [`TraceLayer`]) |
//! | `GET {base}/api/logfile` + `POST …/clear` | In-memory log ring buffer (fed by [`LogBuffer`]) |
//! | `GET {base}/api/runtime` / `server` | tokio worker/task counts + process RSS |
//! | `GET {base}/api/views[/{id}]` | Custom [`AdminView`] plugin listing + payloads |
//! | `GET {base}/api/sse/*` | Live streams: health / metrics / traces / logfile / runtime / server |
//! | `GET/POST/DELETE {base}/api/instances` | Multi-instance server mode (when enabled) |
//!
//! ## Wiring
//!
//! [`mount`] takes an [`AdminConfig`] and an explicit [`AdminDeps`] struct —
//! the Rust answer to pyfly's container-discovered providers. Required
//! collaborators (health, metrics, a [`TraceBuffer`], a [`LogBuffer`]) are
//! always present; the rest are `Option`s the endpoints degrade gracefully
//! around.
//!
//! ```no_run
//! use std::sync::Arc;
//! use firefly_actuator::{HealthComposite, MetricRegistry};
//! use firefly_admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer, TraceLayer};
//!
//! # async fn demo() {
//! let traces = Arc::new(TraceBuffer::new());
//! let logs = LogBuffer::new();
//!
//! // Install the log layer on the global subscriber (capture every event):
//! use tracing_subscriber::prelude::*;
//! // tracing_subscriber::registry().with(logs.clone()).init();
//!
//! let deps = AdminDeps::new(
//!     "orders", "1.4.0",
//!     Arc::new(HealthComposite::new()),
//!     Arc::new(MetricRegistry::new()),
//!     Arc::clone(&traces),
//!     logs,
//! );
//!
//! // The dashboard router (nest it, or serve on a dedicated admin port):
//! let admin = mount(AdminConfig::default(), deps);
//!
//! // Record HTTP traces on the *application* router (admin/actuator excluded):
//! let app: axum::Router = axum::Router::new()
//!     .layer(TraceLayer::new(traces));
//! # let _ = (admin, app);
//! # }
//! ```
//!
//! ## Live streams (SSE)
//!
//! Each `/api/sse/*` route returns an [`axum::response::Sse`] driven by a
//! [`tokio::time::interval`]; `traces` and `logfile` are incremental (only new
//! rows are pushed, via a monotonic cursor), matching pyfly's streams.
//!
//! ## Server / client mode
//!
//! With server mode on (set [`AdminDeps::instances`]), the dashboard tracks
//! downstream instances via an [`InstanceRegistry`] and exposes
//! register/deregister routes. The complementary [`AdminClient`] self-registers
//! this application with a remote admin server on lifecycle start and
//! deregisters on stop.
//!
//! ## Omitted by design
//!
//! Python-runtime introspection with no Rust analogue — bean / bean-graph /
//! conditions / autowired (no DI container), Python GC/thread stats, and ASGI
//! server specifics — is excluded. `GET /api/views` returns the
//! [`AdminView`]-driven list in their place; `runtime` reports tokio
//! task/worker counts + process RSS.

#![warn(missing_docs)]

mod auth;
mod client;
mod config;
mod data;
mod deps;
mod env;
mod instance;
mod log;
mod router;
mod sse;
mod trace;
mod view;

pub use client::AdminClient;
pub use config::{AdminClientConfig, AdminConfig, AdminServerConfig, InstanceConfig};
pub use deps::AdminDeps;
pub use env::{EnvironmentSnapshot, PropertyEntry, PropertySource};
pub use instance::{InstanceInfo, InstanceRegistry};
pub use log::{LogBuffer, LogRecord, DEFAULT_LOG_CAPACITY};
pub use router::mount;
pub use trace::{TraceBuffer, TraceEntry, TraceLayer, TraceService, DEFAULT_TRACE_CAPACITY};
pub use view::{AdminView, AdminViewRegistry};

/// Released framework version. Calendar-versioned (`YY.M.PATCH`), the Rust
/// port's counterpart of the Go `kernel.Version` constant.
pub const VERSION: &str = "26.6.7";

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send_and_sync() {
        assert_send_sync::<AdminConfig>();
        assert_send_sync::<AdminServerConfig>();
        assert_send_sync::<AdminClientConfig>();
        assert_send_sync::<AdminDeps>();
        assert_send_sync::<TraceBuffer>();
        assert_send_sync::<TraceLayer>();
        assert_send_sync::<LogBuffer>();
        assert_send_sync::<InstanceRegistry>();
        assert_send_sync::<InstanceInfo>();
        assert_send_sync::<AdminClient>();
        assert_send_sync::<AdminViewRegistry>();
    }

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
