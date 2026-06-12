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

//! # firefly-observability
//!
//! Three orthogonal observability concerns for Firefly Rust services —
//! the counterpart of the Java `firefly-otel-spring-boot-starter`, the
//! .NET `FireflyFramework.Observability` project, and the Go
//! `observability` module:
//!
//! 1. **Structured logging** — a [`tracing_subscriber`] layer
//!    ([`CorrelationLayer`]) that formats every event as one JSON (or
//!    logfmt text) line and auto-enriches it with the correlation id from
//!    the [`firefly_kernel`] task-local scope. JSON field names (`time`,
//!    `level`, `msg`, `service`, `correlationId`) are identical to the Go
//!    `slog` output, so one log pipeline parses every port.
//! 2. **Health indicators** — composable [`Indicator`] probes with a
//!    [`Composite`] aggregator producing the canonical
//!    UP / DEGRADED / DOWN / UNKNOWN rollup and per-check timing.
//! 3. **Startup banner** — the ASCII Firefly banner + canonical metadata
//!    block (framework version, foundation/license, app, runtime, active
//!    profiles, optional Swagger-UI URL). [`print_banner`] renders it
//!    plainly; [`BannerPrinter`] adds [`BannerMode`] selection, custom
//!    banner files, and TTY-aware ANSI colour.
//!
//! OpenTelemetry SDK wiring (exporters, sampling, resource attributes) is
//! left to the application's `main.rs` — this crate exposes only the
//! building blocks that compose with the `tracing` ecosystem.
//!
//! ## Quick start
//!
//! ```
//! use firefly_observability::{
//!     subscriber_with_writer, BufferWriter, Composite, HealthResult, IndicatorFn, LogConfig,
//!     Status,
//! };
//!
//! // Logging: every record carries the correlation id in scope.
//! let buf = BufferWriter::new();
//! let cfg = LogConfig::new().with_service("orders");
//! tracing::subscriber::with_default(subscriber_with_writer(cfg, buf.clone()), || {
//!     firefly_kernel::with_correlation_id_sync("abc-123", || {
//!         tracing::info!(id = "42", "placed order");
//!     });
//! });
//! // {"time":"…","level":"INFO","msg":"placed order","service":"orders","correlationId":"abc-123","id":"42"}
//! assert!(buf.as_string().contains(r#""correlationId":"abc-123""#));
//!
//! // Health: DOWN beats DEGRADED beats UP.
//! let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
//! rt.block_on(async {
//!     let health = Composite::new();
//!     health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
//!     health.add(IndicatorFn::new("cache", || async {
//!         HealthResult::degraded("cold start")
//!     }));
//!     let (overall, results) = health.check_all().await;
//!     assert_eq!(overall, Status::Degraded);
//!     assert_eq!(results.len(), 2);
//! });
//!
//! // Banner: printed by starter-core on boot.
//! let banner = firefly_observability::banner_string("starter-core", "orders");
//! assert!(banner.contains("Firefly Framework for Rust"));
//! ```
//!
//! The `firefly-actuator` crate mounts a composite like this one on
//! `GET /actuator/health`.

#![warn(missing_docs)]

mod appender;
mod banner;
mod config_loader;
mod health;
mod logging;
mod metrics;
mod process_metrics;
mod redaction;
mod trace_context;

pub use appender::{parse_size, FileConfig, RollingFileWriter, TeeWriter};
pub use banner::{
    banner_string, print_banner, render_banner, BannerConfig, BannerData, BannerMode,
    BannerPrinter, RUSTC_VERSION,
};
pub use config_loader::{apply_external_config, load_log_config, ConfigLoadError};
pub use health::{Composite, HealthResult, Indicator, IndicatorFn, Status};
pub use logging::{
    init_logging, init_logging_with_handle, subscriber, subscriber_with_handle,
    subscriber_with_writer, subscriber_with_writer_and_handle, BufferWriter, CorrelationLayer,
    LevelHandle, LogConfig, LogFormat, ROOT_TARGET,
};
pub use metrics::{
    counted, counted_result, sanitize_metric_name, timed, timed_result, Counted, Counter, Gauge,
    Histogram, LabeledCounter, LabeledGauge, LabeledHistogram, MetricsRegistry, Timed,
    DEFAULT_BUCKETS,
};
pub use process_metrics::{
    ProcessMetricsCollector, PROCESS_START_TIME_SECONDS, PROCESS_UPTIME_SECONDS, SYSTEM_CPU_COUNT,
};
pub use redaction::{
    build_redactor, builtin_pattern, luhn_valid, MaskStyle, RedactionConfig, Redactor,
    RegexRedactor, BUILTIN_ENTITIES, REDACTED,
};
pub use trace_context::{
    current_traceparent, current_tracestate, inject_headers, inject_reqwest, with_trace_context,
    TraceContextError, TraceContextLayer, TraceContextService, TraceParent, TraceState,
    TRACEPARENT_HEADER, TRACESTATE_HEADER,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_types_are_send_sync() {
        assert_send_sync::<Composite>();
        assert_send_sync::<HealthResult>();
        assert_send_sync::<Status>();
        assert_send_sync::<CorrelationLayer>();
        assert_send_sync::<BufferWriter>();
        assert_send_sync::<LogConfig>();
        assert_send_sync::<BannerData>();
        assert_send_sync::<BannerPrinter>();
        assert_send_sync::<BannerMode>();
        // pyfly-parity surface
        assert_send_sync::<MetricsRegistry>();
        assert_send_sync::<Counter>();
        assert_send_sync::<Gauge>();
        assert_send_sync::<Histogram>();
        assert_send_sync::<ProcessMetricsCollector>();
        assert_send_sync::<TraceParent>();
        assert_send_sync::<TraceState>();
        assert_send_sync::<TraceContextLayer>();
        assert_send_sync::<RegexRedactor>();
        assert_send_sync::<RedactionConfig>();
        assert_send_sync::<LevelHandle>();
        assert_send_sync::<FileConfig>();
        assert_send_sync::<RollingFileWriter>();
    }
}
