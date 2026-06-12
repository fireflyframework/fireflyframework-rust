//! Port of pyfly's `tests/idp/test_wave_idp_web.py` — drives the
//! `firefly-idp` REST controller (`idp::web::router`) against the real
//! `firefly-idp-internal-db` [`Adapter`] in-process via
//! `tower::ServiceExt::oneshot`, with no sockets.
//!
//! pyfly's `TestClient`-driven flow (create a user over `/idp/admin/users`,
//! then log in over `/idp/login`, asserting an `access_token`) is ported
//! verbatim, then extended to cover `/idp/refresh`, `/idp/introspect`,
//! `/idp/userinfo`, `/idp/register`, role admin, and the error mappings.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::Router;
use firefly_idp::web;
use firefly_idp_internal_db::{Adapter, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Builds a router over a fresh internal-db adapter (cheap, in-memory).
fn build_app() -> Router {
    let idp = Adapter::new(Config {
        jwt_secret: b"web-controller-test-secret-please-rotate".to_vec(),
        token_ttl: Duration::from_secs(3600),
        issuer: "idp-web-test".into(),
    });
    web::router(Arc::new(idp) as Arc<dyn firefly_idp::Adapter>)
}

/// Issues one request and returns `(status, json_body)`.
async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (axum::http::StatusCode, Value) {
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
        Some(value) => builder
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

// ── pyfly test_idp_login_endpoint_reachable ─────────────────────────────

#[tokio::test]
async fn create_user_then_login_returns_access_token() {
    let app = build_app();

    // Create a user over the HTTP admin surface.
    let (status, created) = request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "alice", "password": "s3cret!!Pass", "email": "a@x.io"})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
    assert_eq!(created["username"], "alice");
    assert!(!created["id"].as_str().unwrap().is_empty());

    // Log in through the HTTP surface — pyfly asserts `access_token` present.
    let (status, token) = request(
        &app,
        "POST",
        "/idp/login",
        Some(json!({"username": "alice", "password": "s3cret!!Pass"})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{token}");
    assert!(token.get("access_token").is_some());
    assert!(!token["access_token"].as_str().unwrap().is_empty());
    assert_eq!(token["token_type"], "Bearer");
}

#[tokio::test]
async fn login_with_bad_password_is_401() {
    let app = build_app();
    request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "bob", "password": "correctPass1!"})),
        None,
    )
    .await;
    let (status, body) = request(
        &app,
        "POST",
        "/idp/login",
        Some(json!({"username": "bob", "password": "wrong"})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("invalid credentials"));
}

// ── refresh / introspect / validate / userinfo round trip ───────────────

#[tokio::test]
async fn login_then_refresh_introspect_validate_and_userinfo() {
    let app = build_app();
    request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "carol", "password": "carolPass12!", "email": "c@x.io"})),
        None,
    )
    .await;
    let (_, token) = request(
        &app,
        "POST",
        "/idp/login",
        Some(json!({"username": "carol", "password": "carolPass12!"})),
        None,
    )
    .await;
    let access = token["access_token"].as_str().unwrap().to_string();
    let refresh = token["refresh_token"].as_str().unwrap().to_string();

    // /idp/refresh exchanges the refresh token for a fresh token.
    let (status, refreshed) = request(
        &app,
        "POST",
        "/idp/refresh",
        Some(json!({"token": refresh})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{refreshed}");
    assert!(!refreshed["access_token"].as_str().unwrap().is_empty());

    // /idp/introspect reports the token active with the owning user.
    let (status, introspection) = request(
        &app,
        "POST",
        "/idp/introspect",
        Some(json!({"token": access})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{introspection}");
    assert_eq!(introspection["active"], true);
    assert_eq!(introspection["username"], "carol");

    // /idp/validate resolves the token to its User.
    let (status, user) = request(
        &app,
        "POST",
        "/idp/validate",
        Some(json!({"token": access})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{user}");
    assert_eq!(user["username"], "carol");

    // /idp/userinfo resolves the Bearer token to the same User.
    let (status, info) = request(&app, "GET", "/idp/userinfo", None, Some(&access)).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{info}");
    assert_eq!(info["username"], "carol");
    assert_eq!(info["email"], "c@x.io");
}

#[tokio::test]
async fn userinfo_without_bearer_is_401() {
    let app = build_app();
    let (status, _) = request(&app, "GET", "/idp/userinfo", None, None).await;
    assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
}

// ── register + admin user CRUD ──────────────────────────────────────────

#[tokio::test]
async fn register_creates_an_enabled_user() {
    let app = build_app();
    let (status, user) = request(
        &app,
        "POST",
        "/idp/register",
        Some(json!({"username": "dave", "password": "davePass123!", "email": "d@x.io"})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{user}");
    assert_eq!(user["username"], "dave");
    assert_eq!(user["enabled"], true);
    // The registered user can authenticate.
    let (status, _) = request(
        &app,
        "POST",
        "/idp/login",
        Some(json!({"username": "dave", "password": "davePass123!"})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
}

#[tokio::test]
async fn admin_list_get_and_delete_users() {
    let app = build_app();
    let (_, created) = request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "erin", "password": "erinPass123!"})),
        None,
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // GET by id.
    let (status, fetched) =
        request(&app, "GET", &format!("/idp/admin/users/{id}"), None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{fetched}");
    assert_eq!(fetched["username"], "erin");

    // GET unknown id → 404.
    let (status, _) = request(&app, "GET", "/idp/admin/users/nope", None, None).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    // LIST returns the user.
    let (status, list) = request(&app, "GET", "/idp/admin/users?limit=10", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["username"] == "erin"));

    // DELETE removes it.
    let (status, deleted) = request(
        &app,
        "DELETE",
        &format!("/idp/admin/users/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{deleted}");
    assert_eq!(deleted["success"], true);
    let (status, _) = request(&app, "GET", &format!("/idp/admin/users/{id}"), None, None).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
}

// ── role admin (pyfly internal-db assign_role catalogue test) ───────────

#[tokio::test]
async fn assign_role_populates_catalogue_and_revoke_removes_it() {
    let app = build_app();
    let (_, created) = request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "frank", "password": "frankPass12!"})),
        None,
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    // Assign ADMIN.
    let (status, body) = request(
        &app,
        "POST",
        &format!("/idp/admin/users/{id}/roles/ADMIN"),
        None,
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);

    // The role appears in the catalogue (pyfly audit #29).
    let (status, roles) = request(&app, "GET", "/idp/admin/roles", None, None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(roles
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "ADMIN"));

    // The user's own roles include ADMIN.
    let (status, user_roles) = request(
        &app,
        "GET",
        &format!("/idp/admin/users/{id}/roles"),
        None,
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(user_roles
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "ADMIN"));

    // Revoke it.
    let (status, body) = request(
        &app,
        "DELETE",
        &format!("/idp/admin/users/{id}/roles/ADMIN"),
        None,
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);
}

// ── logout ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn logout_revokes_the_token() {
    let app = build_app();
    request(
        &app,
        "POST",
        "/idp/admin/users",
        Some(json!({"username": "grace", "password": "gracePass12!"})),
        None,
    )
    .await;
    let (_, token) = request(
        &app,
        "POST",
        "/idp/login",
        Some(json!({"username": "grace", "password": "gracePass12!"})),
        None,
    )
    .await;
    let access = token["access_token"].as_str().unwrap().to_string();

    let (status, body) = request(
        &app,
        "POST",
        "/idp/logout",
        Some(json!({"token": access.clone()})),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);

    // After logout the token no longer resolves to a user via /userinfo.
    let (status, _) = request(&app, "GET", "/idp/userinfo", None, Some(&access)).await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
}

// ── the router mounts over any Arc<dyn Adapter> ─────────────────────────

#[tokio::test]
async fn router_is_generic_over_the_port_trait_object() {
    // The router accepts an erased `Arc<dyn Adapter>`, proving it mounts
    // over the port (not the concrete adapter).
    let idp: Arc<dyn firefly_idp::Adapter> = Arc::new(Adapter::new(Config {
        jwt_secret: b"another-secret-value-for-this-test".to_vec(),
        token_ttl: Duration::from_secs(60),
        issuer: "generic".into(),
    }));
    let _user = idp
        .create_user(firefly_idp::User::default(), "trait-obj-pw1!")
        .await;
    let _app = web::router(idp);
}
