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
use firefly_container::{BeanDescriptor, Container};
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

/// Renders the `/admin/api/overview` body — app info + health summary plus the
/// `beans` (`{total, stereotypes}`) and `wiring` blocks (pyfly's
/// `OverviewProvider`).
///
/// When a DI [`Container`](firefly_container::Container) is wired into
/// [`AdminDeps::container`](crate::AdminDeps::container), the bean total and
/// per-stereotype counts are read from [`Container::bean_stats`]; otherwise
/// both default to zero/empty (the SPA donut + stat card then render `0`
/// without erroring). The `wiring` block reports the live collaborator counts
/// the Rust port can observe — `cqrs_handlers` from the wired
/// [`Bus`](firefly_cqrs::Bus) and `scheduled` from the wired
/// [`Scheduler`](firefly_scheduling::Scheduler) — with the remaining pyfly keys
/// (`event_listeners`, `message_listeners`, `async_methods`, `post_processors`)
/// reported as `0`.
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
        "beans": bean_stats(deps.container.as_ref()),
        "wiring": wiring_counts(deps),
        "views": {
            "total": deps.views.len(),
        },
    })
}

/// Renders the overview `beans` block — `{total, stereotypes}` — from the wired
/// container's [`bean_stats`](firefly_container::Container::bean_stats), or
/// `{total: 0, stereotypes: {}}` when no container is wired (pyfly's
/// `OverviewProvider` bean block).
fn bean_stats(container: Option<&Arc<Container>>) -> Value {
    match container {
        None => json!({ "total": 0, "stereotypes": {} }),
        Some(container) => {
            let stats = container.bean_stats();
            json!({ "total": stats.total, "stereotypes": stats.stereotypes })
        }
    }
}

/// Renders the overview `wiring` block — the live collaborator counts the Rust
/// port can observe (pyfly's `wiring_counts`). `cqrs_handlers` and `scheduled`
/// reflect the wired [`Bus`] / [`Scheduler`]; the listener/post-processor keys
/// the Rust runtime has no central registry for are reported as `0` so the SPA
/// wiring bars render real numbers where available and zeros elsewhere.
fn wiring_counts(deps: &AdminDeps) -> Value {
    let cqrs_handlers = deps
        .bus
        .as_ref()
        .map(|b| b.handler_names().len())
        .unwrap_or(0);
    let scheduled = deps
        .scheduler
        .as_ref()
        .map(|s| s.tasks().len())
        .unwrap_or(0);
    json!({
        "event_listeners": 0,
        "message_listeners": 0,
        "cqrs_handlers": cqrs_handlers,
        "scheduled": scheduled,
        "async_methods": 0,
        "post_processors": 0,
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

/// Maps one [`BeanDescriptor`] to the JSON row shape pyfly's
/// `BeansProvider.get_beans` produces.
///
/// The Rust container exposes name/type/scope/stereotype/primary plus
/// initialized + resolution-count; the type-hint-derived fields pyfly fills by
/// runtime reflection (`dependencies`, `conditions`, `category`, `order`,
/// `profile`, `creation_time_ms`, `created_at`) have no zero-cost Rust analogue
/// and are reported with empty/`null` defaults so the SPA renders without
/// erroring. `category` falls back to `"component"` when no stereotype label is
/// recorded, matching pyfly's `_infer_category` default.
fn bean_to_json(bean: &BeanDescriptor) -> Value {
    let stereotype = bean
        .stereotype
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let category = bean
        .stereotype
        .clone()
        .unwrap_or_else(|| "component".to_string());
    json!({
        "name": bean.name,
        "type": bean.type_name,
        "scope": bean.scope,
        "stereotype": stereotype,
        "category": category,
        "primary": bean.primary,
        "order": Value::Null,
        "profile": Value::Null,
        "conditions": [],
        "dependencies": [],
        "initialized": bean.initialized,
        "creation_time_ms": Value::Null,
        "resolution_count": bean.resolution_count,
        "created_at": Value::Null,
    })
}

/// Renders the `/admin/api/beans` body (pyfly's `BeansProvider.get_beans`) —
/// `{beans: [...], total: N}`, every registered bean sorted by
/// `(stereotype, name)`. A missing container yields an empty listing.
pub fn beans(container: Option<&Arc<Container>>) -> Value {
    let mut rows: Vec<(String, String, Value)> = container
        .map(|c| {
            c.beans()
                .iter()
                .map(|b| {
                    let stereotype = b.stereotype.clone().unwrap_or_else(|| "none".to_string());
                    (stereotype, b.name.clone(), bean_to_json(b))
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let beans: Vec<Value> = rows.into_iter().map(|(_, _, v)| v).collect();
    let total = beans.len();
    json!({ "beans": beans, "total": total })
}

/// Renders the `/admin/api/beans/{name}` body (pyfly's
/// `BeansProvider.get_bean_detail`) — the bean row enriched with the
/// detail-only fields the SPA's detail panel reads, or `None` when no bean
/// matches `name` (the route returns `404`).
///
/// `module` is derived from the Rust type path (everything before the final
/// `::`); reflection-only fields (`file`/`doc`/`dependency_chain`/
/// `post_construct`/`pre_destroy`/`autowired_fields`/`bean_method_origin`)
/// carry empty/`null` defaults.
pub fn bean_detail(container: Option<&Arc<Container>>, name: &str) -> Option<Value> {
    let container = container?;
    let bean = container.beans().into_iter().find(|b| b.name == name)?;
    let module = bean
        .type_name
        .rsplit_once("::")
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_default();
    let mut detail = bean_to_json(&bean);
    let obj = detail.as_object_mut().expect("bean row is an object");
    obj.insert("module".into(), json!(module));
    obj.insert("file".into(), Value::Null);
    obj.insert("doc".into(), json!(""));
    obj.insert("dependency_chain".into(), json!([]));
    obj.insert("bean_method_origin".into(), Value::Null);
    obj.insert("post_construct".into(), json!([]));
    obj.insert("pre_destroy".into(), json!([]));
    obj.insert("autowired_fields".into(), json!([]));
    Some(detail)
}

/// Renders the `/admin/api/beans/graph` body (pyfly's
/// `BeansProvider.get_bean_graph`) — `{nodes, edges}`. Each registered bean
/// becomes a node; edges (constructor/autowired dependencies) require
/// type-hint reflection the Rust container does not retain, so the edge list is
/// empty (best-effort nodes-only graph, per the gap report). A missing
/// container yields an empty graph.
pub fn bean_graph(container: Option<&Arc<Container>>) -> Value {
    let nodes: Vec<Value> = container
        .map(|c| {
            c.beans()
                .iter()
                .map(|b| {
                    let stereotype = b.stereotype.clone().unwrap_or_else(|| "none".to_string());
                    let category = b
                        .stereotype
                        .clone()
                        .unwrap_or_else(|| "component".to_string());
                    json!({
                        "id": b.name,
                        "name": b.name,
                        "type": b.type_name,
                        "stereotype": stereotype,
                        "category": category,
                        "scope": b.scope,
                        "initialized": b.initialized,
                        "order": Value::Null,
                        "resolution_count": b.resolution_count,
                        "creation_time_ms": Value::Null,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "nodes": nodes, "edges": [] })
}

/// A snapshot of every bean's current `resolution_count`, keyed by bean name —
/// the per-tick sample the `/admin/api/sse/beans` stream diffs against its last
/// snapshot (pyfly's `beans_stream` `last_counts`). An absent container yields
/// an empty map.
pub fn bean_resolution_counts(
    container: Option<&Arc<Container>>,
) -> std::collections::BTreeMap<String, u64> {
    container
        .map(|c| {
            c.beans()
                .into_iter()
                .map(|b| (b.name, b.resolution_count))
                .collect()
        })
        .unwrap_or_default()
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
    fn beans_empty_without_container() {
        let body = beans(None);
        assert_eq!(body["total"], 0);
        assert!(body["beans"].as_array().unwrap().is_empty());
    }

    #[test]
    fn bean_detail_none_without_container() {
        assert!(bean_detail(None, "anything").is_none());
    }

    #[test]
    fn bean_graph_empty_without_container() {
        let body = bean_graph(None);
        assert!(body["nodes"].as_array().unwrap().is_empty());
        assert!(body["edges"].as_array().unwrap().is_empty());
    }

    #[test]
    fn bean_resolution_counts_empty_without_container() {
        assert!(bean_resolution_counts(None).is_empty());
    }

    #[test]
    fn beans_lists_registered_container_beans() {
        let container = std::sync::Arc::new(firefly_container::Container::new());
        container.register_instance(42_i32);
        container.register_instance("hello".to_string());
        // Force a resolution so the count is observable.
        let _ = container.resolve::<i32>();

        let body = beans(Some(&container));
        assert_eq!(body["total"], 2);
        let rows = body["beans"].as_array().unwrap();
        // Sorted by (stereotype, name); both are hand-registered (stereotype
        // "none"), so the listing is alphabetical by type name.
        assert!(rows.iter().any(|r| r["type"] == "i32"));
        assert!(rows.iter().any(|r| r["type"] == "alloc::string::String"));
        let int_row = rows.iter().find(|r| r["type"] == "i32").unwrap();
        assert_eq!(int_row["stereotype"], "none");
        assert_eq!(int_row["category"], "component");
        assert!(int_row["resolution_count"].as_u64().unwrap() >= 1);
        assert_eq!(int_row["initialized"], true);
    }

    #[test]
    fn bean_detail_found_carries_module_and_detail_fields() {
        let container = std::sync::Arc::new(firefly_container::Container::new());
        container.register_instance("hello".to_string());
        let detail = bean_detail(Some(&container), "alloc::string::String").unwrap();
        assert_eq!(detail["type"], "alloc::string::String");
        assert_eq!(detail["module"], "alloc::string");
        assert!(detail["dependency_chain"].is_array());
        assert!(detail["post_construct"].is_array());
        assert!(detail["autowired_fields"].is_array());
    }

    #[test]
    fn bean_graph_renders_nodes_only() {
        let container = std::sync::Arc::new(firefly_container::Container::new());
        container.register_instance(7_i32);
        let body = bean_graph(Some(&container));
        assert_eq!(body["nodes"].as_array().unwrap().len(), 1);
        assert!(body["edges"].as_array().unwrap().is_empty());
        assert_eq!(body["nodes"][0]["id"], "i32");
    }

    #[tokio::test]
    async fn overview_carries_beans_and_wiring_blocks() {
        let deps = minimal_deps();
        let body = overview(&deps).await;
        // No container wired ⇒ zero beans, empty stereotypes.
        assert_eq!(body["beans"]["total"], 0);
        assert!(body["beans"]["stereotypes"].is_object());
        // Wiring block is always present with the pyfly keys.
        assert_eq!(body["wiring"]["cqrs_handlers"], 0);
        assert_eq!(body["wiring"]["scheduled"], 0);
        assert_eq!(body["wiring"]["event_listeners"], 0);
    }

    #[tokio::test]
    async fn overview_beans_total_from_container() {
        let container = std::sync::Arc::new(firefly_container::Container::new());
        container.register_instance(1_u8);
        let deps = AdminDeps {
            container: Some(container),
            ..minimal_deps()
        };
        let body = overview(&deps).await;
        assert_eq!(body["beans"]["total"], 1);
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
