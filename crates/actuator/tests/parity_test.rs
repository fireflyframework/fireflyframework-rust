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

//! pyfly-parity HTTP tests for the extended actuator surface, ported
//! from `tests/actuator/` in the Python port (`test_probes`,
//! `test_health`, `test_exposure`, `test_custom_endpoint`,
//! `test_loggers_endpoint`, `test_metrics_endpoint`,
//! `test_extra_endpoints`, `test_prometheus_endpoint`) — driven through
//! `tower::ServiceExt::oneshot`, no sockets.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::routing::get;
use axum::Router;
use firefly_actuator::{
    mount, ActuatorConfig, CacheDescriptor, CacheOps, Endpoint, EndpointRegistry, EnvSource,
    ExposureConfig, HealthComposite, HealthResult, HttpExchangeRecorder, HttpExchangesLayer,
    IndicatorFn, LoggersState, MetricRegistry, ProbeGroup, PropertySourceView, PropertyView,
    Refresher, StaticScheduledTasks, TaskDescriptor, TaskTrigger,
};
use http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tower::ServiceExt;

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

async fn request(
    app: Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let body = match body {
        Some(s) => Body::from(s.to_string()),
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
    let (status, _, body) = request(app, "GET", uri, None, &[]).await;
    (status, serde_json::from_slice(&body).unwrap())
}

async fn post_json(app: Router, uri: &str, body: Option<&str>) -> (StatusCode, Vec<u8>) {
    let (status, _, bytes) = request(
        app,
        "POST",
        uri,
        body,
        &[("content-type", "application/json")],
    )
    .await;
    (status, bytes)
}

fn up_indicator(
    name: &'static str,
) -> IndicatorFn<impl Fn() -> std::future::Ready<HealthResult> + Send + Sync> {
    IndicatorFn::new(name, || std::future::ready(HealthResult::up()))
}

fn down_indicator(
    name: &'static str,
) -> IndicatorFn<impl Fn() -> std::future::Ready<HealthResult> + Send + Sync> {
    IndicatorFn::new(name, || std::future::ready(HealthResult::down("offline")))
}

fn mount_health(health: Arc<HealthComposite>) -> Router {
    mount(ActuatorConfig {
        health,
        ..ActuatorConfig::default()
    })
}

// ---------------------------------------------------------------------
// Probe routes (pyfly test_probes.py::TestProbeRoutes)
// ---------------------------------------------------------------------

#[tokio::test]
async fn liveness_route_200() {
    let health = Arc::new(HealthComposite::new());
    health.add_with_groups(up_indicator("svc"), &[ProbeGroup::Liveness]);
    let (status, body) = get_json(mount_health(health), "/actuator/health/liveness").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
}

#[tokio::test]
async fn liveness_route_503() {
    let health = Arc::new(HealthComposite::new());
    health.add_with_groups(down_indicator("svc"), &[ProbeGroup::Liveness]);
    let (status, body) = get_json(mount_health(health), "/actuator/health/liveness").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");
}

#[tokio::test]
async fn readiness_route_200_and_503() {
    let health = Arc::new(HealthComposite::new());
    health.add_with_groups(up_indicator("svc"), &[ProbeGroup::Readiness]);
    let (status, body) = get_json(
        mount_health(Arc::clone(&health)),
        "/actuator/health/readiness",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");

    let health = Arc::new(HealthComposite::new());
    health.add_with_groups(down_indicator("svc"), &[ProbeGroup::Readiness]);
    let (status, body) = get_json(mount_health(health), "/actuator/health/readiness").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");
}

// pyfly: test_probe_isolation_via_routes
#[tokio::test]
async fn probe_isolation_via_routes() {
    let health = Arc::new(HealthComposite::new());
    health.add_with_groups(down_indicator("live-only"), &[ProbeGroup::Liveness]);
    health.add_with_groups(up_indicator("ready-only"), &[ProbeGroup::Readiness]);

    let (liveness, _) = get_json(
        mount_health(Arc::clone(&health)),
        "/actuator/health/liveness",
    )
    .await;
    let (readiness, _) = get_json(
        mount_health(Arc::clone(&health)),
        "/actuator/health/readiness",
    )
    .await;
    let (overall, _) = get_json(mount_health(health), "/actuator/health").await;

    assert_eq!(liveness, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(readiness, StatusCode::OK);
    assert_eq!(overall, StatusCode::SERVICE_UNAVAILABLE);
}

// pyfly: test_index_includes_probe_links
#[tokio::test]
async fn index_includes_probe_links() {
    let (status, body) = get_json(mount(ActuatorConfig::default()), "/actuator").await;
    assert_eq!(status, StatusCode::OK);
    let links = &body["_links"];
    assert_eq!(links["self"]["href"], "/actuator");
    assert_eq!(links["health"]["href"], "/actuator/health");
    assert_eq!(
        links["health/liveness"]["href"],
        "/actuator/health/liveness"
    );
    assert_eq!(
        links["health/readiness"]["href"],
        "/actuator/health/readiness"
    );
}

// ---------------------------------------------------------------------
// Named groups + component drill-down (pyfly health_endpoint.handle_path)
// ---------------------------------------------------------------------

#[tokio::test]
async fn named_group_drill_down() {
    let health = Arc::new(HealthComposite::new());
    health.add(up_indicator("db"));
    health.add(down_indicator("broker"));
    health.add_group("storage", &["db"]);

    let (status, body) = get_json(mount_health(health), "/actuator/health/storage").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
    assert!(body["details"]["db"].is_object());
    assert!(body["details"].get("broker").is_none());
}

#[tokio::test]
async fn component_drill_down_200_and_503() {
    let health = Arc::new(HealthComposite::new());
    health.add(up_indicator("db"));
    health.add(down_indicator("broker"));

    let (status, body) = get_json(mount_health(Arc::clone(&health)), "/actuator/health/db").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");

    let (status, body) = get_json(mount_health(health), "/actuator/health/broker").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");
    assert_eq!(body["message"], "offline");
}

#[tokio::test]
async fn unknown_health_selector_is_404_with_error() {
    let (status, body) = get_json(mount(ActuatorConfig::default()), "/actuator/health/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "No such health component or group: nope");
}

// ---------------------------------------------------------------------
// show-details / show-components (Spring's health endpoint switches)
// ---------------------------------------------------------------------

#[tokio::test]
async fn show_components_false_omits_details_map() {
    let health = Arc::new(HealthComposite::new());
    health.add(up_indicator("db"));
    let app = mount(ActuatorConfig {
        health,
        show_components: false,
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "UP" }));
}

#[tokio::test]
async fn show_details_false_reduces_components_to_status() {
    let health = Arc::new(HealthComposite::new());
    health.add(down_indicator("broker"));
    let app = mount(ActuatorConfig {
        health: Arc::clone(&health),
        show_details: false,
        ..ActuatorConfig::default()
    });
    let (_, body) = get_json(app, "/actuator/health").await;
    assert_eq!(body["details"]["broker"], json!({ "status": "DOWN" }));

    // Component drill-down honors the switch too.
    let app = mount(ActuatorConfig {
        health,
        show_details: false,
        ..ActuatorConfig::default()
    });
    let (_, body) = get_json(app, "/actuator/health/broker").await;
    assert_eq!(body, json!({ "status": "DOWN" }));
}

// ---------------------------------------------------------------------
// Exposure model (pyfly test_exposure.py)
// ---------------------------------------------------------------------

// pyfly: test_actuator_on_by_default_health_and_info_exposed
#[tokio::test]
async fn spring_default_exposes_only_health_and_info() {
    let app = || {
        mount(ActuatorConfig {
            exposure: ExposureConfig::spring_default(),
            ..ActuatorConfig::default()
        })
    };
    let (status, _) = get_json(app(), "/actuator/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(app(), "/actuator/info").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = request(app(), "GET", "/actuator/metrics", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = request(app(), "GET", "/actuator/env", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// pyfly: test_wildcard_exposes_everything_except_excluded
#[tokio::test]
async fn wildcard_exposure_with_exclude() {
    let app = || {
        mount(ActuatorConfig {
            exposure: ExposureConfig::from_csv("*", "env"),
            ..ActuatorConfig::default()
        })
    };
    let (status, _, _) = request(app(), "GET", "/actuator/metrics", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = request(app(), "GET", "/actuator/env", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "excluded wins");
}

// pyfly: test_prometheus_reachable_when_exposed
#[tokio::test]
async fn prometheus_reachable_when_exposed() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("prometheus", ""),
        ..ActuatorConfig::default()
    });
    let (status, headers, _) = request(app, "GET", "/actuator/prometheus", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
}

// pyfly: test_custom_base_path
#[tokio::test]
async fn custom_base_path() {
    let app = || {
        mount(ActuatorConfig {
            exposure: ExposureConfig {
                base_path: "/manage".into(),
                ..ExposureConfig::default()
            },
            ..ActuatorConfig::default()
        })
    };
    let (status, _) = get_json(app(), "/manage/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = request(app(), "GET", "/actuator/health", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Index moved too.
    let (_, body) = get_json(app(), "/manage").await;
    assert_eq!(body["_links"]["health"]["href"], "/manage/health");
}

// Per-endpoint enabled override disables a built-in.
#[tokio::test]
async fn endpoint_enabled_override_disables_builtin() {
    let mut exposure = ExposureConfig::default();
    exposure.endpoint_enabled.insert("env".into(), false);
    let app = mount(ActuatorConfig {
        exposure,
        ..ActuatorConfig::default()
    });
    let (status, _, _) = request(app, "GET", "/actuator/env", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------
// Custom endpoints (pyfly test_custom_endpoint.py)
// ---------------------------------------------------------------------

struct GitInfoEndpoint;

#[async_trait]
impl Endpoint for GitInfoEndpoint {
    fn id(&self) -> &str {
        "git"
    }
    async fn handle(
        &self,
        _selector: Option<&str>,
        _query: &HashMap<String, String>,
    ) -> Option<Value> {
        Some(json!({ "branch": "main", "commit": "abc123" }))
    }
}

fn mount_with_git(exposure: ExposureConfig) -> Router {
    let endpoints = Arc::new(EndpointRegistry::new());
    endpoints.register(GitInfoEndpoint);
    mount(ActuatorConfig {
        endpoints,
        exposure,
        ..ActuatorConfig::default()
    })
}

// pyfly: test_custom_endpoint_auto_discovered
#[tokio::test]
async fn custom_endpoint_is_mounted() {
    let (status, body) = get_json(mount_with_git(ExposureConfig::default()), "/actuator/git").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["branch"], "main");
    assert_eq!(body["commit"], "abc123");
}

// pyfly: test_custom_endpoint_in_index
#[tokio::test]
async fn custom_endpoint_in_index() {
    let (_, body) = get_json(mount_with_git(ExposureConfig::default()), "/actuator").await;
    assert_eq!(body["_links"]["git"]["href"], "/actuator/git");
}

// pyfly: test_custom_endpoint_disabled_by_config
#[tokio::test]
async fn custom_endpoint_disabled_by_config() {
    let mut exposure = ExposureConfig::default();
    exposure.endpoint_enabled.insert("git".into(), false);
    let (status, _, _) = request(mount_with_git(exposure), "GET", "/actuator/git", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

struct SelectorEndpoint;

#[async_trait]
impl Endpoint for SelectorEndpoint {
    fn id(&self) -> &str {
        "lookup"
    }
    fn supports_selector(&self) -> bool {
        true
    }
    async fn handle(
        &self,
        selector: Option<&str>,
        query: &HashMap<String, String>,
    ) -> Option<Value> {
        match selector {
            None => Some(json!({ "all": true, "q": query.get("q") })),
            Some("known") => Some(json!({ "found": "known" })),
            Some(_) => None,
        }
    }
}

#[tokio::test]
async fn custom_endpoint_selector_drill_down_and_404() {
    let endpoints = Arc::new(EndpointRegistry::new());
    endpoints.register(SelectorEndpoint);
    let app = || {
        mount(ActuatorConfig {
            endpoints: Arc::clone(&endpoints),
            ..ActuatorConfig::default()
        })
    };
    let (status, body) = get_json(app(), "/actuator/lookup?q=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["all"], true);
    assert_eq!(body["q"], "1");

    let (status, body) = get_json(app(), "/actuator/lookup/known").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["found"], "known");

    let (status, body) = get_json(app(), "/actuator/lookup/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "No such lookup: missing");
}

// ---------------------------------------------------------------------
// Loggers endpoint (pyfly test_loggers_endpoint.py)
// ---------------------------------------------------------------------

fn loggers_app(directives: &str) -> (Router, Arc<LoggersState>) {
    let state = Arc::new(LoggersState::with_reload_fn(|_| Ok(()), directives));
    let app = mount(ActuatorConfig {
        loggers: Some(Arc::clone(&state)),
        ..ActuatorConfig::default()
    });
    (app, state)
}

// pyfly: test_get_lists_loggers_levels_and_groups
#[tokio::test]
async fn loggers_get_lists_levels_loggers_and_groups() {
    let (app, _) = loggers_app("info,my_crate=debug");
    let (status, body) = get_json(app, "/actuator/loggers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["levels"],
        json!(["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"])
    );
    assert!(body["loggers"]["ROOT"].is_object());
    assert_eq!(body["loggers"]["my_crate"]["configuredLevel"], "DEBUG");
    assert!(body["groups"].is_object());
}

// pyfly: test_get_single_logger_by_name
#[tokio::test]
async fn loggers_get_single_logger() {
    let (app, _) = loggers_app("info,app::db=debug");
    let (status, body) = get_json(app, "/actuator/loggers/app::db").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configuredLevel"], "DEBUG");
    assert!(body.get("effectiveLevel").is_some());
}

// pyfly: test_post_sets_level_via_path_and_returns_204
#[tokio::test]
async fn loggers_post_sets_level_204() {
    let (app, state) = loggers_app("warn");
    let (status, _) = post_json(
        app,
        "/actuator/loggers/my_crate",
        Some(r#"{"configuredLevel": "DEBUG"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(state.directives(), "warn,my_crate=debug");
}

// pyfly: test_post_null_resets_level
#[tokio::test]
async fn loggers_post_null_resets_204() {
    let (app, state) = loggers_app("warn");
    state.set_level("my_crate", Some("DEBUG")).unwrap();
    let (status, _) = post_json(
        app,
        "/actuator/loggers/my_crate",
        Some(r#"{"configuredLevel": null}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(state.directives(), "warn");
}

// pyfly: test_post_accepts_trace_and_off
#[tokio::test]
async fn loggers_post_accepts_trace_and_off() {
    let (app, _) = loggers_app("info");
    for level in ["TRACE", "OFF"] {
        let (status, _) = post_json(
            app.clone(),
            "/actuator/loggers/some::target",
            Some(&format!(r#"{{"configuredLevel": "{level}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{level}");
    }
}

// pyfly: test_post_invalid_level_returns_400
#[tokio::test]
async fn loggers_post_invalid_level_400() {
    let (app, _) = loggers_app("info");
    let (status, body) = post_json(
        app,
        "/actuator/loggers/ROOT",
        Some(r#"{"configuredLevel": "BANANA"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(body["error"].as_str().unwrap().contains("BANANA"));
}

// End-to-end over a real tracing_subscriber reload handle.
#[tokio::test]
async fn loggers_post_reloads_real_env_filter() {
    use tracing_subscriber::{reload, EnvFilter, Registry};
    let (layer, handle) = reload::Layer::<EnvFilter, Registry>::new(EnvFilter::new("info"));
    let state = Arc::new(LoggersState::from_handle_with_directives(
        handle.clone(),
        "info",
    ));
    let app = mount(ActuatorConfig {
        loggers: Some(state),
        ..ActuatorConfig::default()
    });
    let (status, _) = post_json(
        app,
        "/actuator/loggers/my_crate",
        Some(r#"{"configuredLevel": "TRACE"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let current = handle.with_current(|f| f.to_string()).unwrap();
    assert!(current.contains("my_crate=trace"), "{current}");
    drop(layer);
}

// ---------------------------------------------------------------------
// Scheduled tasks endpoint (pyfly test_extra_endpoints.py)
// ---------------------------------------------------------------------

// pyfly: test_scheduledtasks_groups_by_trigger
#[tokio::test]
async fn scheduledtasks_groups_by_trigger() {
    let source = StaticScheduledTasks(vec![
        TaskDescriptor {
            name: "ReportService.emit".into(),
            trigger: TaskTrigger::FixedRate {
                interval: Duration::from_secs(30),
                initial_delay: None,
            },
        },
        TaskDescriptor {
            name: "Cleaner.purge".into(),
            trigger: TaskTrigger::Cron {
                expression: "0 0 * * *".into(),
            },
        },
    ]);
    let app = mount(ActuatorConfig {
        scheduled_tasks: Some(Arc::new(source)),
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(app, "/actuator/scheduledtasks").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("cron").is_some());
    assert!(body.get("fixedRate").is_some());
    assert!(body.get("fixedDelay").is_some());
    assert_eq!(
        body["fixedRate"][0]["runnable"]["target"],
        "ReportService.emit"
    );
    assert_eq!(body["fixedRate"][0]["interval"], 30000);
    assert_eq!(body["cron"][0]["expression"], "0 0 * * *");
}

#[tokio::test]
async fn scheduledtasks_not_mounted_without_source() {
    let (status, _, _) = request(
        mount(ActuatorConfig::default()),
        "GET",
        "/actuator/scheduledtasks",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------
// Caches endpoint (pyfly test_extra_endpoints.py::test_caches_shape)
// ---------------------------------------------------------------------

struct FakeCacheOps;

#[async_trait]
impl CacheOps for FakeCacheOps {
    fn caches(&self) -> Vec<CacheDescriptor> {
        vec![CacheDescriptor {
            name: "default".into(),
            target: "firefly_cache::MemoryAdapter".into(),
        }]
    }
    async fn evict(&self, name: &str) -> bool {
        name == "default"
    }
}

fn caches_app() -> Router {
    mount(ActuatorConfig {
        cache_ops: Some(Arc::new(FakeCacheOps)),
        ..ActuatorConfig::default()
    })
}

#[tokio::test]
async fn caches_shape() {
    let (status, body) = get_json(caches_app(), "/actuator/caches").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["cacheManagers"]["cacheManager"]["caches"]["default"]["target"],
        "firefly_cache::MemoryAdapter"
    );
}

#[tokio::test]
async fn cache_detail_and_404() {
    let (status, body) = get_json(caches_app(), "/actuator/caches/default").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "default");
    assert_eq!(body["cacheManager"], "cacheManager");

    let (status, body) = get_json(caches_app(), "/actuator/caches/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "No such cache: nope");
}

#[tokio::test]
async fn cache_evict_204_and_404() {
    let (status, _) = post_json(caches_app(), "/actuator/caches/default/evict", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = post_json(caches_app(), "/actuator/caches/nope/evict", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------
// Refresh endpoint (pyfly refresh_endpoint.py)
// ---------------------------------------------------------------------

struct FakeRefresher;

#[async_trait]
impl Refresher for FakeRefresher {
    async fn refresh(&self) -> Vec<String> {
        vec!["app.timeout".into()]
    }
}

#[tokio::test]
async fn refresh_post_returns_refreshed_keys() {
    let app = mount(ActuatorConfig {
        refresher: Some(Arc::new(FakeRefresher)),
        ..ActuatorConfig::default()
    });
    let (status, body) = post_json(app, "/actuator/refresh", None).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["refreshed"], json!(["app.timeout"]));
}

#[tokio::test]
async fn refresh_get_is_405() {
    let app = mount(ActuatorConfig {
        refresher: Some(Arc::new(FakeRefresher)),
        ..ActuatorConfig::default()
    });
    let (status, _, _) = request(app, "GET", "/actuator/refresh", None, &[]).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

// ---------------------------------------------------------------------
// HTTP exchanges (pyfly test_extra_endpoints.py::test_httpexchanges_records_requests)
// ---------------------------------------------------------------------

#[tokio::test]
async fn httpexchanges_records_requests_through_layer() {
    let recorder = Arc::new(HttpExchangeRecorder::new());

    // Application router wrapped in the recording layer.
    let app = Router::new()
        .route("/widgets/:id", get(|| async { "ok" }))
        .layer(HttpExchangesLayer::new(Arc::clone(&recorder)));
    let (status, _, _) = request(
        app,
        "GET",
        "/widgets/7",
        None,
        &[
            ("user-agent", "parity-test"),
            ("authorization", "Bearer secret"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Actuator surface reads the shared recorder.
    let actuator = mount(ActuatorConfig {
        http_exchanges: Some(Arc::clone(&recorder)),
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(actuator, "/actuator/httpexchanges").await;
    assert_eq!(status, StatusCode::OK);
    let exchanges = body["exchanges"].as_array().unwrap();
    assert_eq!(exchanges.len(), 1);
    let exchange = &exchanges[0];
    assert_eq!(exchange["request"]["method"], "GET");
    assert_eq!(exchange["request"]["uri"], "/widgets/7");
    assert_eq!(
        exchange["request"]["headers"]["user-agent"][0],
        "parity-test"
    );
    assert_eq!(exchange["request"]["headers"]["authorization"][0], "******");
    assert_eq!(exchange["response"]["status"], 200);
    assert!(exchange["timeTaken"].as_str().unwrap().starts_with("PT"));
}

#[tokio::test]
async fn httpexchanges_layer_skips_excluded_prefixes() {
    let recorder = Arc::new(HttpExchangeRecorder::new());
    let app = Router::new()
        .route("/actuator/prometheus", get(|| async { "metrics" }))
        .layer(HttpExchangesLayer::new(Arc::clone(&recorder)));
    let (status, _, _) = request(app, "GET", "/actuator/prometheus", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(recorder.is_empty(), "prometheus scrapes are not recorded");
}

// ---------------------------------------------------------------------
// Metrics: Micrometer JSON over HTTP (pyfly test_metrics_endpoint.py)
// ---------------------------------------------------------------------

fn metrics_app(registry: Arc<MetricRegistry>) -> Router {
    mount(ActuatorConfig {
        metric_registry: registry,
        ..ActuatorConfig::default()
    })
}

// pyfly: test_list_returns_dot_meter_names (adapted: names are native)
#[tokio::test]
async fn metrics_accept_json_lists_names() {
    let registry = Arc::new(MetricRegistry::new());
    registry.counter("orders_total").inc();
    registry.histogram("latency_seconds").observe(0.1);
    let (status, _, body) = request(
        metrics_app(registry),
        "GET",
        "/actuator/metrics",
        None,
        &[("accept", "application/json")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["names"], json!(["latency_seconds", "orders_total"]));
}

// pyfly: test_detail_counter_uses_count_statistic
#[tokio::test]
async fn metric_detail_counter_count_statistic() {
    let registry = Arc::new(MetricRegistry::new());
    registry
        .counter_with("orders_total", &[("method", "GET")])
        .add(5);
    let (status, body) = get_json(metrics_app(registry), "/actuator/metrics/orders_total").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "orders_total");
    assert_eq!(body["measurements"][0]["statistic"], "COUNT");
    assert_eq!(body["measurements"][0]["value"], 5.0);
    assert_eq!(body["availableTags"][0]["tag"], "method");
    assert_eq!(body["availableTags"][0]["values"], json!(["GET"]));
}

// pyfly: test_detail_tag_filter
#[tokio::test]
async fn metric_detail_tag_filter() {
    let registry = Arc::new(MetricRegistry::new());
    registry
        .counter_with("hits_total", &[("region", "eu")])
        .add(3);
    registry
        .counter_with("hits_total", &[("region", "us")])
        .add(7);
    let (status, body) = get_json(
        metrics_app(registry),
        "/actuator/metrics/hits_total?tag=region:eu",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["measurements"][0]["value"], 3.0);
}

// pyfly: test_detail_summary_count_sum_and_baseunit
#[tokio::test]
async fn metric_detail_histogram_statistics_and_base_unit() {
    let registry = Arc::new(MetricRegistry::new());
    let h = registry.histogram_with("latency_seconds", &[("uri", "/a")]);
    h.observe(0.5);
    h.observe(1.5);
    let (_, body) = get_json(metrics_app(registry), "/actuator/metrics/latency_seconds").await;
    let stats: HashMap<String, f64> = body["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["statistic"].as_str().unwrap().to_string(),
                m["value"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(stats["COUNT"], 2.0);
    assert_eq!(stats["TOTAL_TIME"], 2.0);
    assert_eq!(stats["MAX"], 1.5);
    assert_eq!(body["baseUnit"], "seconds");
}

// pyfly: test_unknown_meter_returns_none
#[tokio::test]
async fn metric_detail_unknown_is_404() {
    let (status, body) = get_json(
        metrics_app(Arc::new(MetricRegistry::new())),
        "/actuator/metrics/nope_does_not_exist",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "No such metric: nope_does_not_exist");
}

// ---------------------------------------------------------------------
// Prometheus endpoint (pyfly test_prometheus_endpoint.py)
// ---------------------------------------------------------------------

#[tokio::test]
async fn prometheus_serves_labeled_text_exposition() {
    let registry = Arc::new(MetricRegistry::new());
    registry
        .counter_with("hits_total", &[("region", "eu")])
        .add(3);
    registry
        .histogram_with_buckets("req_seconds", &[], &[0.5])
        .observe(0.1);
    let app = mount(ActuatorConfig {
        metric_registry: registry,
        ..ActuatorConfig::default()
    });
    let (status, headers, body) = request(app, "GET", "/actuator/prometheus", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("hits_total{region=\"eu\"} 3"), "{text}");
    assert!(text.contains("# TYPE req_seconds histogram"), "{text}");
    assert!(text.contains("req_seconds_bucket{le=\"+Inf\"} 1"), "{text}");
}

// ---------------------------------------------------------------------
// Index (pyfly test_extra_endpoints.py::test_index_lists_all_exposed_endpoints)
// ---------------------------------------------------------------------

#[tokio::test]
async fn index_lists_all_exposed_endpoints() {
    let app = mount(ActuatorConfig {
        scheduled_tasks: Some(Arc::new(StaticScheduledTasks(Vec::new()))),
        cache_ops: Some(Arc::new(FakeCacheOps)),
        refresher: Some(Arc::new(FakeRefresher)),
        http_exchanges: Some(Arc::new(HttpExchangeRecorder::new())),
        loggers: Some(Arc::new(LoggersState::with_reload_fn(|_| Ok(()), "info"))),
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(app, "/actuator").await;
    assert_eq!(status, StatusCode::OK);
    let links = body["_links"].as_object().unwrap();
    for id in [
        "self",
        "health",
        "info",
        "metrics",
        "prometheus",
        "env",
        "tasks",
        "version",
        "loggers",
        "scheduledtasks",
        "caches",
        "refresh",
        "httpexchanges",
    ] {
        assert!(links.contains_key(id), "missing link: {id}");
    }
}

// Spring-default index only links health + info (+ probe links).
#[tokio::test]
async fn spring_default_index_links_only_health_and_info() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::spring_default(),
        ..ActuatorConfig::default()
    });
    let (_, body) = get_json(app, "/actuator").await;
    let links = body["_links"].as_object().unwrap();
    assert!(links.contains_key("health"));
    assert!(links.contains_key("info"));
    assert!(!links.contains_key("metrics"));
    assert!(!links.contains_key("env"));
}

// ---------------------------------------------------------------------
// /actuator/env Spring property-source view (pyfly EnvEndpoint)
// ---------------------------------------------------------------------

/// A test [`EnvSource`] reproducing the firefly-config bridge: two ordered
/// sources (highest precedence first) plus a couple of active profiles, with
/// a pre-masked secret value.
struct FakeEnvSource;

impl EnvSource for FakeEnvSource {
    fn active_profiles(&self) -> Vec<String> {
        vec!["dev".into(), "test".into()]
    }
    fn property_sources(&self) -> Vec<PropertySourceView> {
        vec![
            PropertySourceView {
                name: "systemEnvironment".into(),
                properties: BTreeMap::from([(
                    "app.name".into(),
                    PropertyView {
                        value: "orders".into(),
                        origin: "System Environment Property".into(),
                    },
                )]),
            },
            PropertySourceView {
                name: "applicationConfig".into(),
                properties: BTreeMap::from([
                    (
                        "app.name".into(),
                        PropertyView {
                            value: "orders-file".into(),
                            origin: "applicationConfig".into(),
                        },
                    ),
                    (
                        "db.password".into(),
                        PropertyView {
                            value: "******".into(),
                            origin: "applicationConfig".into(),
                        },
                    ),
                ]),
            },
        ]
    }
}

fn mount_with_env_source() -> Router {
    mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("*", ""),
        env_source: Some(Arc::new(FakeEnvSource)),
        ..ActuatorConfig::default()
    })
}

/// pyfly `test_shows_active_profiles` + Spring `/actuator/env` shape: when an
/// `EnvSource` is wired, `/actuator/env` returns `{activeProfiles,
/// propertySources}` with ordered, masked, origin-attributed properties.
#[tokio::test]
async fn env_returns_spring_property_source_view() {
    let (status, body) = get_json(mount_with_env_source(), "/actuator/env").await;
    assert_eq!(status, StatusCode::OK);
    let profiles = body["activeProfiles"].as_array().unwrap();
    assert!(profiles.iter().any(|p| p == "dev"));
    assert!(profiles.iter().any(|p| p == "test"));
    let sources = body["propertySources"].as_array().unwrap();
    // highest precedence first
    assert_eq!(sources[0]["name"], "systemEnvironment");
    assert_eq!(sources[1]["name"], "applicationConfig");
    assert_eq!(
        sources[0]["properties"]["app.name"],
        json!({"value": "orders", "origin": "System Environment Property"})
    );
    // secret stays masked exactly as the source provided it
    assert_eq!(sources[1]["properties"]["db.password"]["value"], "******");
}

/// Spring `/actuator/env/{toMatch}` drill-down: the winning value is the
/// highest-precedence source carrying the property, and every source that has
/// it is listed.
#[tokio::test]
async fn env_property_detail_drill_down() {
    let (status, body) = get_json(mount_with_env_source(), "/actuator/env/app.name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["property"],
        json!({"source": "systemEnvironment", "value": "orders"})
    );
    let per_source = body["propertySources"].as_array().unwrap();
    assert_eq!(per_source.len(), 2);
    assert_eq!(per_source[0]["name"], "systemEnvironment");
    assert!(body["activeProfiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "dev"));
}

/// An unknown property returns a well-formed body with a `null` winning
/// property and an empty per-source list (Spring shape preserved).
#[tokio::test]
async fn env_property_detail_unknown_is_null() {
    let (status, body) = get_json(mount_with_env_source(), "/actuator/env/nope").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["property"].is_null());
    assert!(body["propertySources"].as_array().unwrap().is_empty());
}

/// Without an `EnvSource` the legacy flat redacted env map is preserved
/// (backward compatibility) and the per-property drill-down route is not
/// mounted (404).
#[tokio::test]
async fn env_without_source_is_flat_and_no_drill_down() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("*", ""),
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(app, "/actuator/env").await;
    assert_eq!(status, StatusCode::OK);
    // Flat map: no Spring envelope keys.
    assert!(body.get("activeProfiles").is_none());
    assert!(body.get("propertySources").is_none());

    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("*", ""),
        ..ActuatorConfig::default()
    });
    let (status, _, _) = request(app, "GET", "/actuator/env/anything", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------
// /actuator/threaddump (pyfly ThreadDumpEndpoint)
// ---------------------------------------------------------------------

/// pyfly `test_threaddump_returns_threads`: `/actuator/threaddump` returns
/// `{threads:[…]}` with at least one entry carrying a `stackTrace` field and
/// the Spring field set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threaddump_returns_threads() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("*", ""),
        ..ActuatorConfig::default()
    });
    let (status, body) = get_json(app, "/actuator/threaddump").await;
    assert_eq!(status, StatusCode::OK);
    let threads = body["threads"].as_array().unwrap();
    assert!(!threads.is_empty());
    let first = &threads[0];
    assert!(first.get("threadName").is_some());
    assert!(first.get("threadId").is_some());
    assert!(first.get("daemon").is_some());
    assert!(first.get("threadState").is_some());
    assert!(first["stackTrace"].is_array());
}

/// `/actuator/threaddump` is wired into the index `_links` when exposed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threaddump_in_index() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::from_csv("*", ""),
        ..ActuatorConfig::default()
    });
    let (_, body) = get_json(app, "/actuator").await;
    assert!(body["_links"]
        .as_object()
        .unwrap()
        .contains_key("threaddump"));
}

/// `/actuator/threaddump` honors exposure exclusion (Spring-default omits it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threaddump_excluded_when_not_exposed() {
    let app = mount(ActuatorConfig {
        exposure: ExposureConfig::spring_default(),
        ..ActuatorConfig::default()
    });
    let (status, _, _) = request(app, "GET", "/actuator/threaddump", None, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
