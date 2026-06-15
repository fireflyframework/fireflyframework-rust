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

//! The admin router — [`mount`] assembles the full `/admin` surface: the SPA
//! shell + embedded static assets, the `/admin/api/*` JSON routes, the SSE
//! streams, and (in server mode) the instance-registry routes, all behind the
//! optional auth guard.
//!
//! This is the Rust rendering of pyfly's `AdminRouteBuilder.build_routes()`.
//! The Beans view (`GET /api/beans`, `/api/beans/{name}`, `/api/beans/graph`,
//! and the `/api/sse/beans` stream) is backed by the optional DI
//! [`Container`](firefly_container::Container) wired into
//! [`AdminDeps::container`](crate::AdminDeps::container) — when no container is
//! present the listing is empty rather than 404. The remaining DI-introspection
//! endpoints (`conditions` / `configprops`) and the route table (`mappings`)
//! are reported as empty stubs, and `GET /admin/api/views` returns the
//! [`AdminView`](crate::AdminView)-driven list.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{guard, AuthPolicy};
use crate::config::AdminConfig;
use crate::data;
use crate::deps::AdminDeps;
use crate::sse;

/// The vendored SPA: `index.html`, `css/`, `js/`, `assets/`. Served verbatim;
/// the `/admin/api` contract is identical to pyfly's, so the JS is unchanged.
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets");

/// Shared router state — the wired deps plus the config snapshot.
#[derive(Clone)]
struct AdminState {
    deps: Arc<AdminDeps>,
    cfg: Arc<AdminConfig>,
}

/// Mounts the admin dashboard, returning an [`axum::Router`] to nest into the
/// application (or serve on a dedicated admin port).
///
/// The router exposes, under [`cfg.path`](AdminConfig::path):
/// - the SPA shell (`GET {base}` and `GET {base}/{*rest}`) + static assets
///   (`{base}/static/*`), served with `Cache-Control: no-cache`;
/// - the full `/api/*` JSON route set (overview / health / env / config /
///   loggers / metrics / scheduled / caches / cqrs / transactions / traces /
///   logfile / runtime / server / views / settings);
/// - the `/api/sse/*` live streams;
/// - the `/api/instances` register/deregister routes when server mode is on
///   (i.e. [`AdminDeps::instances`] is `Some`).
///
/// When [`cfg.require_auth`](AdminConfig::require_auth) is set, every `/api/*`
/// route is guarded against [`firefly_security::Authentication`]; the shell
/// and assets stay public.
///
/// ```
/// use std::sync::Arc;
/// use firefly_actuator::{HealthComposite, MetricRegistry};
/// use firefly_admin::{mount, AdminConfig, AdminDeps, LogBuffer, TraceBuffer};
///
/// let deps = AdminDeps::new(
///     "orders", "1.0.0",
///     Arc::new(HealthComposite::new()),
///     Arc::new(MetricRegistry::new()),
///     Arc::new(TraceBuffer::new()),
///     LogBuffer::new(),
/// );
/// let router: axum::Router = mount(AdminConfig::default(), deps);
/// # let _ = router;
/// ```
pub fn mount(cfg: AdminConfig, deps: AdminDeps) -> Router {
    let base = cfg.base_path();
    let cfg = Arc::new(cfg);
    let deps = Arc::new(deps);
    let state = AdminState {
        deps: Arc::clone(&deps),
        cfg: Arc::clone(&cfg),
    };

    // --- /api JSON + SSE routes ---
    let mut api: Router<AdminState> = Router::new()
        .route("/overview", get(overview))
        .route("/health", get(health))
        .route("/env", get(env))
        .route("/config", get(config))
        .route("/loggers", get(loggers))
        .route("/loggers/*name", post(set_logger))
        .route("/metrics", get(metrics))
        .route("/metrics/*name", get(metric_detail))
        .route("/scheduled", get(scheduled))
        .route("/mappings", get(mappings))
        .route("/caches", get(caches))
        .route("/caches/keys", get(cache_keys))
        .route("/caches/:name/evict", post(cache_evict))
        .route("/cqrs", get(cqrs))
        .route("/beans", get(beans))
        .route("/beans/graph", get(bean_graph))
        .route("/beans/:name", get(bean_detail))
        .route("/transactions", get(transactions))
        .route("/traces", get(traces))
        .route("/logfile", get(logfile))
        .route("/logfile/clear", post(logfile_clear))
        .route("/runtime", get(runtime))
        .route("/server", get(server))
        .route("/views", get(views))
        .route("/views/:id", get(view_detail))
        .route("/settings", get(settings))
        .route("/sse/beans", get(sse_beans))
        .route("/sse/health", get(sse_health))
        .route("/sse/metrics", get(sse_metrics))
        .route("/sse/traces", get(sse_traces))
        .route("/sse/logfile", get(sse_logfile))
        .route("/sse/runtime", get(sse_runtime))
        .route("/sse/server", get(sse_server));

    // --- Instance registry routes (server mode) ---
    if deps.instances.is_some() {
        api = api
            .route("/instances", get(instances_list).post(instances_register))
            .route("/instances/:name", delete(instances_deregister));
    }

    // --- Auth guard on every /api route ---
    if cfg.require_auth {
        let policy = AuthPolicy {
            require_auth: true,
            allowed_roles: Arc::new(cfg.allowed_roles.clone()),
        };
        api = api.layer(axum::middleware::from_fn_with_state(policy, guard));
    }

    // --- Assemble: /api under {base}/api, static + SPA under {base} ---
    let mut router: Router<AdminState> = Router::new()
        .nest("/api", api)
        .route("/static/*path", get(static_asset))
        .route("/", get(spa_shell))
        .route("/*rest", get(spa_shell));

    // Nest under the configured base path (root mount when base is empty).
    router = if base.is_empty() {
        router
    } else {
        // axum's `nest` matches `{base}` and `{base}/{rest}` but NOT the bare
        // `{base}/` with an empty rest — which is exactly the canonical URL
        // `base_href()` injects (`/admin/`). Serve the SPA shell there too so a
        // browser opening `http://host/admin/` gets the dashboard, not a 404.
        Router::new()
            .route(&format!("{base}/"), get(spa_shell))
            .nest(&base, router)
    };

    router.with_state(state)
}

// ── JSON handlers ───────────────────────────────────────────────────────────

async fn overview(State(st): State<AdminState>) -> Json<Value> {
    Json(data::overview(&st.deps).await)
}

async fn health(State(st): State<AdminState>) -> Response {
    let (body, down) = data::health(&st.deps.health).await;
    let code = if down {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, Json(body)).into_response()
}

async fn env(State(st): State<AdminState>) -> Json<Value> {
    Json(data::env(&st.deps))
}

async fn config(State(st): State<AdminState>) -> Json<Value> {
    Json(data::config(&st.deps))
}

async fn loggers(State(st): State<AdminState>) -> Json<Value> {
    match &st.deps.loggers {
        Some(state) => Json(state.levels_json()),
        None => Json(json!({
            "levels": firefly_actuator::SPRING_LEVELS,
            "loggers": {},
            "groups": {},
        })),
    }
}

/// POST body for `set_logger`.
#[derive(Deserialize, Default)]
struct LevelBody {
    #[serde(default)]
    level: Option<String>,
}

async fn set_logger(
    State(st): State<AdminState>,
    Path(name): Path<String>,
    body: Option<Json<LevelBody>>,
) -> Response {
    let Some(state) = &st.deps.loggers else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Logger control not available" })),
        )
            .into_response();
    };
    let level = body
        .and_then(|Json(b)| b.level)
        .unwrap_or_else(|| "INFO".into());
    match state.set_level(&name, Some(&level)) {
        Ok(()) => {
            Json(json!({ "logger": name, "configuredLevel": level.to_uppercase() })).into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn metrics(State(st): State<AdminState>) -> Json<Value> {
    Json(data::metric_names(&st.deps.metrics))
}

async fn metric_detail(State(st): State<AdminState>, Path(name): Path<String>) -> Json<Value> {
    Json(data::metric_detail(&st.deps.metrics, &name))
}

async fn scheduled(State(st): State<AdminState>) -> Json<Value> {
    Json(data::scheduled(st.deps.scheduler.as_ref()))
}

async fn mappings(State(_st): State<AdminState>) -> Json<Value> {
    Json(data::mappings())
}

async fn caches(State(st): State<AdminState>) -> Json<Value> {
    Json(data::caches(&st.deps))
}

async fn cache_keys(State(st): State<AdminState>) -> Json<Value> {
    let body = data::caches(&st.deps);
    Json(json!({ "keys": body.get("keys").cloned().unwrap_or_else(|| json!([])) }))
}

async fn cache_evict(State(st): State<AdminState>, Path(name): Path<String>) -> Response {
    match &st.deps.cache {
        None => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "No cache adapter available" })),
        )
            .into_response(),
        Some(cache) => {
            if cache.evict(&name).await {
                Json(json!({ "evicted": true, "cache": name })).into_response()
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("Cache not found: {name}") })),
                )
                    .into_response()
            }
        }
    }
}

async fn cqrs(State(st): State<AdminState>) -> Json<Value> {
    Json(data::cqrs(st.deps.bus.as_ref()))
}

async fn beans(State(st): State<AdminState>) -> Json<Value> {
    Json(data::beans(st.deps.container.as_ref()))
}

async fn bean_graph(State(st): State<AdminState>) -> Json<Value> {
    Json(data::bean_graph(st.deps.container.as_ref()))
}

async fn bean_detail(State(st): State<AdminState>, Path(name): Path<String>) -> Response {
    match data::bean_detail(st.deps.container.as_ref(), &name) {
        Some(body) => Json(body).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Bean not found" })),
        )
            .into_response(),
    }
}

async fn transactions(State(st): State<AdminState>) -> Json<Value> {
    Json(data::transactions(st.deps.orchestration.as_ref()))
}

/// Query string for `traces` (`?limit=N`, default 100).
#[derive(Deserialize)]
struct TracesQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

async fn traces(State(st): State<AdminState>, Query(q): Query<TracesQuery>) -> Json<Value> {
    Json(st.deps.traces.traces_json(q.limit))
}

async fn logfile(State(st): State<AdminState>) -> Json<Value> {
    Json(st.deps.logs.logfile_json())
}

async fn logfile_clear(State(st): State<AdminState>) -> Json<Value> {
    st.deps.logs.clear();
    Json(json!({ "cleared": true }))
}

async fn runtime(State(_st): State<AdminState>) -> Json<Value> {
    Json(data::runtime())
}

async fn server(State(_st): State<AdminState>) -> Json<Value> {
    Json(data::server())
}

async fn views(State(st): State<AdminState>) -> Json<Value> {
    let views: Vec<Value> = st
        .deps
        .views
        .iter()
        .map(|v| json!({ "id": v.view_id(), "name": v.display_name(), "icon": v.icon() }))
        .collect();
    Json(json!({ "views": views }))
}

async fn view_detail(State(st): State<AdminState>, Path(id): Path<String>) -> Response {
    match st.deps.views.iter().find(|v| v.view_id() == id) {
        Some(view) => Json(view.data().await).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "View not found" })),
        )
            .into_response(),
    }
}

async fn settings(State(st): State<AdminState>) -> Json<Value> {
    Json(data::settings(&st.deps, &st.cfg))
}

// ── Instance registry handlers (server mode) ────────────────────────────────

async fn instances_list(State(st): State<AdminState>) -> Response {
    match &st.deps.instances {
        Some(reg) => Json(reg.to_json()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// POST body for instance registration.
#[derive(Deserialize, Default)]
struct RegisterBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    metadata: std::collections::BTreeMap<String, String>,
}

async fn instances_register(
    State(st): State<AdminState>,
    body: Option<Json<RegisterBody>>,
) -> Response {
    let Some(reg) = &st.deps.instances else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Json(body) = body.unwrap_or_default();
    if body.name.is_empty() || body.url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Both 'name' and 'url' are required" })),
        )
            .into_response();
    }
    let info = reg.register(body.name, body.url, body.metadata);
    (StatusCode::CREATED, Json(info.to_json())).into_response()
}

async fn instances_deregister(State(st): State<AdminState>, Path(name): Path<String>) -> Response {
    let Some(reg) = &st.deps.instances else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if reg.deregister(&name) {
        Json(json!({ "removed": name })).into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Instance not found" })),
        )
            .into_response()
    }
}

// ── SSE handlers ────────────────────────────────────────────────────────────

async fn sse_beans(State(st): State<AdminState>) -> Response {
    sse::beans_stream(Arc::clone(&st.deps)).into_response()
}

async fn sse_health(State(st): State<AdminState>) -> Response {
    sse::health_stream(Arc::clone(&st.deps), st.cfg.refresh_interval).into_response()
}

async fn sse_metrics(State(st): State<AdminState>) -> Response {
    sse::metrics_stream(Arc::clone(&st.deps), st.cfg.refresh_interval).into_response()
}

async fn sse_traces(State(st): State<AdminState>) -> Response {
    sse::traces_stream(Arc::clone(&st.deps)).into_response()
}

async fn sse_logfile(State(st): State<AdminState>) -> Response {
    sse::logfile_stream(Arc::clone(&st.deps)).into_response()
}

async fn sse_runtime(State(st): State<AdminState>) -> Response {
    sse::runtime_stream(st.cfg.refresh_interval).into_response()
}

async fn sse_server(State(st): State<AdminState>) -> Response {
    sse::server_stream(st.cfg.refresh_interval).into_response()
}

// ── Static assets + SPA shell ───────────────────────────────────────────────

async fn static_asset(Path(path): Path<String>) -> Response {
    match ASSETS.get_file(&path) {
        Some(file) => {
            let mime = mime_for(&path);
            (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                file.contents().to_vec(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serves `index.html` with a `<base href>` injected so the SPA's relative
/// asset URLs resolve, version-stamping the local asset references and
/// disabling caching of the shell (pyfly's `_handle_spa`).
async fn spa_shell(State(st): State<AdminState>) -> Response {
    let Some(file) = ASSETS.get_file("index.html") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let html = String::from_utf8_lossy(file.contents());
    let base_href = st.cfg.base_href();
    let with_base = html.replacen(
        "<head>",
        &format!("<head>\n    <base href=\"{base_href}\">"),
        1,
    );
    let version = crate::VERSION;
    // Version-stamp local static assets so an upgrade busts stale caches.
    let stamped = with_base
        .replace(
            "href=\"static/",
            &format!("data-v=\"{version}\" href=\"static/"),
        )
        .replace(
            "src=\"static/",
            &format!("data-v=\"{version}\" src=\"static/"),
        );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        stamped,
    )
        .into_response()
}

/// A coarse content-type guess from a static asset's extension.
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_embed_includes_index_and_js() {
        assert!(ASSETS.get_file("index.html").is_some());
        assert!(ASSETS.get_file("js/app.js").is_some());
        assert!(ASSETS.get_file("css/admin.css").is_some());
    }

    #[test]
    fn mime_mapping() {
        assert_eq!(
            mime_for("js/app.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for("css/admin.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("assets/logo.png"), "image/png");
    }
}
