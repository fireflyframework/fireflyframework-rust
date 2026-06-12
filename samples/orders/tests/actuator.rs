//! Port of the Go sample's `web/actuator_test.go` — the starter-core
//! lifecycle/actuator wiring exposes the management endpoints alongside
//! the public API. Driven in-process via `tower::ServiceExt::oneshot`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use firefly_starter_core::{Core, CoreConfig};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// Go: TestActuatorMounted. Where Go counts goroutines on
/// `/actuator/goroutines`, the Rust actuator counts alive tokio tasks
/// on `/actuator/tasks`; `/actuator/env` is part of the same surface.
#[tokio::test]
async fn actuator_mounted() {
    let core = Core::new(CoreConfig {
        app_name: "orders-test".into(),
        ..CoreConfig::default()
    });
    let admin = core.actuator_router(Vec::new());

    for path in [
        "/actuator/health",
        "/actuator/info",
        "/actuator/metrics",
        "/actuator/version",
        "/actuator/env",
        "/actuator/tasks",
    ] {
        let res = admin
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "{path} status: {}",
            res.status()
        );
    }
}

/// Go: TestActuatorVersionContainsAppMeta.
#[tokio::test]
async fn actuator_version_contains_app_meta() {
    let core = Core::new(CoreConfig {
        app_name: "orders-test".into(),
        app_version: "1.2.3".into(),
        ..CoreConfig::default()
    });
    let admin = core.actuator_router(Vec::new());

    let res = admin
        .oneshot(
            Request::get("/actuator/version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let out = String::from_utf8_lossy(&bytes);
    for want in ["orders-test", "1.2.3", "firefly"] {
        assert!(out.contains(want), "missing {want:?} in {out}");
    }
}

/// The sample main()'s info contributor shape flows onto
/// `/actuator/info`, like Go's `core.ActuatorHandler(func() map[string]any …)`.
#[tokio::test]
async fn actuator_info_carries_sample_contributor() {
    let core = Core::new(CoreConfig {
        app_name: "orders-sample".into(),
        ..CoreConfig::default()
    });
    let contributor: firefly_starter_core::InfoContributor = Box::new(|| {
        let mut info = serde_json::Map::new();
        info.insert(
            "sample".into(),
            serde_json::json!({ "orders": "in-memory" }),
        );
        info
    });
    let admin = core.actuator_router(vec![contributor]);

    let res = admin
        .oneshot(Request::get("/actuator/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info["app"]["name"], "orders-sample");
    assert_eq!(info["sample"]["orders"], "in-memory");
}
