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

//! # firefly-actuator
//!
//! The framework's canonical **management endpoints** — the Rust
//! counterpart of Spring Boot's `actuator` and the Go port's `actuator`
//! module:
//!
//! | Endpoint                            | What it returns                                                                 |
//! |-------------------------------------|---------------------------------------------------------------------------------|
//! | `GET /actuator`                     | `_links` index of the exposed endpoints                                          |
//! | `GET /actuator/health`              | Per-indicator + overall status; 200 UP/DEGRADED, 503 DOWN                        |
//! | `GET /actuator/health/{selector}`   | `liveness` / `readiness` probes, named groups, per-component drill-down          |
//! | `GET /actuator/info`                | Build info + app metadata + info contributors                                    |
//! | `GET /actuator/metrics`             | Prometheus text (`Accept: application/json` → Micrometer `{"names":[…]}`)        |
//! | `GET /actuator/metrics/{name}`      | Micrometer JSON detail with `?tag=k:v` drill-down + `availableTags`              |
//! | `GET /actuator/prometheus`          | Prometheus exposition format (labeled), the scrape target                        |
//! | `GET /actuator/env`                 | Spring `{activeProfiles, propertySources}` when an [`EnvSource`] is wired; else a flat redacted env view |
//! | `GET /actuator/env/{toMatch}`       | One property's value across the ordered sources (when an [`EnvSource`] is wired)|
//! | `GET /actuator/tasks`               | `{"count": N}` alive tokio tasks; `?dump=true` returns a runtime report          |
//! | `GET /actuator/threaddump`          | Spring `{threads:[…]}` — the tokio runtime worker/task snapshot (Rust analog)    |
//! | `GET /actuator/version`             | `{"firefly":"26.6.13","app":"orders","appVersion":"…","rust":"…"}`                |
//! | `GET/POST /actuator/loggers[/{n}]`  | Runtime log levels over a `tracing_subscriber` reload handle                     |
//! | `GET /actuator/scheduledtasks`      | Tasks grouped by trigger (cron / fixedDelay / fixedRate)                         |
//! | `GET/POST /actuator/caches[…]`      | Configured caches + `POST /{name}/evict`                                         |
//! | `POST /actuator/refresh`            | `{"refreshed": [keys…]}` from the wired [`Refresher`]                            |
//! | `GET /actuator/httpexchanges`       | The last 100 exchanges recorded by [`HttpExchangesLayer`]                        |
//!
//! Which ids actually go on the wire — and under which base path — is
//! controlled by the Spring-style [`ExposureConfig`]; custom endpoints
//! implement the [`Endpoint`] trait and register on an
//! [`EndpointRegistry`]. Bind these on a separate admin port (e.g.
//! `:8081`) so they never leak onto the public network.
//!
//! ## Why a separate crate?
//!
//! Spring Boot's `actuator` is the canonical management surface every
//! operator expects — health probes for Kubernetes, metrics for
//! Prometheus, info for service catalogues. Without a unified
//! `actuator`, every Firefly service ends up handcrafting its own
//! `/healthz` with subtly different shapes. The JSON and Prometheus
//! wire formats match the Java, .NET, Go, and Python ports.
//!
//! Where Go counts goroutines on `/actuator/goroutines`, this port
//! counts alive tokio tasks on `/actuator/tasks` — the same operational
//! question ("how much concurrent work is in flight?") answered with the
//! runtime's native unit.
//!
//! ## Quick start
//!
//! ```
//! use std::sync::Arc;
//! use firefly_actuator::{
//!     mount, ActuatorConfig, HealthComposite, HealthResult, IndicatorFn, MetricRegistry,
//! };
//!
//! let health = Arc::new(HealthComposite::new());
//! health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
//!
//! let registry = Arc::new(MetricRegistry::new());
//! registry.counter("orders_placed_total").inc();
//! registry.gauge("queue_depth").set(42.5);
//!
//! let app: axum::Router = mount(ActuatorConfig {
//!     app_name: "orders".into(),
//!     health,
//!     metric_registry: registry,
//!     info_contributors: vec![Box::new(|| {
//!         let mut m = serde_json::Map::new();
//!         m.insert("git".into(), serde_json::json!({ "sha": "abc" }));
//!         m
//!     })],
//!     ..ActuatorConfig::default()
//! });
//! # let _ = app;
//! ```

mod caches;
mod endpoint;
mod env_source;
mod exposure;
mod handler;
mod health;
mod http_exchanges;
mod loggers;
mod metrics;
mod refresh;
mod scheduledtasks;
mod threaddump;

pub use caches::{CacheDescriptor, CacheOps};
pub use endpoint::{Endpoint, EndpointRegistry};
pub use env_source::{EnvSource, PropertySourceView, PropertyView};
pub use exposure::{ExposureConfig, DEFAULT_BASE_PATH};
pub use handler::{mount, ActuatorConfig, InfoContributor};
pub use health::{
    HealthComposite, HealthIndicator, HealthResult, HealthStatus, IndicatorFn, ProbeGroup,
};
pub use http_exchanges::{
    ExchangeRequest, ExchangeResponse, HttpExchange, HttpExchangeRecorder, HttpExchangesLayer,
    HttpExchangesService, DEFAULT_EXCHANGE_CAPACITY,
};
pub use loggers::{LoggersError, LoggersState, SPRING_LEVELS};
pub use metrics::{Counter, Gauge, Histogram, MetricRegistry, TimerGuard, DEFAULT_BUCKETS};
pub use refresh::Refresher;
pub use scheduledtasks::{ScheduledTasksSource, StaticScheduledTasks, TaskDescriptor, TaskTrigger};
pub use threaddump::{thread_dump, StackFrame, ThreadInfo};

/// Released framework version. Calendar-versioned (`YY.M.PATCH`), the
/// Rust port's counterpart of the Go `kernel.Version` constant.
pub const VERSION: &str = "26.6.13";

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send_and_sync() {
        assert_send_sync::<HealthComposite>();
        assert_send_sync::<HealthResult>();
        assert_send_sync::<HealthStatus>();
        assert_send_sync::<MetricRegistry>();
        assert_send_sync::<Counter>();
        assert_send_sync::<Gauge>();
        assert_send_sync::<ActuatorConfig>();
        // pyfly parity surface
        assert_send_sync::<ProbeGroup>();
        assert_send_sync::<ExposureConfig>();
        assert_send_sync::<EndpointRegistry>();
        assert_send_sync::<Histogram>();
        assert_send_sync::<LoggersState>();
        assert_send_sync::<HttpExchangeRecorder>();
        assert_send_sync::<HttpExchangesLayer>();
        assert_send_sync::<TaskDescriptor>();
        assert_send_sync::<CacheDescriptor>();
        assert_send_sync::<PropertySourceView>();
        assert_send_sync::<PropertyView>();
        assert_send_sync::<ThreadInfo>();
        assert_send_sync::<StackFrame>();
    }

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
