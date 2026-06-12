//! In-process HTTP tests for the Spring-Cloud-Config-compatible
//! endpoint — the port of the Go `server_test.go`, plus Rust-specific
//! wire-shape assertions. Requests are driven through the axum router
//! with `tower::ServiceExt::oneshot`; no sockets are opened.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use firefly_config_server::{
    router, ConfigServerError, Environment, MemoryStore, PropertySource, Store,
};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A store seeded exactly like the Go TestConfigServerLookup fixture.
fn seeded_router() -> Router {
    let store = Arc::new(MemoryStore::new());
    store.put(
        "orders",
        "prod",
        "main",
        Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            property_sources: vec![PropertySource {
                name: "default".into(),
                source: [("db.url".to_string(), "x".into())].into_iter().collect(),
            }],
            ..Environment::default()
        },
    );
    router(store)
}

/// Drives one GET through the router and returns (status, content-type, body).
async fn get(app: Router, path: &str) -> (StatusCode, String, String) {
    let res = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

// Port of Go TestConfigServerLookup.
#[tokio::test]
async fn config_server_lookup() {
    let (status, content_type, body) = get(seeded_router(), "/orders/prod/main").await;
    assert_eq!(status, StatusCode::OK, "status {status}");
    assert_eq!(content_type, "application/json");
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(
        env.property_sources[0].source["db.url"],
        serde_json::json!("x"),
        "env: {env:?}"
    );
}

// Port of Go TestConfigServerSoftMiss.
#[tokio::test]
async fn config_server_soft_miss() {
    let app = router(Arc::new(MemoryStore::new()));
    let (status, _, body) = get(app, "/missing/dev").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "expected soft-miss 200, got {status}"
    );
    // Exact wire shape of the soft miss, byte-for-byte with the Go encoder.
    assert_eq!(
        body,
        "{\"name\":\"missing\",\"profiles\":[\"dev\"],\"label\":\"main\",\"propertySources\":[]}\n"
    );
}

#[tokio::test]
async fn exact_wire_shape_matches_go_encoder() {
    let (_, _, body) = get(seeded_router(), "/orders/prod/main").await;
    assert_eq!(
        body,
        "{\"name\":\"orders\",\"profiles\":[\"prod\"],\"label\":\"main\",\"propertySources\":[{\"name\":\"default\",\"source\":{\"db.url\":\"x\"}}]}\n"
    );
}

#[tokio::test]
async fn label_defaults_to_main() {
    // Two-segment path → label "main" → hits the seeded entry.
    let (status, _, body) = get(seeded_router(), "/orders/prod").await;
    assert_eq!(status, StatusCode::OK);
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(env.label, "main");
    assert_eq!(env.property_sources.len(), 1);
}

#[tokio::test]
async fn extra_path_segments_are_ignored() {
    // Go splits the path and reads at most three segments.
    let (status, _, body) = get(seeded_router(), "/orders/prod/main/extra/junk").await;
    assert_eq!(status, StatusCode::OK);
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(env.label, "main");
    assert_eq!(env.property_sources.len(), 1);
}

#[tokio::test]
async fn short_path_returns_400_with_go_message() {
    for path in ["/", "/only-one", "/only-one/"] {
        let (status, content_type, body) = get(seeded_router(), path).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "path {path}: status {status}"
        );
        assert_eq!(body, "expect /{app}/{profile}[/{label}]\n", "path {path}");
        assert_eq!(content_type, "text/plain; charset=utf-8");
    }
}

#[tokio::test]
async fn version_and_state_appear_on_the_wire_when_set() {
    let store = Arc::new(MemoryStore::new());
    store.put(
        "orders",
        "prod",
        "main",
        Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            version: "abc123".into(),
            state: "ok".into(),
            ..Environment::default()
        },
    );
    let (_, _, body) = get(router(store), "/orders/prod/main").await;
    assert_eq!(
        body,
        "{\"name\":\"orders\",\"profiles\":[\"prod\"],\"label\":\"main\",\"version\":\"abc123\",\"state\":\"ok\",\"propertySources\":[]}\n"
    );
}

#[tokio::test]
async fn source_map_keys_encode_sorted_like_go() {
    let store = Arc::new(MemoryStore::new());
    store.put(
        "orders",
        "prod",
        "main",
        Environment {
            name: "orders".into(),
            profiles: vec!["prod".into()],
            label: "main".into(),
            property_sources: vec![PropertySource {
                name: "default".into(),
                // Inserted out of order on purpose.
                source: [
                    ("z.last".to_string(), "1".into()),
                    ("a.first".to_string(), "2".into()),
                ]
                .into_iter()
                .collect(),
            }],
            ..Environment::default()
        },
    );
    let (_, _, body) = get(router(store), "/orders/prod/main").await;
    assert_eq!(
        body,
        "{\"name\":\"orders\",\"profiles\":[\"prod\"],\"label\":\"main\",\"propertySources\":[{\"name\":\"default\",\"source\":{\"a.first\":\"2\",\"z.last\":\"1\"}}]}\n"
    );
}

#[tokio::test]
async fn any_http_method_is_served() {
    // Go's http.HandlerFunc never inspects the method; neither do we.
    let res = seeded_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/orders/prod/main")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn percent_encoded_segments_are_decoded() {
    let store = Arc::new(MemoryStore::new());
    store.put(
        "my app",
        "dev",
        "main",
        Environment {
            name: "my app".into(),
            profiles: vec!["dev".into()],
            label: "main".into(),
            property_sources: vec![PropertySource::default()],
            ..Environment::default()
        },
    );
    let (status, _, body) = get(router(store), "/my%20app/dev").await;
    assert_eq!(status, StatusCode::OK);
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(env.name, "my app");
    assert_eq!(env.property_sources.len(), 1);
}

// Regression: an encoded slash must split segments, exactly like the
// Go reference (configserver/server.go), where net/http hands the
// handler a percent-decoded r.URL.Path and the *decoded* path is
// split. GET /my%2Fapp/dev is (app="my", profile="app", label="dev"),
// not ("my/app", "dev", "main") as the old raw-path split produced.
#[tokio::test]
async fn encoded_slash_splits_segments_like_go() {
    let store = Arc::new(MemoryStore::new());
    store.put(
        "my",
        "app",
        "dev",
        Environment {
            name: "my".into(),
            profiles: vec!["app".into()],
            label: "dev".into(),
            property_sources: vec![PropertySource::default()],
            ..Environment::default()
        },
    );
    // The tuple the buggy raw-path split used to resolve; must NOT be hit.
    store.put(
        "my/app",
        "dev",
        "main",
        Environment {
            name: "wrong".into(),
            ..Environment::default()
        },
    );
    let (status, _, body) = get(router(store), "/my%2Fapp/dev").await;
    assert_eq!(status, StatusCode::OK);
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(env.name, "my");
    assert_eq!(env.profiles, vec!["app".to_string()]);
    assert_eq!(env.label, "dev");
    assert_eq!(env.property_sources.len(), 1, "env: {env:?}");
}

// Regression: an invalid percent-escape in the path is a 400, like
// Go's net/http rejecting the request line before the handler runs
// (body "400 Bad Request", no trailing newline) — not a 200 soft miss.
#[tokio::test]
async fn invalid_percent_escape_returns_400() {
    for path in ["/bad%zz/dev", "/trailing%2/dev", "/app/dev%"] {
        let (status, content_type, body) = get(seeded_router(), path).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "path {path}: status {status}"
        );
        assert_eq!(body, "400 Bad Request", "path {path}");
        assert_eq!(content_type, "text/plain; charset=utf-8", "path {path}");
    }
}

#[tokio::test]
async fn seeding_after_router_build_is_visible() {
    // The store stays shared through the Arc, like Go's *MemoryStore.
    let store = Arc::new(MemoryStore::new());
    let app = router(Arc::clone(&store) as Arc<dyn Store>);
    store.put(
        "late",
        "dev",
        "main",
        Environment {
            name: "late".into(),
            profiles: vec!["dev".into()],
            label: "main".into(),
            property_sources: vec![PropertySource::default()],
            ..Environment::default()
        },
    );
    let (status, _, body) = get(app, "/late/dev").await;
    assert_eq!(status, StatusCode::OK);
    let env: Environment = serde_json::from_str(&body).unwrap();
    assert_eq!(env.property_sources.len(), 1);
}

/// A store that always fails, to exercise the 500 path.
struct FailingStore;

#[async_trait]
impl Store for FailingStore {
    async fn lookup(
        &self,
        _app: &str,
        _profile: &str,
        _label: &str,
    ) -> Result<Environment, ConfigServerError> {
        Err(ConfigServerError::Store("backend unavailable".into()))
    }
}

#[tokio::test]
async fn store_error_returns_500_with_error_text() {
    let (status, content_type, body) = get(router(Arc::new(FailingStore)), "/orders/prod").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "backend unavailable\n");
    assert_eq!(content_type, "text/plain; charset=utf-8");
}
