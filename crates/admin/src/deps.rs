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

//! The [`AdminDeps`] wiring struct — explicit constructor injection for the
//! admin router.
//!
//! Where pyfly discovers its providers' collaborators from the DI container,
//! the Rust port has no container: every collaborator the dashboard needs is
//! handed in through this struct (the analogue of constructor injection). The
//! starter builds it once and passes it to [`mount`](crate::mount).

use std::sync::Arc;

use firefly_actuator::{CacheOps, HealthComposite, LoggersState, MetricRegistry};
use firefly_cqrs::Bus;
use firefly_orchestration::OrchestrationRegistry;
use firefly_scheduling::Scheduler;

use crate::instance::InstanceRegistry;
use crate::log::LogBuffer;
use crate::trace::TraceBuffer;
use crate::view::AdminView;

/// Everything the admin router needs, wired explicitly — the Rust answer to
/// pyfly's container-discovered providers.
///
/// Required collaborators ([`health`](Self::health),
/// [`metrics`](Self::metrics), [`traces`](Self::traces),
/// [`logs`](Self::logs)) are always present; the rest are `Option`s that the
/// corresponding endpoints degrade gracefully around (a missing scheduler ⇒
/// an empty task list, a missing cache ⇒ `{"available": false}`, …), matching
/// pyfly's lazily-resolved providers.
#[derive(Clone)]
pub struct AdminDeps {
    /// Application name surfaced on `/admin/api/overview` and
    /// `/admin/api/settings`.
    pub app_name: String,
    /// Application version surfaced on `/admin/api/overview`.
    pub app_version: String,
    /// Health aggregator behind `/admin/api/health` (+ its SSE stream and the
    /// overview health block).
    pub health: Arc<HealthComposite>,
    /// Metric registry behind `/admin/api/metrics[/{name}]` (+ its SSE
    /// stream).
    pub metrics: Arc<MetricRegistry>,
    /// HTTP-trace ring buffer behind `/admin/api/traces` (+ its SSE stream),
    /// fed by [`TraceLayer`](crate::TraceLayer).
    pub traces: Arc<TraceBuffer>,
    /// Log ring buffer behind `/admin/api/logfile[/clear]` (+ its SSE
    /// stream), fed by the [`LogBuffer`] `tracing` layer.
    pub logs: LogBuffer,
    /// Optional task scheduler behind `/admin/api/scheduled`.
    pub scheduler: Option<Arc<Scheduler>>,
    /// Optional CQRS bus behind `/admin/api/cqrs` (handler listing).
    pub bus: Option<Arc<Bus>>,
    /// Optional orchestration registry behind `/admin/api/transactions`
    /// (saga / workflow / TCC listing).
    pub orchestration: Option<Arc<OrchestrationRegistry>>,
    /// Optional cache operations behind `/admin/api/caches[…]`.
    pub cache: Option<Arc<dyn CacheOps>>,
    /// Optional runtime logger control behind `/admin/api/loggers[/{name}]`
    /// (a `tracing_subscriber` reload handle). Read-only listing still works
    /// without it.
    pub loggers: Option<Arc<LoggersState>>,
    /// Optional instance registry — present iff **server mode** is enabled.
    /// Drives `/admin/api/instances` and the `serverMode` settings flag.
    pub instances: Option<Arc<InstanceRegistry>>,
    /// Custom dashboard views, surfaced under `/admin/api/views`.
    pub views: Vec<Arc<dyn AdminView>>,
}

impl AdminDeps {
    /// Builds a minimal deps set with only the required collaborators wired;
    /// the optional fields can be set afterwards with struct-update syntax.
    ///
    /// ```
    /// use std::sync::Arc;
    /// use firefly_actuator::{HealthComposite, MetricRegistry};
    /// use firefly_admin::{AdminDeps, LogBuffer, TraceBuffer};
    ///
    /// let deps = AdminDeps::new(
    ///     "orders",
    ///     "1.4.0",
    ///     Arc::new(HealthComposite::new()),
    ///     Arc::new(MetricRegistry::new()),
    ///     Arc::new(TraceBuffer::new()),
    ///     LogBuffer::new(),
    /// );
    /// assert_eq!(deps.app_name, "orders");
    /// assert!(deps.scheduler.is_none());
    /// ```
    pub fn new(
        app_name: impl Into<String>,
        app_version: impl Into<String>,
        health: Arc<HealthComposite>,
        metrics: Arc<MetricRegistry>,
        traces: Arc<TraceBuffer>,
        logs: LogBuffer,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            app_version: app_version.into(),
            health,
            metrics,
            traces,
            logs,
            scheduler: None,
            bus: None,
            orchestration: None,
            cache: None,
            loggers: None,
            instances: None,
            views: Vec::new(),
        }
    }
}
