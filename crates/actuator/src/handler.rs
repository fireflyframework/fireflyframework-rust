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

//! The `/actuator/*` HTTP surface: [`ActuatorConfig`] plus [`mount`],
//! which returns an axum [`Router`] exposing health (with probe groups
//! and drill-down), info, metrics (Prometheus text + Micrometer JSON),
//! prometheus, env, tasks, version, loggers, scheduledtasks, caches,
//! refresh, httpexchanges, and custom [`Endpoint`]s — honoring the
//! Spring-style [`ExposureConfig`] include/exclude/base-path model.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{SecondsFormat, Utc};
use http::{header, HeaderMap, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};

use crate::caches::{CacheOps, CACHE_MANAGER};
use crate::endpoint::EndpointRegistry;
use crate::env_source::{EnvSource, PropertySourceView};
use crate::exposure::ExposureConfig;
use crate::health::{HealthComposite, HealthResult, HealthStatus};
use crate::http_exchanges::HttpExchangeRecorder;
use crate::loggers::{LoggersError, LoggersState};
use crate::metrics::MetricRegistry;
use crate::refresh::Refresher;
use crate::scheduledtasks::{render_tasks, ScheduledTasksSource};
use crate::threaddump::thread_dump;
use crate::VERSION;

/// A function contributing extra top-level entries to `/actuator/info` —
/// the counterpart of Go's `func() map[string]any` contributors.
pub type InfoContributor = Box<dyn Fn() -> serde_json::Map<String, Value> + Send + Sync>;

/// Endpoint ids handled by [`mount`] itself; custom endpoints with these
/// ids are skipped to avoid route collisions.
const BUILTIN_IDS: [&str; 13] = [
    "health",
    "info",
    "metrics",
    "prometheus",
    "env",
    "tasks",
    "threaddump",
    "version",
    "loggers",
    "scheduledtasks",
    "caches",
    "refresh",
    "httpexchanges",
];

/// Tunes [`mount`].
pub struct ActuatorConfig {
    /// Reflected on `/actuator/info` and `/actuator/version`.
    pub app_name: String,

    /// Reflected on `/actuator/info` and `/actuator/version`; defaults
    /// to the framework [`VERSION`] when empty.
    pub app_version: String,

    /// Health composite checked on `/actuator/health`; defaults to an
    /// empty composite (overall status `UP`).
    pub health: Arc<HealthComposite>,

    /// Contributors enriching `/actuator/info` with arbitrary
    /// structured data, merged into the top-level object.
    pub info_contributors: Vec<InfoContributor>,

    /// Prefix list of env var names whose values are returned by
    /// `/actuator/env` (everything else is redacted to `"***"`).
    /// Defaults to `["FIREFLY_"]`. Matching is case-insensitive. Only
    /// consulted when [`env_source`](Self::env_source) is `None`.
    pub env_allow_prefixes: Vec<String>,

    /// When set, `/actuator/env` returns the Spring-style
    /// `{activeProfiles, propertySources}` view (with masked values and a
    /// per-property `/actuator/env/{toMatch}` drill-down) sourced from the
    /// application's configuration layer — pyfly's `EnvEndpoint`. When `None`
    /// (the default), `/actuator/env` keeps the legacy flat redacted
    /// process-environment map. Wire this in a starter over
    /// `firefly-config`'s `Layered::property_sources` to keep the actuator
    /// crate decoupled from any concrete config crate.
    pub env_source: Option<Arc<dyn EnvSource>>,

    /// Registry consulted by `/actuator/metrics`, `/actuator/metrics/{name}`,
    /// and `/actuator/prometheus`; defaults to an empty registry.
    pub metric_registry: Arc<MetricRegistry>,

    /// Spring-style web exposure: include/exclude id sets, base path,
    /// per-endpoint enabled overrides. The default exposes everything
    /// under `/actuator` (Go-parity backward compatibility); use
    /// [`ExposureConfig::spring_default`] for Spring's `health,info`.
    pub exposure: ExposureConfig,

    /// Custom [`Endpoint`](crate::Endpoint)s mounted at
    /// `{base_path}/{id}` — pyfly's `ActuatorRegistry`.
    pub endpoints: Arc<EndpointRegistry>,

    /// When set, mounts `GET/POST {base_path}/loggers[/{name}]` over the
    /// wrapped `tracing_subscriber` reload handle.
    pub loggers: Option<Arc<LoggersState>>,

    /// When set, mounts `GET {base_path}/scheduledtasks`.
    pub scheduled_tasks: Option<Arc<dyn ScheduledTasksSource>>,

    /// When set, mounts `GET {base_path}/caches[/{name}]` and
    /// `POST {base_path}/caches/{name}/evict`.
    pub cache_ops: Option<Arc<dyn CacheOps>>,

    /// When set, mounts `POST {base_path}/refresh`.
    pub refresher: Option<Arc<dyn Refresher>>,

    /// When set, mounts `GET {base_path}/httpexchanges` over the shared
    /// recorder (populate it by applying
    /// [`HttpExchangesLayer`](crate::HttpExchangesLayer) to the
    /// application router).
    pub http_exchanges: Option<Arc<HttpExchangeRecorder>>,

    /// Spring's `management.endpoint.health.show-details`: when false,
    /// per-component entries carry only `{"status": …}`. Default true.
    pub show_details: bool,

    /// Spring's `management.endpoint.health.show-components`: when
    /// false, health bodies omit the per-component `details` map
    /// entirely. Default true.
    pub show_components: bool,
}

impl Default for ActuatorConfig {
    fn default() -> Self {
        Self {
            app_name: String::new(),
            app_version: String::new(),
            health: Arc::default(),
            info_contributors: Vec::new(),
            env_allow_prefixes: Vec::new(),
            env_source: None,
            metric_registry: Arc::default(),
            exposure: ExposureConfig::default(),
            endpoints: Arc::default(),
            loggers: None,
            scheduled_tasks: None,
            cache_ops: None,
            refresher: None,
            http_exchanges: None,
            show_details: true,
            show_components: true,
        }
    }
}

/// Shared per-router state.
struct ActuatorState {
    app_name: String,
    app_version: String,
    health: Arc<HealthComposite>,
    info_contributors: Vec<InfoContributor>,
    env_allow_prefixes: Vec<String>,
    env_source: Option<Arc<dyn EnvSource>>,
    metric_registry: Arc<MetricRegistry>,
    loggers: Option<Arc<LoggersState>>,
    scheduled_tasks: Option<Arc<dyn ScheduledTasksSource>>,
    cache_ops: Option<Arc<dyn CacheOps>>,
    refresher: Option<Arc<dyn Refresher>>,
    http_exchanges: Option<Arc<HttpExchangeRecorder>>,
    show_details: bool,
    show_components: bool,
}

type SharedState = Arc<ActuatorState>;

/// Returns an axum [`Router`] exposing the actuator surface under the
/// given config. Merge it into the router used for the application's
/// public traffic, or — preferred — serve it from a dedicated admin port
/// so the management surface never leaks onto the public network.
///
/// Routes (under `exposure.base_path`, default `/actuator`; each
/// endpoint id is mounted only when exposed by [`ExposureConfig`] and
/// not disabled via `endpoint_enabled`):
///
/// - `GET {bp}` — `_links` index of the exposed endpoints
/// - `GET {bp}/health` — composite status; 200 UP/DEGRADED, 503 DOWN
/// - `GET {bp}/health/{liveness|readiness|group|component}` — drill-down
/// - `GET {bp}/info` — app + runtime + build info, merged contributors
/// - `GET {bp}/metrics` — Prometheus exposition format (with
///   `Accept: application/json`: the Micrometer `{"names": […]}` list)
/// - `GET {bp}/metrics/{name}?tag=k:v` — Micrometer JSON meter detail
/// - `GET {bp}/prometheus` — Prometheus exposition format (labeled)
/// - `GET {bp}/env` — Spring `{activeProfiles, propertySources}` when an
///   [`EnvSource`] is wired (masked, origin-attributed); otherwise the legacy
///   flat redacted environment map
/// - `GET {bp}/env/{toMatch}` — one property's value across the ordered
///   sources (only mounted when an [`EnvSource`] is wired)
/// - `GET {bp}/tasks` — `{"count": N}` alive tokio tasks; `?dump=true`
///   returns a plain-text runtime report (the async analog of Go's
///   `/actuator/goroutines`)
/// - `GET {bp}/threaddump` — Spring `{threads:[…]}`; the tokio runtime
///   worker/task snapshot (the async-Rust analog of a JVM thread dump)
/// - `GET {bp}/version` — framework / app / language version stamp
/// - `GET {bp}/loggers`, `GET/POST {bp}/loggers/{name}` — runtime log
///   levels (when `loggers` is wired)
/// - `GET {bp}/scheduledtasks` (when `scheduled_tasks` is wired)
/// - `GET {bp}/caches`, `GET {bp}/caches/{name}`,
///   `POST {bp}/caches/{name}/evict` (when `cache_ops` is wired)
/// - `POST {bp}/refresh` (when `refresher` is wired)
/// - `GET {bp}/httpexchanges` (when `http_exchanges` is wired)
/// - `GET {bp}/{id}[/{selector}]` for each custom registered
///   [`Endpoint`](crate::Endpoint)
pub fn mount(cfg: ActuatorConfig) -> Router {
    let app_version = if cfg.app_version.is_empty() {
        VERSION.to_string()
    } else {
        cfg.app_version
    };
    let env_allow_prefixes = if cfg.env_allow_prefixes.is_empty() {
        vec!["FIREFLY_".to_string()]
    } else {
        cfg.env_allow_prefixes
    };

    let exposure = cfg.exposure;
    let bp = exposure.normalized_base_path();
    let expose = |id: &str, default_enabled: bool| {
        exposure.is_exposed(id) && exposure.is_enabled(id, default_enabled)
    };

    let state: SharedState = Arc::new(ActuatorState {
        app_name: cfg.app_name,
        app_version,
        health: cfg.health,
        info_contributors: cfg.info_contributors,
        env_allow_prefixes,
        env_source: cfg.env_source,
        metric_registry: cfg.metric_registry,
        loggers: cfg.loggers,
        scheduled_tasks: cfg.scheduled_tasks,
        cache_ops: cfg.cache_ops,
        refresher: cfg.refresher,
        http_exchanges: cfg.http_exchanges,
        show_details: cfg.show_details,
        show_components: cfg.show_components,
    });

    let mut exposed_ids: Vec<String> = Vec::new();
    let mut router = Router::new();

    if expose("health", true) {
        exposed_ids.push("health".into());
        router = router
            .route(&format!("{bp}/health"), get(health_handler))
            .route(
                &format!("{bp}/health/:selector"),
                get(health_selector_handler),
            );
    }
    if expose("info", true) {
        exposed_ids.push("info".into());
        router = router.route(&format!("{bp}/info"), get(info_handler));
    }
    if expose("metrics", true) {
        exposed_ids.push("metrics".into());
        router = router
            .route(&format!("{bp}/metrics"), get(metrics_handler))
            .route(&format!("{bp}/metrics/:name"), get(metric_detail_handler));
    }
    if expose("prometheus", true) {
        exposed_ids.push("prometheus".into());
        router = router.route(&format!("{bp}/prometheus"), get(prometheus_handler));
    }
    if expose("env", true) {
        exposed_ids.push("env".into());
        router = router.route(&format!("{bp}/env"), get(env_handler));
        // Spring's per-property drill-down — only meaningful (and only
        // mounted) when a property-source view is wired.
        if state.env_source.is_some() {
            router = router.route(&format!("{bp}/env/:selector"), get(env_selector_handler));
        }
    }
    if expose("tasks", true) {
        exposed_ids.push("tasks".into());
        router = router.route(&format!("{bp}/tasks"), get(tasks_handler));
    }
    if expose("threaddump", true) {
        exposed_ids.push("threaddump".into());
        router = router.route(&format!("{bp}/threaddump"), get(threaddump_handler));
    }
    if expose("version", true) {
        exposed_ids.push("version".into());
        router = router.route(&format!("{bp}/version"), get(version_handler));
    }
    if state.loggers.is_some() && expose("loggers", true) {
        exposed_ids.push("loggers".into());
        router = router
            .route(&format!("{bp}/loggers"), get(loggers_handler))
            .route(
                &format!("{bp}/loggers/:name"),
                get(logger_get_handler).post(logger_post_handler),
            );
    }
    if state.scheduled_tasks.is_some() && expose("scheduledtasks", true) {
        exposed_ids.push("scheduledtasks".into());
        router = router.route(&format!("{bp}/scheduledtasks"), get(scheduledtasks_handler));
    }
    if state.cache_ops.is_some() && expose("caches", true) {
        exposed_ids.push("caches".into());
        router = router
            .route(&format!("{bp}/caches"), get(caches_handler))
            .route(&format!("{bp}/caches/:name"), get(cache_detail_handler))
            .route(
                &format!("{bp}/caches/:name/evict"),
                post(cache_evict_handler),
            );
    }
    if state.refresher.is_some() && expose("refresh", true) {
        exposed_ids.push("refresh".into());
        router = router.route(&format!("{bp}/refresh"), post(refresh_handler));
    }
    if state.http_exchanges.is_some() && expose("httpexchanges", true) {
        exposed_ids.push("httpexchanges".into());
        router = router.route(&format!("{bp}/httpexchanges"), get(httpexchanges_handler));
    }

    // Auto-register the DI/route introspection endpoints (Spring's
    // beans/mappings/conditions); each is then served only if exposed below. A
    // user-registered endpoint with the same id is left untouched.
    crate::introspection::register_introspection(&cfg.endpoints);

    // Custom endpoints (pyfly's ActuatorRegistry surface).
    for ep in cfg.endpoints.all() {
        let id = ep.id().to_string();
        if BUILTIN_IDS.contains(&id.as_str()) {
            continue; // built-ins own these routes
        }
        if !exposure.is_exposed(&id) || !exposure.is_enabled(&id, ep.enabled()) {
            continue;
        }
        exposed_ids.push(id.clone());
        let base = format!("{bp}/{id}");

        let ep_base = Arc::clone(&ep);
        let id_base = id.clone();
        router = router.route(
            &base,
            get(move |Query(query): Query<HashMap<String, String>>| {
                let ep = Arc::clone(&ep_base);
                let id = id_base.clone();
                async move {
                    match ep.handle(None, &query).await {
                        Some(body) => json_response(StatusCode::OK, &body),
                        None => json_response(
                            StatusCode::NOT_FOUND,
                            &json!({ "error": format!("No content for {id}") }),
                        ),
                    }
                }
            }),
        );

        if ep.supports_selector() {
            let ep_sel = Arc::clone(&ep);
            let id_sel = id.clone();
            router = router.route(
                &format!("{base}/:selector"),
                get(
                    move |Path(selector): Path<String>,
                          Query(query): Query<HashMap<String, String>>| {
                        let ep = Arc::clone(&ep_sel);
                        let id = id_sel.clone();
                        async move {
                            match ep.handle(Some(&selector), &query).await {
                                Some(body) => json_response(StatusCode::OK, &body),
                                None => json_response(
                                    StatusCode::NOT_FOUND,
                                    &json!({ "error": format!("No such {id}: {selector}") }),
                                ),
                            }
                        }
                    },
                ),
            );
        }
    }

    // Index: `GET {bp}` — `_links` of everything exposed (pyfly parity).
    let mut links = serde_json::Map::new();
    let self_href = if bp.is_empty() {
        "/".to_string()
    } else {
        bp.clone()
    };
    links.insert("self".into(), json!({ "href": self_href }));
    for id in &exposed_ids {
        links.insert(id.clone(), json!({ "href": format!("{bp}/{id}") }));
    }
    if exposed_ids.iter().any(|id| id == "health") {
        links.insert(
            "health/liveness".into(),
            json!({ "href": format!("{bp}/health/liveness") }),
        );
        links.insert(
            "health/readiness".into(),
            json!({ "href": format!("{bp}/health/readiness") }),
        );
    }
    let index_body = json!({ "_links": links });
    let index_path = if bp.is_empty() {
        "/".to_string()
    } else {
        bp.clone()
    };
    router = router.route(
        &index_path,
        get(move || {
            let body = index_body.clone();
            async move { json_response(StatusCode::OK, &body) }
        }),
    );

    router.with_state(state)
}

/// Renders `value` as a JSON response body terminated by a single `\n` —
/// the counterpart of Go's `writeJSON`, whose `json.NewEncoder(w).Encode(v)`
/// always appends a trailing newline. Emitting the same final byte keeps
/// the wire format identical across the ports.
fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(mut body) => {
            body.push(b'\n');
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// 503 only for DOWN — DEGRADED stays 200, matching every Firefly port.
fn health_status_code(status: HealthStatus) -> StatusCode {
    if status == HealthStatus::Down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

/// Builds the health body — `{"status": …, "details": {…}}` — honoring
/// Spring's `show-components` / `show-details` switches.
fn render_health_body(
    overall: HealthStatus,
    results: &BTreeMap<String, HealthResult>,
    show_components: bool,
    show_details: bool,
) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("status".into(), json!(overall));
    if show_components {
        let mut details = serde_json::Map::new();
        for (name, result) in results {
            details.insert(name.clone(), render_component(result, show_details));
        }
        body.insert("details".into(), Value::Object(details));
    }
    Value::Object(body)
}

/// One component's entry: the full [`HealthResult`] when `show_details`,
/// otherwise just `{"status": …}`.
fn render_component(result: &HealthResult, show_details: bool) -> Value {
    if show_details {
        serde_json::to_value(result).unwrap_or_else(|_| json!({ "status": result.status }))
    } else {
        json!({ "status": result.status })
    }
}

/// `GET /actuator/health` — runs every registered indicator and answers
/// `{"status": overall, "details": {name: result}}`; 503 when DOWN,
/// 200 otherwise.
async fn health_handler(State(st): State<SharedState>) -> Response {
    let (overall, details) = st.health.check_all().await;
    json_response(
        health_status_code(overall),
        &render_health_body(overall, &details, st.show_components, st.show_details),
    )
}

/// `GET /actuator/health/{selector}` — the `liveness` / `readiness`
/// probes, a named health group, or a single component drill-down;
/// 404 with an error body when the selector matches neither.
async fn health_selector_handler(
    State(st): State<SharedState>,
    Path(selector): Path<String>,
) -> Response {
    if let Some((overall, details)) = st.health.check_group(&selector).await {
        return json_response(
            health_status_code(overall),
            &render_health_body(overall, &details, st.show_components, st.show_details),
        );
    }
    if let Some(result) = st.health.check_component(&selector).await {
        return json_response(
            health_status_code(result.status),
            &render_component(&result, st.show_details),
        );
    }
    json_response(
        StatusCode::NOT_FOUND,
        &json!({ "error": format!("No such health component or group: {selector}") }),
    )
}

/// `GET /actuator/info` — build info + app metadata, with every
/// configured contributor merged into the top-level object.
async fn info_handler(State(st): State<SharedState>) -> Response {
    let mut info = serde_json::Map::new();
    info.insert(
        "app".into(),
        json!({ "name": st.app_name, "version": st.app_version }),
    );
    info.insert(
        "runtime".into(),
        json!({
            "rustVersion": env!("CARGO_PKG_RUST_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "numCPU": std::thread::available_parallelism().map(usize::from).unwrap_or(1),
        }),
    );
    info.insert(
        "build".into(),
        json!({
            "crate": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
        }),
    );
    for contributor in &st.info_contributors {
        for (k, v) in contributor() {
            info.insert(k, v);
        }
    }
    json_response(StatusCode::OK, &Value::Object(info))
}

/// `GET /actuator/metrics` — Prometheus exposition format by default;
/// with `Accept: application/json`, the Micrometer `{"names": […]}`
/// meter list (pyfly's `MetricsEndpoint._list`).
async fn metrics_handler(State(st): State<SharedState>, headers: HeaderMap) -> Response {
    let wants_json = headers.get_all(header::ACCEPT).iter().any(|v| {
        v.to_str()
            .map(|s| s.contains("application/json"))
            .unwrap_or(false)
    });
    if wants_json {
        return json_response(
            StatusCode::OK,
            &json!({ "names": st.metric_registry.meter_names() }),
        );
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        st.metric_registry.render(),
    )
        .into_response()
}

/// `GET /actuator/metrics/{name}?tag=k:v` — Micrometer JSON meter
/// detail with `measurements` + `availableTags`; 404 for unknown meters.
async fn metric_detail_handler(
    State(st): State<SharedState>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let tag = params.get("tag").and_then(|raw| raw.split_once(':'));
    match st.metric_registry.meter_json(&name, tag) {
        Some(body) => json_response(StatusCode::OK, &body),
        None => json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": format!("No such metric: {name}") }),
        ),
    }
}

/// `GET /actuator/prometheus` — the Prometheus scrape target, classic
/// text exposition format (`version=0.0.4`), labels included.
async fn prometheus_handler(State(st): State<SharedState>) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        st.metric_registry.render(),
    )
        .into_response()
}

/// `GET /actuator/env` — when an [`EnvSource`] is wired, the Spring-style
/// `{activeProfiles, propertySources:[{name, properties:{k:{value, origin}}}]}`
/// view with masked values (pyfly's `EnvEndpoint`); otherwise the legacy flat
/// process-environment map with values outside the allow-prefix list redacted
/// to `"***"`.
async fn env_handler(State(st): State<SharedState>) -> Response {
    if let Some(source) = &st.env_source {
        let body = json!({
            "activeProfiles": source.active_profiles(),
            "propertySources": source.property_sources(),
        });
        return json_response(StatusCode::OK, &body);
    }
    let mut out = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        let value = value.to_string_lossy().into_owned();
        let redacted = redact(&key, value, &st.env_allow_prefixes);
        out.insert(key, redacted);
    }
    json_response(StatusCode::OK, &out)
}

/// `GET /actuator/env/{toMatch}` — Spring's per-property drill-down: the
/// property's winning value (the first source that has it, highest precedence
/// first) plus its appearance in each source. Only mounted when an
/// [`EnvSource`] is wired. Mirrors pyfly's `EnvEndpoint._property_detail`.
async fn env_selector_handler(
    State(st): State<SharedState>,
    Path(selector): Path<String>,
) -> Response {
    let Some(source) = &st.env_source else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let profiles = source.active_profiles();
    let sources = source.property_sources();
    json_response(
        StatusCode::OK,
        &render_property_detail(&selector, profiles, &sources),
    )
}

/// Builds the `/actuator/env/{toMatch}` body: `{property, activeProfiles,
/// propertySources}` where `property` is the winning `{source, value}` (or
/// `null`) and `propertySources` lists each source that carries the property.
fn render_property_detail(
    name: &str,
    profiles: Vec<String>,
    sources: &[PropertySourceView],
) -> Value {
    let mut winning: Option<Value> = None;
    let mut per_source: Vec<Value> = Vec::new();
    for source in sources {
        if let Some(prop) = source.properties.get(name) {
            per_source.push(json!({ "name": source.name, "property": prop }));
            if winning.is_none() {
                winning = Some(json!({ "source": source.name, "value": prop.value }));
            }
        }
    }
    json!({
        "property": winning,
        "activeProfiles": profiles,
        "propertySources": per_source,
    })
}

/// Returns `value` when `key` matches an allow prefix (case-insensitive),
/// `"***"` otherwise.
fn redact(key: &str, value: String, allow_prefixes: &[String]) -> String {
    let upper = key.to_uppercase();
    for prefix in allow_prefixes {
        if upper.starts_with(&prefix.to_uppercase()) {
            return value;
        }
    }
    "***".to_string()
}

/// `GET /actuator/tasks` — `{"count": N}` where N is the number of alive
/// tokio tasks, the async-Rust analog of Go's goroutine count. With
/// `?dump=true`, returns a plain-text tokio runtime report (Rust cannot
/// capture per-task stacks the way `runtime.Stack` dumps goroutines, so
/// the dump reports runtime metrics instead).
async fn tasks_handler(Query(params): Query<HashMap<String, String>>) -> Response {
    let metrics = tokio::runtime::Handle::current().metrics();
    if params.get("dump").map(String::as_str) == Some("true") {
        use std::fmt::Write as _;
        let mut dump = String::new();
        let _ = writeln!(dump, "=== tokio runtime ===");
        let _ = writeln!(dump, "workers: {}", metrics.num_workers());
        let _ = writeln!(dump, "alive_tasks: {}", metrics.num_alive_tasks());
        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], dump).into_response();
    }
    json_response(
        StatusCode::OK,
        &json!({ "count": metrics.num_alive_tasks() }),
    )
}

/// `GET /actuator/threaddump` — Spring Boot's thread-dump endpoint, adapted
/// to async Rust: `{"threads": […]}` describing the tokio runtime's worker
/// threads plus a synthetic runtime-summary thread (async Rust has no
/// per-task stack frames — see [`crate::thread_dump`]).
async fn threaddump_handler() -> Response {
    json_response(StatusCode::OK, &thread_dump())
}

/// `GET /actuator/version` — framework, app, and language version stamp.
/// The Go port reports its toolchain under `"go"`; this port reports the
/// minimum supported Rust version under `"rust"`.
async fn version_handler(State(st): State<SharedState>) -> Response {
    json_response(
        StatusCode::OK,
        &json!({
            "firefly": VERSION,
            "app": st.app_name,
            "appVersion": st.app_version,
            "rust": env!("CARGO_PKG_RUST_VERSION"),
            "buildTime": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }),
    )
}

/// `GET /actuator/loggers` — Spring's levels vocabulary + every
/// configured logger + groups.
async fn loggers_handler(State(st): State<SharedState>) -> Response {
    match &st.loggers {
        Some(loggers) => json_response(StatusCode::OK, &loggers.levels_json()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /actuator/loggers/{name}` — one logger's configured + effective
/// level.
async fn logger_get_handler(State(st): State<SharedState>, Path(name): Path<String>) -> Response {
    match &st.loggers {
        Some(loggers) => json_response(StatusCode::OK, &loggers.logger_json(&name)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /actuator/loggers/{name}` — body `{"configuredLevel": "DEBUG"}`
/// (or `null` / empty to reset). 204 on success, 400 on a bad level.
async fn logger_post_handler(
    State(st): State<SharedState>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let Some(loggers) = &st.loggers else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let level: Option<String> = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(payload) => match payload.get("configuredLevel") {
                Some(Value::String(level)) => Some(level.clone()),
                Some(Value::Null) | None => None,
                Some(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": "configuredLevel must be a string or null" }),
                    )
                }
            },
            Err(_) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": "invalid JSON body" }),
                )
            }
        }
    };
    match loggers.set_level(&name, level.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(LoggersError::UnknownLevel(msg)) => {
            json_response(StatusCode::BAD_REQUEST, &json!({ "error": msg }))
        }
        Err(LoggersError::Reload(msg)) => {
            json_response(StatusCode::INTERNAL_SERVER_ERROR, &json!({ "error": msg }))
        }
    }
}

/// `GET /actuator/scheduledtasks` — tasks grouped by trigger type
/// (cron / fixedDelay / fixedRate), intervals in milliseconds.
async fn scheduledtasks_handler(State(st): State<SharedState>) -> Response {
    match &st.scheduled_tasks {
        Some(source) => json_response(StatusCode::OK, &render_tasks(&source.scheduled_tasks())),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /actuator/caches` — Spring's
/// `{"cacheManagers": {"cacheManager": {"caches": {…}}}}` shape.
async fn caches_handler(State(st): State<SharedState>) -> Response {
    let Some(ops) = &st.cache_ops else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut caches = serde_json::Map::new();
    for descriptor in ops.caches() {
        caches.insert(descriptor.name, json!({ "target": descriptor.target }));
    }
    json_response(
        StatusCode::OK,
        &json!({ "cacheManagers": { CACHE_MANAGER: { "caches": caches } } }),
    )
}

/// `GET /actuator/caches/{name}` — a single cache's descriptor; 404 for
/// unknown names.
async fn cache_detail_handler(State(st): State<SharedState>, Path(name): Path<String>) -> Response {
    let Some(ops) = &st.cache_ops else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match ops.caches().into_iter().find(|c| c.name == name) {
        Some(descriptor) => json_response(
            StatusCode::OK,
            &json!({
                "name": descriptor.name,
                "cacheManager": CACHE_MANAGER,
                "target": descriptor.target,
            }),
        ),
        None => json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": format!("No such cache: {name}") }),
        ),
    }
}

/// `POST /actuator/caches/{name}/evict` — clears the named cache; 204
/// on success, 404 for unknown names.
async fn cache_evict_handler(State(st): State<SharedState>, Path(name): Path<String>) -> Response {
    let Some(ops) = &st.cache_ops else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if ops.evict(&name).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        json_response(
            StatusCode::NOT_FOUND,
            &json!({ "error": format!("No such cache: {name}") }),
        )
    }
}

/// `POST /actuator/refresh` — Spring Cloud's context refresh; answers
/// `{"refreshed": [keys…]}`.
async fn refresh_handler(State(st): State<SharedState>) -> Response {
    match &st.refresher {
        Some(refresher) => json_response(
            StatusCode::OK,
            &json!({ "refreshed": refresher.refresh().await }),
        ),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /actuator/httpexchanges` — the recorded exchanges, newest first.
async fn httpexchanges_handler(State(st): State<SharedState>) -> Response {
    match &st.http_exchanges {
        Some(recorder) => json_response(StatusCode::OK, &json!({ "exchanges": recorder.recent() })),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_passes_allow_listed_prefixes() {
        let allow = vec!["FIREFLY_".to_string()];
        assert_eq!(redact("FIREFLY_WEB_PORT", "8080".into(), &allow), "8080");
        assert_eq!(
            redact("DATABASE_PASSWORD", "supersecret".into(), &allow),
            "***"
        );
    }

    #[test]
    fn redact_is_case_insensitive() {
        let allow = vec!["firefly_".to_string()];
        assert_eq!(redact("FIREFLY_X", "1".into(), &allow), "1");
        assert_eq!(redact("firefly_y", "2".into(), &allow), "2");
        assert_eq!(redact("other", "3".into(), &allow), "***");
    }

    #[test]
    fn config_defaults_show_flags_true() {
        let cfg = ActuatorConfig::default();
        assert!(cfg.show_details);
        assert!(cfg.show_components);
        assert!(cfg.exposure.is_exposed("metrics"));
    }
}
