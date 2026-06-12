//! End-to-end `SessionLayer` tests via `tower::ServiceExt::oneshot`, the
//! Rust analog of pyfly's `TestSessionFilter` cases: new-session cookie
//! issuance, secure-over-HTTPS / `X-Forwarded-Proto`, existing-session
//! load, invalidation delete-cookie, and rotation store+cookie migration.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::routing::get;
use axum::{Extension, Router};
use http::header::{COOKIE, SET_COOKIE};
use http::{Request, StatusCode};
use tower::ServiceExt;

use firefly_session::{MemorySessionStore, Session, SessionConfig, SessionLayer, SessionStore};

fn store() -> Arc<dyn SessionStore> {
    Arc::new(MemorySessionStore::new())
}

/// Reads the single `Set-Cookie` header from a response, if present.
fn set_cookie(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get(SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned)
}

/// Extracts the `PYFLY_SESSION` value from a `Set-Cookie` string.
fn session_id_from_cookie(cookie: &str) -> String {
    cookie
        .split(';')
        .next()
        .and_then(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_string())
        .expect("cookie has a value")
}

#[tokio::test]
async fn new_session_issues_cookie_insecure_over_http() {
    // pyfly: test_new_session_issues_cookie_insecure_over_http
    let store = store();
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.set_attribute("hello", "world").await.unwrap();
                "ok"
            }),
        )
        .layer(SessionLayer::new(store.clone()));

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = set_cookie(&resp).expect("Set-Cookie issued");
    assert!(cookie.starts_with("PYFLY_SESSION="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(!cookie.contains("Secure")); // plain HTTP dev

    // The attribute was persisted under the new id.
    let id = session_id_from_cookie(&cookie);
    let data = store.get(&id).await.unwrap().expect("session saved");
    assert_eq!(data.get("hello").unwrap(), "world");
}

#[tokio::test]
async fn cookie_secure_over_https() {
    // pyfly: test_cookie_secure_over_https
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.set_attribute("x", 1u8).await.unwrap();
                "ok"
            }),
        )
        .layer(SessionLayer::new(store()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("https://example.com/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(set_cookie(&resp).unwrap().contains("Secure"));
}

#[tokio::test]
async fn cookie_secure_via_forwarded_proto() {
    // pyfly: test_cookie_secure_via_forwarded_proto
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.set_attribute("x", 1u8).await.unwrap();
                "ok"
            }),
        )
        .layer(SessionLayer::new(store()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(set_cookie(&resp).unwrap().contains("Secure"));
}

#[tokio::test]
async fn existing_session_is_loaded() {
    // pyfly: test_existing_session_is_loaded
    let store = store();
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_string(), serde_json::Value::from("ada"));
    store
        .save("existing", &data, Duration::from_secs(60))
        .await
        .unwrap();

    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                assert_eq!(session.id().await, "existing");
                assert_eq!(
                    session.attribute::<String>("user").await.as_deref(),
                    Some("ada")
                );
                "ok"
            }),
        )
        .layer(SessionLayer::new(store.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=existing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn read_only_request_reissues_sliding_cookie_without_store_write() {
    // A read of an existing, unmodified session re-issues the cookie
    // (sliding Max-Age) but does not write the store (modified == false).
    let store = store();
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_string(), serde_json::Value::from("ada"));
    store
        .save("existing", &data, Duration::from_secs(60))
        .await
        .unwrap();

    let app = Router::new()
        .route("/", get(|_session: Extension<Session>| async { "ok" }))
        .layer(SessionLayer::new(store.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=existing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let cookie = set_cookie(&resp).expect("sliding cookie reissued");
    assert_eq!(session_id_from_cookie(&cookie), "existing");
    // Still the original data (no spurious overwrite).
    assert_eq!(
        store
            .get("existing")
            .await
            .unwrap()
            .unwrap()
            .get("user")
            .unwrap(),
        "ada"
    );
}

#[tokio::test]
async fn invalidate_deletes_cookie_and_store_entry() {
    // pyfly: test_invalidate_deletes_cookie_and_store_entry
    let store = store();
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_string(), serde_json::Value::from("ada"));
    store
        .save("existing", &data, Duration::from_secs(60))
        .await
        .unwrap();

    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.invalidate().await;
                "bye"
            }),
        )
        .layer(SessionLayer::new(store.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=existing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let cookie = set_cookie(&resp).expect("delete-cookie issued");
    assert!(cookie.contains("Max-Age=0"));
    assert_eq!(store.get("existing").await.unwrap(), None);
}

#[tokio::test]
async fn rotation_migrates_store_and_cookie() {
    // pyfly: test_rotation_migrates_store_and_cookie
    let store = store();
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_string(), serde_json::Value::from("ada"));
    store
        .save("fixed-id", &data, Duration::from_secs(60))
        .await
        .unwrap();

    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.rotate_id().await; // e.g. on login
                "ok"
            }),
        )
        .layer(SessionLayer::new(store.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=fixed-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let cookie = set_cookie(&resp).expect("Set-Cookie issued");
    let new_id = session_id_from_cookie(&cookie);
    assert_ne!(new_id, "fixed-id");
    // Old (fixed) id no longer resolves.
    assert_eq!(store.get("fixed-id").await.unwrap(), None);
    // Data carried to the new id.
    let migrated = store.get(&new_id).await.unwrap().expect("new id present");
    assert_eq!(migrated.get("user").unwrap(), "ada");
}

#[tokio::test]
async fn signed_cookie_roundtrip() {
    // With a signer, the cookie value is signed; a second request carrying
    // that signed cookie resolves to the same session.
    use firefly_session::SessionSigner;

    let store = store();
    let layer = SessionLayer::new(store.clone()).with_signer(SessionSigner::new("secret"));

    let app = Router::new()
        .route(
            "/set",
            get(|session: Extension<Session>| async move {
                session.set_attribute("k", "v").await.unwrap();
                "ok"
            }),
        )
        .route(
            "/get",
            get(|session: Extension<Session>| async move {
                session
                    .attribute::<String>("k")
                    .await
                    .unwrap_or_else(|| "MISSING".to_string())
            }),
        )
        .layer(layer);

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/set").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = set_cookie(&resp).expect("signed cookie issued");
    let cookie_value = cookie.split(';').next().unwrap().to_string();
    // The value is signed: id.signature.
    assert!(cookie_value.contains('.'));

    let resp2 = app
        .oneshot(
            Request::builder()
                .uri("/get")
                .header(COOKIE, &cookie_value)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"v");
}

#[tokio::test]
async fn forged_signed_cookie_starts_fresh_session() {
    // A tampered signed cookie fails verification, so the layer mints a new
    // session rather than honoring the forged id.
    use firefly_session::SessionSigner;

    let store = store();
    let layer = SessionLayer::new(store.clone()).with_signer(SessionSigner::new("secret"));
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                // A genuinely new session for a forged cookie.
                assert!(session.attribute::<String>("k").await.is_none());
                "ok"
            }),
        )
        .layer(layer);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=forged.AAAA")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn absolute_timeout_expires_old_session() {
    // A session older than the absolute timeout is discarded and replaced.
    let store = store();
    let mut data = std::collections::HashMap::new();
    data.insert("user".to_string(), serde_json::Value::from("ada"));
    // _created_at far in the past.
    data.insert("_created_at".to_string(), serde_json::Value::from(0i64));
    store
        .save("old", &data, Duration::from_secs(60))
        .await
        .unwrap();

    let config = SessionConfig {
        absolute_timeout_seconds: Some(1),
        ..SessionConfig::default()
    };
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                // New session: the stale "ada" is gone.
                assert!(session.attribute::<String>("user").await.is_none());
                session.id().await
            }),
        )
        .layer(SessionLayer::from_config(config, store.clone()));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header(COOKIE, "PYFLY_SESSION=old")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // The stale entry was deleted.
    assert_eq!(store.get("old").await.unwrap(), None);
}

#[tokio::test]
async fn custom_cookie_name_and_config() {
    let config = SessionConfig {
        cookie_name: "SID".to_string(),
        same_site: firefly_session::SameSite::Strict,
        http_only: false,
        ..SessionConfig::default()
    };
    let app = Router::new()
        .route(
            "/",
            get(|session: Extension<Session>| async move {
                session.set_attribute("a", 1u8).await.unwrap();
                "ok"
            }),
        )
        .layer(SessionLayer::from_config(config, store()));

    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cookie = set_cookie(&resp).unwrap();
    assert!(cookie.starts_with("SID="));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(!cookie.contains("HttpOnly"));
}
