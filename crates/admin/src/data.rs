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

//! JSON data shaping for the `/admin/api/*` endpoints and SSE streams.
//!
//! These free functions read the collaborators in [`AdminDeps`](crate::AdminDeps)
//! and render the exact wire shapes pyfly's providers produce. They are pure
//! data shapers (no HTTP), shared by both the route handlers and the live SSE
//! streams so a value and its push stream never drift.

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use firefly_actuator::{HealthComposite, HealthStatus, MetricRegistry};
use firefly_cqrs::Bus;
use firefly_orchestration::{ExecutionPattern, OrchestrationRegistry};
use firefly_scheduling::Scheduler;
use serde_json::{json, Value};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::deps::AdminDeps;

/// Process start instant, captured at first reference, for uptime reporting.
static START: OnceLock<Instant> = OnceLock::new();

/// Seconds elapsed since the process first reported uptime.
fn uptime_seconds() -> f64 {
    let started = START.get_or_init(Instant::now);
    (started.elapsed().as_secs_f64() * 10.0).round() / 10.0
}

/// Renders the `/admin/api/health` body — overall status plus per-component
/// detail — matching the actuator's `{status, details}` shape, with a
/// `components` alias for SPA convenience (pyfly's `HealthProvider`).
pub async fn health(health: &HealthComposite) -> (Value, bool) {
    let (overall, details) = health.check_all().await;
    let mut components = serde_json::Map::new();
    for (name, result) in &details {
        components.insert(
            name.clone(),
            serde_json::to_value(result).unwrap_or_else(|_| json!({ "status": result.status })),
        );
    }
    let body = json!({
        "status": overall,
        "details": components,
        "components": components,
    });
    (body, overall == HealthStatus::Down)
}

/// Renders the `/admin/api/overview` body — app info + health summary
/// (pyfly's `OverviewProvider`, minus the DI-container bean stats Rust has no
/// analogue for).
pub async fn overview(deps: &AdminDeps) -> Value {
    let (health_body, _down) = health(&deps.health).await;
    json!({
        "app": {
            "name": deps.app_name,
            "version": deps.app_version,
            "framework_version": crate::VERSION,
            "uptime_seconds": uptime_seconds(),
            "rust_version": option_env!("CARGO_PKG_RUST_VERSION").unwrap_or(""),
            "platform": std::env::consts::OS,
        },
        "health": health_body,
        "views": {
            "total": deps.views.len(),
        },
    })
}

/// Renders the `/admin/api/metrics` body — the meter-name listing
/// (pyfly's `MetricsProvider.get_metric_names`).
pub fn metric_names(registry: &MetricRegistry) -> Value {
    let names = registry.meter_names();
    json!({
        "names": names,
        "available": true,
        "has_prometheus": true,
    })
}

/// Renders the live metric-values snapshot streamed over SSE (pyfly's
/// `MetricsProvider.get_metric_values`): each meter name mapped to its
/// summed numeric value.
pub fn metric_values(registry: &MetricRegistry) -> Value {
    let names = registry.meter_names();
    let mut values = serde_json::Map::new();
    for name in &names {
        if let Some(detail) = registry.meter_json(name, None) {
            // Sum every measurement value into a single trend point.
            let total: f64 = detail
                .get("measurements")
                .and_then(Value::as_array)
                .map(|ms| {
                    ms.iter()
                        .filter_map(|m| m.get("value").and_then(Value::as_f64))
                        .sum()
                })
                .unwrap_or(0.0);
            values.insert(name.clone(), json!(total));
        }
    }
    json!({
        "names": names,
        "values": values,
        "available": true,
        "has_prometheus": true,
    })
}

/// Renders the `/admin/api/metrics/{name}` detail body (pyfly's
/// `MetricsProvider.get_metric_detail`), or a `not available` stub.
pub fn metric_detail(registry: &MetricRegistry, name: &str) -> Value {
    match registry.meter_json(name, None) {
        Some(detail) => detail,
        None => json!({ "name": name, "measurements": [], "available": false }),
    }
}

/// Renders the `/admin/api/scheduled` body (pyfly's `ScheduledProvider`).
/// A missing scheduler yields an empty list.
pub fn scheduled(scheduler: Option<&Arc<Scheduler>>) -> Value {
    let tasks: Vec<Value> = scheduler
        .map(|s| {
            s.tasks()
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
                .collect()
        })
        .unwrap_or_default();
    let total = tasks.len();
    json!({ "tasks": tasks, "total": total })
}

/// Renders the `/admin/api/cqrs` body (pyfly's `CqrsProvider`). A missing bus
/// yields an empty handler list. The Rust bus has a single typed dispatch
/// path, so every handler is reported with `kind = "message"`.
pub fn cqrs(bus: Option<&Arc<Bus>>) -> Value {
    let handlers: Vec<Value> = bus
        .map(|b| {
            b.handler_names()
                .into_iter()
                .map(|name| {
                    json!({
                        "message_type": name,
                        "message_name": name.rsplit("::").next().unwrap_or(name),
                        "kind": "message",
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let total = handlers.len();
    json!({
        "handlers": handlers,
        "total": total,
        "pipeline": { "command_bus": bus.is_some(), "query_bus": bus.is_some() },
    })
}

/// Renders the `/admin/api/transactions` body (pyfly's `TransactionsProvider`)
/// — saga / TCC definition listing from the orchestration registry. Workflows
/// (the Rust DAG engine) are surfaced under `workflows`.
pub fn transactions(registry: Option<&Arc<OrchestrationRegistry>>) -> Value {
    let Some(registry) = registry else {
        return json!({
            "sagas": [], "tcc": [], "workflows": [],
            "saga_count": 0, "tcc_count": 0, "total": 0, "in_flight": 0,
        });
    };
    let mut sagas = Vec::new();
    let mut tccs = Vec::new();
    let mut workflows = Vec::new();
    for def in registry.definitions() {
        let row = json!({
            "name": def.name,
            "steps": def.steps,
            "step_count": def.steps.len(),
        });
        match def.pattern {
            ExecutionPattern::Saga => sagas.push(row),
            ExecutionPattern::Tcc => tccs.push(row),
            ExecutionPattern::Workflow => workflows.push(row),
        }
    }
    let saga_count = sagas.len();
    let tcc_count = tccs.len();
    let total = saga_count + tcc_count + workflows.len();
    json!({
        "sagas": sagas,
        "tcc": tccs,
        "workflows": workflows,
        "saga_count": saga_count,
        "tcc_count": tcc_count,
        "total": total,
        "in_flight": 0,
    })
}

/// Renders the `/admin/api/caches` body (pyfly's `CacheProvider.get_caches`).
/// A missing cache yields `{"available": false}`.
pub fn caches(deps: &AdminDeps) -> Value {
    match &deps.cache {
        None => json!({ "available": false, "type": Value::Null, "caches": [], "keys": [] }),
        Some(cache) => {
            let descriptors: Vec<Value> = cache
                .caches()
                .iter()
                .map(|c| json!({ "name": c.name, "target": c.target }))
                .collect();
            json!({
                "available": true,
                "caches": descriptors,
                "keys": [],
            })
        }
    }
}

/// Renders the `/admin/api/runtime` body — tokio worker / task counts and
/// process RSS (pyfly's `RuntimeProvider`, adapted from Python GC to the
/// tokio runtime + sysinfo).
pub fn runtime() -> Value {
    let (workers, alive_tasks) = tokio_runtime_metrics();
    let memory = process_memory();
    json!({
        "timestamp": chrono::Utc::now().timestamp_millis(),
        "memory": memory,
        "tokio": {
            "worker_threads": workers,
            "alive_tasks": alive_tasks,
        },
        "cpu": {
            "logical_cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "rust": {
            "version": option_env!("CARGO_PKG_RUST_VERSION").unwrap_or(""),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
    })
}

/// Renders the `/admin/api/server` body — bind / worker info, replacing
/// pyfly's ASGI-specific `ServerProvider`.
pub fn server() -> Value {
    let (workers, _) = tokio_runtime_metrics();
    json!({
        "name": "axum",
        "runtime": "tokio",
        "workers": workers,
        "platform": {
            "system": std::env::consts::OS,
            "machine": std::env::consts::ARCH,
            "cpu_count": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        },
        "timestamp": chrono::Utc::now().timestamp_millis(),
    })
}

/// Renders the `/admin/api/settings` body (pyfly's `_handle_settings`).
pub fn settings(deps: &AdminDeps, cfg: &crate::AdminConfig) -> Value {
    json!({
        "title": cfg.title,
        "theme": cfg.theme,
        "refreshInterval": cfg.refresh_interval,
        "serverMode": deps.instances.is_some(),
    })
}

/// The tokio worker-thread count and alive-task count, when invoked inside a
/// tokio runtime (zeros otherwise).
fn tokio_runtime_metrics() -> (usize, usize) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let metrics = handle.metrics();
            (metrics.num_workers(), metrics.num_alive_tasks())
        }
        Err(_) => (0, 0),
    }
}

/// The current process's resident / virtual memory in MiB via sysinfo.
fn process_memory() -> Value {
    let Ok(pid) = sysinfo::get_current_pid() else {
        return json!({ "rss_mb": 0.0, "vms_mb": 0.0 });
    };
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    match system.process(pid) {
        Some(proc) => json!({
            "rss_mb": (proc.memory() as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0,
            "vms_mb": (proc.virtual_memory() as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0,
        }),
        None => json!({ "rss_mb": 0.0, "vms_mb": 0.0 }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::LogBuffer;
    use crate::trace::TraceBuffer;
    use firefly_actuator::{HealthResult, IndicatorFn};

    fn minimal_deps() -> AdminDeps {
        AdminDeps::new(
            "orders",
            "1.0.0",
            Arc::new(HealthComposite::new()),
            Arc::new(MetricRegistry::new()),
            Arc::new(TraceBuffer::new()),
            LogBuffer::new(),
        )
    }

    #[tokio::test]
    async fn health_reports_up_for_empty_composite() {
        let composite = HealthComposite::new();
        composite.add(IndicatorFn::new("db", || async { HealthResult::up() }));
        let (body, down) = health(&composite).await;
        assert_eq!(body["status"], "UP");
        assert_eq!(body["components"]["db"]["status"], "UP");
        assert!(!down);
    }

    #[tokio::test]
    async fn health_marks_down() {
        let composite = HealthComposite::new();
        composite.add(IndicatorFn::new("db", || async {
            HealthResult::down("boom")
        }));
        let (body, down) = health(&composite).await;
        assert_eq!(body["status"], "DOWN");
        assert!(down);
    }

    #[test]
    fn metric_names_lists_registered() {
        let registry = MetricRegistry::new();
        registry.counter("orders_total").inc();
        registry.gauge("queue_depth").set(3.0);
        let body = metric_names(&registry);
        let names = body["names"].as_array().unwrap();
        assert!(names.iter().any(|n| n == "orders_total"));
        assert!(names.iter().any(|n| n == "queue_depth"));
    }

    #[test]
    fn metric_values_snapshots_numbers() {
        let registry = MetricRegistry::new();
        registry.counter("orders_total").add(5);
        let body = metric_values(&registry);
        assert_eq!(body["values"]["orders_total"], 5.0);
    }

    #[test]
    fn scheduled_empty_without_scheduler() {
        let body = scheduled(None);
        assert_eq!(body["total"], 0);
        assert!(body["tasks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn cqrs_empty_without_bus() {
        let body = cqrs(None);
        assert_eq!(body["total"], 0);
        assert_eq!(body["pipeline"]["command_bus"], false);
    }

    #[test]
    fn transactions_empty_without_registry() {
        let body = transactions(None);
        assert_eq!(body["total"], 0);
        assert_eq!(body["saga_count"], 0);
    }

    #[test]
    fn caches_unavailable_without_cache() {
        let deps = minimal_deps();
        let body = caches(&deps);
        assert_eq!(body["available"], false);
    }

    #[test]
    fn settings_reports_server_mode_false() {
        let deps = minimal_deps();
        let body = settings(&deps, &crate::AdminConfig::default());
        assert_eq!(body["serverMode"], false);
        assert_eq!(body["title"], "Firefly Admin");
    }

    #[tokio::test]
    async fn runtime_reports_tokio_workers() {
        let body = runtime();
        assert!(body["tokio"]["worker_threads"].as_u64().unwrap() >= 1);
        assert!(body["memory"]["rss_mb"].is_number());
    }

    #[tokio::test]
    async fn overview_carries_app_block() {
        let deps = minimal_deps();
        let body = overview(&deps).await;
        assert_eq!(body["app"]["name"], "orders");
        assert_eq!(body["app"]["version"], "1.0.0");
        assert_eq!(body["app"]["framework_version"], crate::VERSION);
    }
}
