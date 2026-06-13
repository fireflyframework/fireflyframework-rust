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

//! CQRS dispatch metrics — pyfly's `CqrsMetricsService` (`command/metrics.py`)
//! recording per-dispatch counters and a processing-time histogram.
//!
//! [`CqrsMetrics`] records into a [`firefly_observability::MetricsRegistry`]
//! using the same metric names pyfly emits, so dashboards port across
//! runtimes:
//!
//! | Metric                                          | Type      | Recorded on            |
//! |-------------------------------------------------|-----------|------------------------|
//! | `firefly_cqrs_command_processed`                | counter   | successful command     |
//! | `firefly_cqrs_command_failed`                   | counter   | failed command         |
//! | `firefly_cqrs_command_validation_failed`        | counter   | validation rejection   |
//! | `firefly_cqrs_command_processing_time_seconds`  | histogram | every command dispatch |
//! | `firefly_cqrs_query_processed`                  | counter   | successful query       |
//! | `firefly_cqrs_query_failed`                     | counter   | failed query           |
//! | `firefly_cqrs_query_processing_time_seconds`    | histogram | every query dispatch   |
//!
//! [`MetricsMiddleware`] is the drop-in bus hook: install it on the
//! [`Bus`](crate::Bus) and every dispatch is timed and counted. Because the
//! bus does not distinguish commands from queries (both share one registry),
//! the middleware records under the **command** family by default; construct
//! it with [`MetricsMiddleware::for_queries`] on a query-only bus to record
//! under the query family instead.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_cqrs::{Bus, CqrsError, CqrsMetrics, Message, MetricsMiddleware};
//! use firefly_observability::MetricsRegistry;
//! use serde::Serialize;
//!
//! #[derive(Clone, Serialize)]
//! struct Ping;
//! impl Message for Ping {}
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let registry = Arc::new(MetricsRegistry::isolated());
//! let metrics = Arc::new(CqrsMetrics::new(registry.clone()));
//!
//! let bus = Bus::new();
//! bus.use_middleware(MetricsMiddleware::new(metrics.clone()));
//! bus.register(|_p: Ping| async move { Ok::<_, CqrsError>(()) });
//!
//! let _: () = bus.send(Ping).await.unwrap();
//! assert_eq!(metrics.commands_processed(), 1.0);
//! # });
//! ```

use std::sync::Arc;
use std::time::Instant;

use firefly_observability::{Counter, Histogram, MetricsRegistry};

use crate::bus::{DynHandler, Envelope, HandlerFuture, Middleware};
use crate::CqrsError;

/// Records CQRS command / query dispatch metrics into a
/// [`MetricsRegistry`] — pyfly's `CqrsMetricsService`.
///
/// Cheaply cloneable (the metric handles are `Arc`-backed). Record manually
/// via [`CqrsMetrics::record_command_success`] etc., or install a
/// [`MetricsMiddleware`] to record automatically around every dispatch.
#[derive(Clone)]
pub struct CqrsMetrics {
    cmd_processed: Arc<Counter>,
    cmd_failed: Arc<Counter>,
    cmd_validation_failed: Arc<Counter>,
    cmd_time: Arc<Histogram>,
    qry_processed: Arc<Counter>,
    qry_failed: Arc<Counter>,
    qry_time: Arc<Histogram>,
}

impl CqrsMetrics {
    /// Registers the CQRS metric family on `registry` and returns the
    /// recorder. Re-registering against the same registry returns the same
    /// metric handles (the registry deduplicates by name).
    pub fn new(registry: Arc<MetricsRegistry>) -> Self {
        Self {
            cmd_processed: registry.counter(
                "firefly_cqrs_command_processed",
                "Successful commands",
                &[],
            ),
            cmd_failed: registry.counter("firefly_cqrs_command_failed", "Failed commands", &[]),
            cmd_validation_failed: registry.counter(
                "firefly_cqrs_command_validation_failed",
                "Command validation failures",
                &[],
            ),
            cmd_time: registry.histogram(
                "firefly_cqrs_command_processing_time_seconds",
                "Command processing duration",
                &[],
                None,
            ),
            qry_processed: registry.counter(
                "firefly_cqrs_query_processed",
                "Successful queries",
                &[],
            ),
            qry_failed: registry.counter("firefly_cqrs_query_failed", "Failed queries", &[]),
            qry_time: registry.histogram(
                "firefly_cqrs_query_processing_time_seconds",
                "Query processing duration",
                &[],
                None,
            ),
        }
    }

    /// Records a successful command and its processing duration (seconds) —
    /// pyfly's `record_command_success`.
    pub fn record_command_success(&self, duration_secs: f64) {
        self.cmd_processed.inc();
        self.cmd_time.observe(duration_secs);
    }

    /// Records a failed command and its processing duration (seconds) —
    /// pyfly's `record_command_failure`.
    pub fn record_command_failure(&self, duration_secs: f64) {
        self.cmd_failed.inc();
        self.cmd_time.observe(duration_secs);
    }

    /// Records a command validation rejection — pyfly's
    /// `record_validation_failure`.
    pub fn record_validation_failure(&self) {
        self.cmd_validation_failed.inc();
    }

    /// Records a successful query and its processing duration (seconds) —
    /// pyfly's `record_query_success`.
    pub fn record_query_success(&self, duration_secs: f64) {
        self.qry_processed.inc();
        self.qry_time.observe(duration_secs);
    }

    /// Records a failed query and its processing duration (seconds) —
    /// pyfly's `record_query_failure`.
    pub fn record_query_failure(&self, duration_secs: f64) {
        self.qry_failed.inc();
        self.qry_time.observe(duration_secs);
    }

    /// Current `firefly_cqrs_command_processed` count.
    pub fn commands_processed(&self) -> f64 {
        self.cmd_processed.value()
    }

    /// Current `firefly_cqrs_command_failed` count.
    pub fn commands_failed(&self) -> f64 {
        self.cmd_failed.value()
    }

    /// Current `firefly_cqrs_command_validation_failed` count.
    pub fn commands_validation_failed(&self) -> f64 {
        self.cmd_validation_failed.value()
    }

    /// Current `firefly_cqrs_query_processed` count.
    pub fn queries_processed(&self) -> f64 {
        self.qry_processed.value()
    }

    /// Current `firefly_cqrs_query_failed` count.
    pub fn queries_failed(&self) -> f64 {
        self.qry_failed.value()
    }
}

/// Bus [`Middleware`] that times every dispatch and records the outcome into
/// a [`CqrsMetrics`] — pyfly's metrics step in `DefaultCommandBus._execute` /
/// `DefaultQueryBus`.
///
/// Records under the **command** family by default; use
/// [`MetricsMiddleware::for_queries`] on a query-only bus to record under the
/// query family. A [`CqrsError::Validation`] failure additionally bumps the
/// validation-failed counter (pyfly's audit #99).
#[derive(Clone)]
pub struct MetricsMiddleware {
    metrics: Arc<CqrsMetrics>,
    queries: bool,
}

impl MetricsMiddleware {
    /// Records every dispatch under the command metric family.
    pub fn new(metrics: Arc<CqrsMetrics>) -> Self {
        Self {
            metrics,
            queries: false,
        }
    }

    /// Records every dispatch under the query metric family — install this on
    /// a query-only bus.
    pub fn for_queries(metrics: Arc<CqrsMetrics>) -> Self {
        Self {
            metrics,
            queries: true,
        }
    }
}

impl Middleware for MetricsMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        let metrics = Arc::clone(&self.metrics);
        let queries = self.queries;
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            let metrics = Arc::clone(&metrics);
            Box::pin(async move {
                let start = Instant::now();
                let result = next(env).await;
                let elapsed = start.elapsed().as_secs_f64();
                match &result {
                    Ok(_) => {
                        if queries {
                            metrics.record_query_success(elapsed);
                        } else {
                            metrics.record_command_success(elapsed);
                        }
                    }
                    Err(err) => {
                        if matches!(err, CqrsError::Validation(_)) && !queries {
                            metrics.record_validation_failure();
                        }
                        if queries {
                            metrics.record_query_failure(elapsed);
                        } else {
                            metrics.record_command_failure(elapsed);
                        }
                    }
                }
                result
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bus, Message, ValidationMiddleware};

    #[derive(Clone, serde::Serialize)]
    struct Ping;
    impl Message for Ping {}

    #[derive(Clone, serde::Serialize)]
    struct Bad;
    impl Message for Bad {
        fn validate(&self) -> Result<(), CqrsError> {
            Err(CqrsError::validation("nope"))
        }
    }

    #[tokio::test]
    async fn records_command_success() {
        let registry = Arc::new(MetricsRegistry::isolated());
        let metrics = Arc::new(CqrsMetrics::new(registry));
        let bus = Bus::new();
        bus.use_middleware(MetricsMiddleware::new(metrics.clone()));
        bus.register(|_p: Ping| async move { Ok::<_, CqrsError>(()) });
        let _: () = bus.send(Ping).await.unwrap();
        assert_eq!(metrics.commands_processed(), 1.0);
        assert_eq!(metrics.commands_failed(), 0.0);
    }

    #[tokio::test]
    async fn records_command_failure() {
        let registry = Arc::new(MetricsRegistry::isolated());
        let metrics = Arc::new(CqrsMetrics::new(registry));
        let bus = Bus::new();
        bus.use_middleware(MetricsMiddleware::new(metrics.clone()));
        bus.register(|_p: Ping| async move { Err::<(), _>(CqrsError::handler("boom")) });
        let res: Result<(), _> = bus.send(Ping).await;
        assert!(res.is_err());
        assert_eq!(metrics.commands_failed(), 1.0);
        assert_eq!(metrics.commands_processed(), 0.0);
    }

    #[tokio::test]
    async fn records_validation_failure() {
        let registry = Arc::new(MetricsRegistry::isolated());
        let metrics = Arc::new(CqrsMetrics::new(registry));
        let bus = Bus::new();
        // Metrics outermost so it observes the validation middleware's error.
        bus.use_middleware(MetricsMiddleware::new(metrics.clone()));
        bus.use_middleware(ValidationMiddleware::new());
        bus.register(|_b: Bad| async move { Ok::<_, CqrsError>(()) });
        let res: Result<(), _> = bus.send(Bad).await;
        assert!(res.is_err());
        assert_eq!(metrics.commands_validation_failed(), 1.0);
        assert_eq!(metrics.commands_failed(), 1.0);
    }

    #[tokio::test]
    async fn records_query_family_when_configured() {
        let registry = Arc::new(MetricsRegistry::isolated());
        let metrics = Arc::new(CqrsMetrics::new(registry));
        let bus = Bus::new();
        bus.use_middleware(MetricsMiddleware::for_queries(metrics.clone()));
        bus.register(|_p: Ping| async move { Ok::<_, CqrsError>(()) });
        let _: () = bus.query(Ping).await.unwrap();
        assert_eq!(metrics.queries_processed(), 1.0);
        assert_eq!(metrics.commands_processed(), 0.0);
    }
}
