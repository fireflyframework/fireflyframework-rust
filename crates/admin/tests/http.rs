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

//! In-process HTTP integration tests for the admin router — the Rust
//! rendering of pyfly's `test_admin_api` / `test_wave_admin_auth` suites.
//!
//! Every test drives the router via `tower::ServiceExt::oneshot` (no real
//! socket); the SSE-framing test binds an ephemeral port (`127.0.0.1:0`) so
//! it can read the first streamed frame off the wire.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use firefly_actuator::{HealthComposite, HealthResult, IndicatorFn, MetricRegistry};
use firefly_admin::{
    mount, AdminConfig, AdminDeps, AdminView, InstanceRegistry, LogBuffer, TraceBuffer,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

fn base_deps() -> AdminDeps {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
    let metrics = Arc::new(MetricRegistry::new());
    metrics.counter("orders_total").inc();
    AdminDeps::new(
        "orders",
        "1.0.0",
        health,
        metrics,
        Arc::new(TraceBuffer::new()),
        LogBuffer::new(),
    )
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn status_of(router: &axum::Router, method: &str, path: &str) -> StatusCode {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

// ── Route presence + payload shape ──────────────────────────────────────────

#[tokio::test]
async fn overview_route_returns_app_block() {
    let router = mount(AdminConfig::default(), base_deps());
    let (status, body) = get(&router, "/admin/api/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["name"], "orders");
    assert_eq!(body["app"]["version"], "1.0.0");
}

#[tokio::test]
async fn health_route_is_200_when_up() {
    let router = mount(AdminConfig::default(), base_deps());
    let (status, body) = get(&router, "/admin/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
}

#[tokio::test]
async fn health_route_is_503_when_down() {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("db", || async {
        HealthResult::down("offline")
    }));
    let deps = AdminDeps::new(
        "orders",
        "1.0.0",
        health,
        Arc::new(MetricRegistry::new()),
        Arc::new(TraceBuffer::new()),
        LogBuffer::new(),
    );
    let router = mount(AdminConfig::default(), deps);
    let (status, body) = get(&router, "/admin/api/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");
}

#[tokio::test]
async fn full_api_route_set_is_present() {
    let router = mount(AdminConfig::default(), base_deps());
    for path in [
        "/admin/api/overview",
        "/admin/api/health",
        "/admin/api/env",
        "/admin/api/config",
        "/admin/api/loggers",
        "/admin/api/metrics",
        "/admin/api/scheduled",
        "/admin/api/mappings",
        "/admin/api/caches",
        "/admin/api/caches/keys",
        "/admin/api/cqrs",
        "/admin/api/transactions",
        "/admin/api/traces",
        "/admin/api/logfile",
        "/admin/api/runtime",
        "/admin/api/server",
        "/admin/api/views",
        "/admin/api/settings",
    ] {
        let status = status_of(&router, "GET", path).await;
        assert_eq!(status, StatusCode::OK, "GET {path}");
    }
}

#[tokio::test]
async fn metrics_route_lists_registered_meters() {
    let router = mount(AdminConfig::default(), base_deps());
    let (status, body) = get(&router, "/admin/api/metrics").await;
    assert_eq!(status, StatusCode::OK);
    let names = body["names"].as_array().unwrap();
    assert!(names.iter().any(|n| n == "orders_total"));
}

#[tokio::test]
async fn settings_reports_title_and_server_mode() {
    let router = mount(AdminConfig::default(), base_deps());
    let (_status, body) = get(&router, "/admin/api/settings").await;
    assert_eq!(body["title"], "Firefly Admin");
    assert_eq!(body["serverMode"], false);
}

#[tokio::test]
async fn logfile_route_and_clear() {
    let logs = LogBuffer::new();
    logs.push(firefly_admin::LogRecord {
        id: 0,
        timestamp: "2026-06-12T00:00:00Z".into(),
        level: "INFO".into(),
        logger: "test".into(),
        message: "hi".into(),
        context: String::new(),
        thread: None,
    });
    let deps = AdminDeps {
        logs: logs.clone(),
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);
    let (status, body) = get(&router, "/admin/api/logfile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);

    let clear = status_of(&router, "POST", "/admin/api/logfile/clear").await;
    assert_eq!(clear, StatusCode::OK);
    let (_s, after) = get(&router, "/admin/api/logfile").await;
    assert_eq!(after["total"], 0);
}

// ── Custom views ────────────────────────────────────────────────────────────

struct FlagsView;

#[async_trait::async_trait]
impl AdminView for FlagsView {
    fn view_id(&self) -> &str {
        "flags"
    }
    fn display_name(&self) -> &str {
        "Feature Flags"
    }
    fn icon(&self) -> &str {
        "flag"
    }
    async fn data(&self) -> Value {
        serde_json::json!({ "enabled": ["beta"] })
    }
}

#[tokio::test]
async fn custom_view_listed_and_served() {
    let deps = AdminDeps {
        views: vec![Arc::new(FlagsView)],
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);

    let (_s, list) = get(&router, "/admin/api/views").await;
    assert_eq!(list["views"][0]["id"], "flags");
    assert_eq!(list["views"][0]["name"], "Feature Flags");

    let (status, detail) = get(&router, "/admin/api/views/flags").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["enabled"][0], "beta");

    let missing = status_of(&router, "GET", "/admin/api/views/missing").await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
}

// ── SPA shell + static assets ───────────────────────────────────────────────

#[tokio::test]
async fn spa_shell_serves_html_with_base_href() {
    let router = mount(AdminConfig::default(), base_deps());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(cache, "no-cache");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&bytes);
    assert!(
        html.contains("<base href=\"/admin/\">"),
        "base href injected"
    );
}

#[tokio::test]
async fn static_asset_served() {
    let router = mount(AdminConfig::default(), base_deps());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/static/js/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("application/javascript"), "{ct}");
}

// ── Auth guard (pyfly #66) ──────────────────────────────────────────────────

#[tokio::test]
async fn api_open_when_auth_disabled() {
    let router = mount(AdminConfig::default(), base_deps());
    let status = status_of(&router, "GET", "/admin/api/overview").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn api_blocked_when_auth_required_and_anonymous() {
    let cfg = AdminConfig {
        require_auth: true,
        ..AdminConfig::default()
    };
    let router = mount(cfg, base_deps());
    assert_eq!(
        status_of(&router, "GET", "/admin/api/overview").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&router, "GET", "/admin/api/settings").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn spa_shell_public_even_with_auth() {
    let cfg = AdminConfig {
        require_auth: true,
        ..AdminConfig::default()
    };
    let router = mount(cfg, base_deps());
    // The SPA shell must boot regardless of the API guard.
    assert_ne!(
        status_of(&router, "GET", "/admin").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn authenticated_admin_passes_guard() {
    let cfg = AdminConfig {
        require_auth: true,
        allowed_roles: vec!["ADMIN".into()],
        ..AdminConfig::default()
    };
    let router = mount(cfg, base_deps());
    let auth = firefly_security::Authentication {
        principal: "u1".into(),
        username: "alice".into(),
        roles: vec!["ADMIN".into()],
        ..Default::default()
    };
    let mut req = Request::builder()
        .uri("/admin/api/overview")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(auth);
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn authenticated_wrong_role_is_403() {
    let cfg = AdminConfig {
        require_auth: true,
        allowed_roles: vec!["ADMIN".into()],
        ..AdminConfig::default()
    };
    let router = mount(cfg, base_deps());
    let auth = firefly_security::Authentication {
        principal: "u2".into(),
        username: "bob".into(),
        roles: vec!["USER".into()],
        ..Default::default()
    };
    let mut req = Request::builder()
        .uri("/admin/api/overview")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(auth);
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ── Server mode: instance registry routes ───────────────────────────────────

#[tokio::test]
async fn instances_routes_only_in_server_mode() {
    // Without server mode the instances route is absent: the request falls
    // through to the SPA shell (HTML), not the instances JSON.
    let router = mount(AdminConfig::default(), base_deps());
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/api/instances")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.starts_with("text/html"), "SPA fallthrough, got {ct}");

    // With an instance registry wired, GET lists and POST registers.
    let registry = Arc::new(InstanceRegistry::new());
    let deps = AdminDeps {
        instances: Some(Arc::clone(&registry)),
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);
    let (status, body) = get(&router, "/admin/api/instances").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["instances"].as_array().unwrap().len(), 0);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "name": "svc-a", "url": "http://a:8080" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(registry.len(), 1);

    let removed = status_of(&router, "DELETE", "/admin/api/instances/svc-a").await;
    assert_eq!(removed, StatusCode::OK);
    assert!(registry.is_empty());

    let missing = status_of(&router, "DELETE", "/admin/api/instances/nope").await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn instances_register_rejects_missing_fields() {
    let registry = Arc::new(InstanceRegistry::new());
    let deps = AdminDeps {
        instances: Some(registry),
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({ "name": "x" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── Loggers mutation ────────────────────────────────────────────────────────

#[tokio::test]
async fn set_logger_level_via_http() {
    use firefly_actuator::LoggersState;
    let loggers = Arc::new(LoggersState::with_reload_fn(|_| Ok(()), "info"));
    let deps = AdminDeps {
        loggers: Some(Arc::clone(&loggers)),
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api/loggers/my_crate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "level": "DEBUG" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(loggers.directives().contains("my_crate=debug"));
}

// ── SSE framing (binds an ephemeral port to read the first frame) ───────────

#[tokio::test]
async fn sse_traces_stream_frames_each_trace() {
    let traces = Arc::new(TraceBuffer::new());
    // Pre-seed a trace so the first incremental tick has something to push.
    traces.record(firefly_admin::TraceEntry {
        timestamp: "2026-06-12T00:00:00Z".into(),
        method: "GET".into(),
        path: "/api/users".into(),
        query_string: String::new(),
        status: 200,
        duration_ms: 1.0,
        client_host: None,
        content_type: None,
        user_agent: String::new(),
        content_length: None,
    });
    let deps = AdminDeps {
        traces: Arc::clone(&traces),
        ..base_deps()
    };
    let router = mount(AdminConfig::default(), deps);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    // Read the raw SSE stream until the first `event: trace` frame arrives.
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        let mut resp = reqwest::Client::new()
            .get(format!("http://{addr}/admin/api/sse/traces"))
            .send()
            .await
            .unwrap();
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await.unwrap() {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if buf.contains("event: trace") && buf.contains("/api/users") {
                return buf;
            }
        }
        buf
    })
    .await
    .expect("SSE frame within timeout");

    assert!(frame.contains("event: trace"), "got: {frame}");
    assert!(frame.contains("\"path\":\"/api/users\""), "got: {frame}");
    server.abort();
}

#[tokio::test]
async fn sse_health_stream_emits_initial_frame() {
    let router = mount(AdminConfig::default(), base_deps());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        let mut resp = reqwest::Client::new()
            .get(format!("http://{addr}/admin/api/sse/health"))
            .send()
            .await
            .unwrap();
        let mut buf = String::new();
        while let Some(chunk) = resp.chunk().await.unwrap() {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if buf.contains("event: health") {
                return buf;
            }
        }
        buf
    })
    .await
    .expect("SSE health frame within timeout");

    assert!(frame.contains("event: health"), "got: {frame}");
    assert!(frame.contains("\"status\":\"UP\""), "got: {frame}");
    server.abort();
}
