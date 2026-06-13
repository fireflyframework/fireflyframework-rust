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

//! Optional actuator integration (feature `actuator`): a relational
//! [`HealthIndicator`](firefly_actuator::HealthIndicator) and a per-query
//! metrics recorder.
//!
//! - [`SqlxHealthIndicator`] is the Rust port of pyfly's
//!   `SqlAlchemyHealthIndicator`: it runs `SELECT 1` against the pool and
//!   reports `UP` (with the backend kind on `details.database`) on success,
//!   `DOWN` (with the error) on failure — the `db` component on
//!   `GET /actuator/health`.
//! - [`SqlxQueryMetrics`] is the Rust port of pyfly's
//!   `SqlAlchemyQueryMetrics`: it records a query-duration histogram, a query
//!   counter, and a query-error counter, all labelled by a bounded
//!   `operation` set (`SELECT` / `INSERT` / `UPDATE` / `DELETE` / `OTHER`),
//!   using the same metric names pyfly emits
//!   (`pyfly_db_query_duration_seconds` / `pyfly_db_queries_total` /
//!   `pyfly_db_query_errors_total`) so cross-port dashboards carry over.
//!
//! Both are off the base build; enable the `actuator` feature to compile
//! them and register them from a starter.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use firefly_actuator::{HealthIndicator, HealthResult, HealthStatus};
use firefly_observability::{Counter, Histogram, MetricsRegistry};

use crate::db::{Backend, Db};

/// The metric name for the per-query duration histogram (pyfly parity).
pub const DB_QUERY_DURATION: &str = "pyfly_db_query_duration_seconds";
/// The metric name for the per-query counter (pyfly parity).
pub const DB_QUERIES_TOTAL: &str = "pyfly_db_queries_total";
/// The metric name for the per-query error counter (pyfly parity).
pub const DB_QUERY_ERRORS_TOTAL: &str = "pyfly_db_query_errors_total";

/// The lower-cased backend label reported in the health `details.database`
/// field — `postgres` / `mysql` / `sqlite`.
fn backend_label(backend: Backend) -> &'static str {
    match backend {
        Backend::Postgres => "postgres",
        Backend::MySql => "mysql",
        Backend::Sqlite => "sqlite",
    }
}

/// A relational database [`HealthIndicator`] — `UP` iff `SELECT 1` succeeds.
///
/// The Rust port of pyfly's `SqlAlchemyHealthIndicator`: registered as the
/// `db` component on `GET /actuator/health`, it pings the pool with
/// `SELECT 1` and reports the backend kind on `details.database` so the
/// actuator response makes the probe obvious. A failed ping reports `DOWN`
/// with the error message on `details.error`.
#[derive(Clone)]
pub struct SqlxHealthIndicator {
    db: Db,
    name: String,
}

impl SqlxHealthIndicator {
    /// Builds the indicator over `db`, named `db` (the conventional Spring
    /// Boot / pyfly component name).
    pub fn new(db: Db) -> Self {
        SqlxHealthIndicator {
            db,
            name: "db".to_string(),
        }
    }

    /// Builds the indicator with a custom component `name` — useful when a
    /// service wires more than one datasource and wants each probed under a
    /// distinct name (`db`, `db-reporting`, …), the named-datasource health
    /// path.
    pub fn named(db: Db, name: impl Into<String>) -> Self {
        SqlxHealthIndicator {
            db,
            name: name.into(),
        }
    }

    /// Runs `SELECT 1` against the pool, returning `Ok(())` on success.
    async fn ping(&self) -> Result<(), String> {
        match &self.db {
            #[cfg(feature = "postgres")]
            Db::Postgres(pool) => sqlx::query("SELECT 1")
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            #[cfg(feature = "mysql")]
            Db::MySql(pool) => sqlx::query("SELECT 1")
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            #[cfg(feature = "sqlite")]
            Db::Sqlite(pool) => sqlx::query("SELECT 1")
                .execute(pool)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            #[allow(unreachable_patterns)]
            _ => Err("no backend feature enabled".to_string()),
        }
    }
}

#[async_trait]
impl HealthIndicator for SqlxHealthIndicator {
    fn name(&self) -> &str {
        &self.name
    }

    async fn check(&self) -> HealthResult {
        let started = Instant::now();
        match self.ping().await {
            Ok(()) => {
                let mut details = serde_json::Map::new();
                details.insert(
                    "database".to_string(),
                    serde_json::Value::String(backend_label(self.db.backend()).to_string()),
                );
                let mut r = HealthResult::new(HealthStatus::Up).with_details(details);
                r.duration = started.elapsed();
                r
            }
            Err(e) => {
                let mut details = serde_json::Map::new();
                // pyfly truncates the error message to 200 chars.
                let msg: String = e.chars().take(200).collect();
                details.insert("error".to_string(), serde_json::Value::String(msg.clone()));
                let mut r = HealthResult::down(msg).with_details(details);
                r.duration = started.elapsed();
                r
            }
        }
    }
}

/// Per-query metrics recorder — the Rust port of pyfly's
/// `SqlAlchemyQueryMetrics`.
///
/// Holds the three metric handles (registered once at construction against a
/// [`MetricsRegistry`]) and records, per executed statement, a
/// duration observation, a query-count increment, and — on failure — an
/// error-count increment, all labelled by a **bounded** `operation`
/// ([`operation_label`]) so Prometheus cardinality stays bounded regardless
/// of query text. Call [`SqlxQueryMetrics::record`] after running a statement
/// (wrapping the repository's read/write helpers).
#[derive(Clone)]
pub struct SqlxQueryMetrics {
    duration: Arc<Histogram>,
    count: Arc<Counter>,
    errors: Arc<Counter>,
}

impl SqlxQueryMetrics {
    /// Registers the three query metrics against `registry`, matching
    /// pyfly's metric names and the single `operation` label.
    pub fn new(registry: &MetricsRegistry) -> Self {
        let duration = registry.histogram(
            DB_QUERY_DURATION,
            "Database query execution time",
            &["operation"],
            None,
        );
        let count = registry.counter(
            DB_QUERIES_TOTAL,
            "Database queries executed",
            &["operation"],
        );
        let errors = registry.counter(
            DB_QUERY_ERRORS_TOTAL,
            "Database query errors",
            &["operation"],
        );
        SqlxQueryMetrics {
            duration,
            count,
            errors,
        }
    }

    /// Records one statement execution: a duration observation + a count
    /// increment, and an error increment when `errored`. `sql` drives the
    /// bounded `operation` label.
    pub fn record(&self, sql: &str, elapsed_secs: f64, errored: bool) {
        let op = operation_label(sql);
        self.count.labels(&[op]).inc();
        self.duration.labels(&[op]).observe(elapsed_secs);
        if errored {
            self.errors.labels(&[op]).inc();
        }
    }

    /// Times `f`, recording the duration / count / error metrics for `sql`,
    /// and returns `f`'s result. The convenient wrapper around an executed
    /// statement.
    pub async fn timed<F, Fut, T, E>(&self, sql: &str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let started = Instant::now();
        let result = f().await;
        let elapsed = started.elapsed().as_secs_f64();
        self.record(sql, elapsed, result.is_err());
        result
    }
}

/// Maps a SQL statement to its bounded `operation` label — the leading verb
/// normalised to `SELECT` / `INSERT` / `UPDATE` / `DELETE`, or `OTHER` for
/// everything else (DDL, `CALL`, `MERGE`, empty, …). The Rust port of
/// pyfly's `_operation`.
pub fn operation_label(sql: &str) -> &'static str {
    let first = sql
        .trim_start()
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    match first.as_str() {
        "SELECT" => "SELECT",
        "INSERT" => "INSERT",
        "UPDATE" => "UPDATE",
        "DELETE" => "DELETE",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_label_is_bounded() {
        assert_eq!(operation_label("SELECT * FROM t"), "SELECT");
        assert_eq!(operation_label("  insert into t values (1)"), "INSERT");
        assert_eq!(operation_label("UPDATE t SET x = 1"), "UPDATE");
        assert_eq!(operation_label("delete from t"), "DELETE");
        assert_eq!(operation_label("CREATE TABLE t (id INT)"), "OTHER");
        assert_eq!(operation_label("MERGE INTO t"), "OTHER");
        assert_eq!(operation_label(""), "OTHER");
    }

    #[test]
    fn metrics_record_increments_bounded_labels() {
        let registry = MetricsRegistry::new();
        let metrics = SqlxQueryMetrics::new(&registry);
        metrics.record("SELECT 1", 0.01, false);
        metrics.record("SELECT 2", 0.02, false);
        metrics.record("UPDATE t SET x = 1", 0.03, true);
        assert_eq!(metrics.count.value_with(&["SELECT"]), 2.0);
        assert_eq!(metrics.count.value_with(&["UPDATE"]), 1.0);
        assert_eq!(metrics.errors.value_with(&["UPDATE"]), 1.0);
        assert_eq!(metrics.errors.value_with(&["SELECT"]), 0.0);
        assert_eq!(metrics.duration.count_with(&["SELECT"]), 2);
    }

    #[tokio::test]
    async fn timed_records_and_returns_result() {
        let registry = MetricsRegistry::new();
        let metrics = SqlxQueryMetrics::new(&registry);
        let r: Result<i32, &str> = metrics
            .timed("SELECT 1", || async { Ok::<i32, &str>(7) })
            .await;
        assert_eq!(r, Ok(7));
        assert_eq!(metrics.count.value_with(&["SELECT"]), 1.0);
        assert_eq!(metrics.errors.value_with(&["SELECT"]), 0.0);

        let e: Result<i32, &str> = metrics
            .timed("DELETE FROM t", || async { Err::<i32, &str>("boom") })
            .await;
        assert_eq!(e, Err("boom"));
        assert_eq!(metrics.errors.value_with(&["DELETE"]), 1.0);
    }
}
