//! # firefly-actuator
//!
//! The framework's canonical **management endpoints** — the Rust
//! counterpart of Spring Boot's `actuator` and the Go port's `actuator`
//! module:
//!
//! | Endpoint                 | What it returns                                                                 |
//! |--------------------------|---------------------------------------------------------------------------------|
//! | `GET /actuator/health`   | Per-indicator + overall status; 200 UP/DEGRADED, 503 DOWN                        |
//! | `GET /actuator/info`     | Build info + app metadata + info contributors                                    |
//! | `GET /actuator/metrics`  | Prometheus exposition format from the [`MetricRegistry`] primitives              |
//! | `GET /actuator/env`      | Redacted environment view (`FIREFLY_*` visible by default; everything else `***`)|
//! | `GET /actuator/tasks`    | `{"count": N}` alive tokio tasks; `?dump=true` returns a runtime report          |
//! | `GET /actuator/version`  | `{"firefly":"26.6.1","app":"orders","appVersion":"…","rust":"…"}`                |
//!
//! Bind these on a separate admin port (e.g. `:8081`) so they never leak
//! onto the public network.
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

mod handler;
mod health;
mod metrics;

pub use handler::{mount, ActuatorConfig, InfoContributor};
pub use health::{HealthComposite, HealthIndicator, HealthResult, HealthStatus, IndicatorFn};
pub use metrics::{Counter, Gauge, MetricRegistry};

/// Released framework version. Calendar-versioned (`YY.M.PATCH`), the
/// Rust port's counterpart of the Go `kernel.Version` constant.
pub const VERSION: &str = "26.6.1";

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
    }

    #[test]
    fn version_matches_crate_version() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }
}
