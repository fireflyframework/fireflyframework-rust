//! In-process HTTP tests for the `/actuator/*` surface, ported 1:1 from
//! the Go module's `actuator_test.go` (plus Rust-specific cases), driven
//! through `tower::ServiceExt::oneshot` — no sockets.

use std::sync::Arc;

use axum::body::Body;
use axum::Router;
use firefly_actuator::{
    mount, ActuatorConfig, HealthComposite, HealthResult, IndicatorFn, MetricRegistry, VERSION,
};
use http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// Sends a GET request through the router and returns status, headers,
/// and the collected body bytes.
async fn get(app: Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, body.to_vec())
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
    let (status, _, body) = get(app, uri).await;
    (status, serde_json::from_slice(&body).unwrap())
}

// ----- Go: TestHealthOK -----

#[tokio::test]
async fn health_ok() {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("db", || async { HealthResult::up() }));
    let app = mount(ActuatorConfig {
        health,
        app_name: "orders".into(),
        ..ActuatorConfig::default()
    });

    let (status, body) = get_json(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
    assert_eq!(body["details"]["db"]["status"], "UP");
}

// ----- Go: TestHealthDown503 -----

#[tokio::test]
async fn health_down_503() {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("broker", || async {
        HealthResult::down("disconnected")
    }));
    let app = mount(ActuatorConfig {
        health,
        ..ActuatorConfig::default()
    });

    let (status, body) = get_json(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"], "DOWN");
    assert_eq!(body["details"]["broker"]["message"], "disconnected");
}

// ----- Rust extra: DEGRADED maps to 200 -----

#[tokio::test]
async fn health_degraded_is_200() {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("cache", || async {
        HealthResult::degraded("evicting")
    }));
    let app = mount(ActuatorConfig {
        health,
        ..ActuatorConfig::default()
    });

    let (status, body) = get_json(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "DEGRADED");
}

// ----- Rust extra: empty composite is UP with empty details -----

#[tokio::test]
async fn health_empty_composite_is_up() {
    let app = mount(ActuatorConfig::default());

    let (status, body) = get_json(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "UP");
    assert_eq!(body["details"], serde_json::json!({}));
}

// ----- Go: TestInfoIncludesContributors -----

#[tokio::test]
async fn info_includes_contributors() {
    let app = mount(ActuatorConfig {
        app_name: "orders".into(),
        app_version: "1.2.3".into(),
        info_contributors: vec![Box::new(|| {
            let mut m = serde_json::Map::new();
            m.insert("git".into(), serde_json::json!({ "sha": "abc" }));
            m
        })],
        ..ActuatorConfig::default()
    });

    let (status, body) = get_json(app, "/actuator/info").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["name"], "orders");
    assert_eq!(body["app"]["version"], "1.2.3");
    assert_eq!(body["git"]["sha"], "abc");
    // Runtime block is always present.
    assert!(body["runtime"]["numCPU"].as_u64().unwrap() >= 1);
    assert!(body["runtime"]["os"].is_string());
}

// ----- Go: TestEnvRedaction -----

#[tokio::test]
async fn env_redaction() {
    std::env::set_var("FIREFLY_WEB_PORT", "8080");
    std::env::set_var("DATABASE_PASSWORD", "supersecret");

    let app = mount(ActuatorConfig::default());

    let (status, body) = get_json(app, "/actuator/env").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["FIREFLY_WEB_PORT"], "8080",
        "FIREFLY_ should pass through"
    );
    assert_eq!(
        body["DATABASE_PASSWORD"], "***",
        "non-allow-list should redact"
    );
}

// ----- Rust extra: custom allow prefixes replace the default -----

#[tokio::test]
async fn env_custom_allow_prefixes() {
    std::env::set_var("MYAPP_ACTUATOR_FLAG", "on");
    std::env::set_var("FIREFLY_ACTUATOR_SECRET", "s3cret");

    let app = mount(ActuatorConfig {
        env_allow_prefixes: vec!["MYAPP_".into()],
        ..ActuatorConfig::default()
    });

    let (_, body) = get_json(app, "/actuator/env").await;
    assert_eq!(body["MYAPP_ACTUATOR_FLAG"], "on");
    assert_eq!(body["FIREFLY_ACTUATOR_SECRET"], "***");
}

// ----- Go: TestMetricsPrometheus -----

#[tokio::test]
async fn metrics_prometheus() {
    let registry = Arc::new(MetricRegistry::new());
    registry.counter("orders_placed_total").inc();
    registry.counter("orders_placed_total").add(2);
    registry.gauge("queue_depth").set(42.5);

    let app = mount(ActuatorConfig {
        metric_registry: registry,
        ..ActuatorConfig::default()
    });

    let (status, headers, body) = get(app, "/actuator/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/plain; version=0.0.4"
    );
    let out = String::from_utf8(body).unwrap();
    assert!(
        out.contains("# TYPE orders_placed_total counter"),
        "output: {out}"
    );
    assert!(out.contains("orders_placed_total 3"), "counter: {out}");
    assert!(out.contains("queue_depth 42.5"), "gauge: {out}");
    // Byte-for-byte the Go %f rendering.
    assert!(out.contains("queue_depth 42.500000\n"), "gauge fmt: {out}");
}

// ----- Go: TestGoroutinesEndpoint (adapted to tokio tasks) -----

#[tokio::test]
async fn tasks_endpoint() {
    // Keep one task alive so the count is provably non-zero, the way
    // Go's test relies on the runtime's own goroutines.
    let keepalive = tokio::spawn(std::future::pending::<()>());

    let app = mount(ActuatorConfig::default());
    let (status, body) = get_json(app, "/actuator/tasks").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["count"].as_u64().unwrap() >= 1, "count: {body}");

    keepalive.abort();
}

// ----- Go README: dump variant (adapted: runtime report, not stacks) -----

#[tokio::test]
async fn tasks_dump_endpoint() {
    let app = mount(ActuatorConfig::default());
    let (status, headers, body) = get(app, "/actuator/tasks?dump=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "text/plain; charset=utf-8"
    );
    let out = String::from_utf8(body).unwrap();
    assert!(out.contains("=== tokio runtime ==="), "dump: {out}");
    assert!(out.contains("workers:"), "dump: {out}");
    assert!(out.contains("alive_tasks:"), "dump: {out}");
}

// ----- Go: TestVersionEndpoint -----

#[tokio::test]
async fn version_endpoint() {
    let app = mount(ActuatorConfig {
        app_name: "orders".into(),
        app_version: "9.9.9".into(),
        ..ActuatorConfig::default()
    });

    let (status, body) = get_json(app, "/actuator/version").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"], "orders");
    assert_eq!(body["appVersion"], "9.9.9");
    assert_eq!(body["firefly"], VERSION);
    assert!(body["rust"].is_string());
    // buildTime is RFC 3339, seconds precision, like Go's time.RFC3339.
    let build_time = body["buildTime"].as_str().unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(build_time).is_ok());
}

// ----- Rust extra: app_version defaults to the framework VERSION -----

#[tokio::test]
async fn version_defaults_to_framework_version() {
    let app = mount(ActuatorConfig::default());
    let (_, body) = get_json(app, "/actuator/version").await;
    assert_eq!(body["appVersion"], VERSION);
}

// ----- Rust extra: routing behavior -----

#[tokio::test]
async fn unknown_route_is_404() {
    let app = mount(ActuatorConfig::default());
    let (status, _, _) = get(app, "/actuator/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn post_to_health_is_405() {
    let app = mount(ActuatorConfig::default());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/actuator/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// ----- Regression: JSON bodies carry the trailing newline Go emits -----
//
// Go renders every actuator JSON body with `json.NewEncoder(w).Encode(v)`,
// which always appends a `\n`. The wire format must match byte-for-byte,
// so every JSON endpoint here must end with exactly one newline too.

#[tokio::test]
async fn json_bodies_end_with_single_trailing_newline_like_go() {
    for uri in [
        "/actuator/health",
        "/actuator/info",
        "/actuator/env",
        "/actuator/tasks",
        "/actuator/version",
    ] {
        let app = mount(ActuatorConfig::default());
        let (status, headers, body) = get(app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json",
            "{uri}"
        );
        let out = String::from_utf8(body).unwrap();
        assert!(
            out.ends_with('\n'),
            "{uri} body must end with the newline Go's json.Encoder appends: {out:?}"
        );
        assert!(
            !out.ends_with("\n\n"),
            "{uri} body must end with exactly one newline: {out:?}"
        );
        // The newline is trailing whitespace only — the payload still parses.
        serde_json::from_str::<Value>(&out).unwrap();
    }
}

// Health DOWN takes a different write path (503); it must carry the same
// trailing newline.
#[tokio::test]
async fn health_down_body_ends_with_trailing_newline() {
    let health = Arc::new(HealthComposite::new());
    health.add(IndicatorFn::new("broker", || async {
        HealthResult::down("disconnected")
    }));
    let app = mount(ActuatorConfig {
        health,
        ..ActuatorConfig::default()
    });

    let (status, _, body) = get(app, "/actuator/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body.last(), Some(&b'\n'), "503 body must also end with \\n");
}

// ----- Rust extra: health responses carry application/json -----

#[tokio::test]
async fn health_content_type_is_json() {
    let app = mount(ActuatorConfig::default());
    let (_, headers, _) = get(app, "/actuator/health").await;
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
}
