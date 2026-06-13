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

//! End-to-end tests for the session-restore layer and the cookie-keyed
//! `firefly-session` bridge (pyfly:
//! `OAuth2SessionSecurityFilter` + the real `LoginSessionStore`), plus the
//! OAuth2 login → session concurrency enforcement at the login binding
//! point.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::routing::get;
use axum::{Extension, Router};
use firefly_security::oauth2::{
    ClientRegistration, InMemoryClientRegistrationRepository, LoginSession, LoginSessionStore,
    OAuth2LoginHandler, SESSION_KEY_SECURITY_CONTEXT, SESSION_KEY_STATE,
};
use firefly_security::{
    Authentication, SessionAuthenticationLayer, SessionLoginSessionStore, ANONYMOUS_ID,
};
use firefly_session::{
    ConcurrencyPolicy, MemorySessionRegistry, SessionConcurrencyController, SessionLayer,
    SessionRegistry, SessionStore, Strategy,
};
use http::header::{COOKIE, SET_COOKIE};
use http::{Method, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

/// A downstream handler that echoes the restored principal.
async fn whoami(auth: Option<Extension<Authentication>>) -> String {
    match auth {
        Some(Extension(a)) => a.principal,
        None => "<none>".into(),
    }
}

/// A handler that stores an authenticated `SECURITY_CONTEXT` on the session
/// (the way the OAuth2 login handler does, as a JSON string).
async fn login(session: Extension<firefly_session::Session>, principal: String) {
    let auth = Authentication {
        principal,
        ..Default::default()
    };
    session
        .set_attribute(
            SESSION_KEY_SECURITY_CONTEXT,
            serde_json::to_string(&auth).unwrap(),
        )
        .await
        .unwrap();
}

fn app(store: Arc<dyn SessionStore>) -> Router {
    Router::new()
        .route(
            "/login/:principal",
            get(
                |session: Extension<firefly_session::Session>,
                 axum::extract::Path(principal): axum::extract::Path<String>| async move {
                    login(session, principal).await;
                    "ok"
                },
            ),
        )
        .route("/whoami", get(whoami))
        // last-added layer runs first: SessionLayer must run before the
        // restore layer, so add it after.
        .layer(SessionAuthenticationLayer::new())
        .layer(SessionLayer::new(store))
}

fn session_cookie(resp: &axum::response::Response) -> String {
    resp.headers()
        .get(SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn restores_principal_across_requests() {
    let store: Arc<dyn SessionStore> = Arc::new(firefly_session::MemorySessionStore::new());
    let app = app(store);

    // Request 1: establish a session and store a security context.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/login/alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = session_cookie(&resp);

    // Request 2: a fresh request carrying the cookie sees the restored
    // principal injected by the SessionAuthenticationLayer.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/whoami")
                .header(COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"alice");
}

#[tokio::test]
async fn unauthenticated_request_gets_anonymous_context() {
    let store: Arc<dyn SessionStore> = Arc::new(firefly_session::MemorySessionStore::new());
    let app = app(store);

    // No prior login: the layer injects an anonymous Authentication.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], ANONYMOUS_ID.as_bytes());
}

#[tokio::test]
async fn anonymous_fallback_can_be_disabled() {
    let store: Arc<dyn SessionStore> = Arc::new(firefly_session::MemorySessionStore::new());
    let app = Router::new()
        .route("/whoami", get(whoami))
        .layer(SessionAuthenticationLayer::new().anonymous_fallback(false))
        .layer(SessionLayer::new(store));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    // No Authentication inserted → handler sees None.
    assert_eq!(&body[..], b"<none>");
}

// ---------------------------------------------------------------------------
// Cookie-keyed LoginSessionStore bridge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cookie_keyed_store_resolves_same_session_by_cookie() {
    let store: Arc<dyn SessionStore> = Arc::new(firefly_session::MemorySessionStore::new());
    let login_store = SessionLoginSessionStore::new(store.clone());

    // First resolve (no cookie) mints a session and we set an attribute.
    let s1 = login_store.session(&http::HeaderMap::new()).await;
    s1.set_attribute(SESSION_KEY_STATE, "abc".into()).await;
    let sid = s1.id().await.unwrap();

    // A *new* store instance resolves the same session id from the cookie.
    let next = SessionLoginSessionStore::new(store.clone());
    let mut headers = http::HeaderMap::new();
    headers.insert(COOKIE, format!("PYFLY_SESSION={sid}").parse().unwrap());
    let s2 = next.session(&headers).await;
    assert_eq!(
        s2.get_attribute(SESSION_KEY_STATE).await.as_deref(),
        Some("abc")
    );
}

// ---------------------------------------------------------------------------
// OAuth2 login → concurrency enforcement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_enforces_max_sessions_reject_new() {
    use firefly_security::oauth2::FixedLoginSessionStore;

    // A token+userinfo provider mock.
    let provider = spawn_provider(json!({"sub": "user-1"})).await;
    let reg = ClientRegistration::new("acme", "cid")
        .client_secret("secret")
        .redirect_uri("https://app/cb")
        .scopes(&["openid"])
        .authorization_uri("https://idp/auth")
        .token_uri(format!("{provider}/token"))
        .user_info_uri(format!("{provider}/userinfo"))
        .use_pkce(false);

    let registry = Arc::new(MemorySessionRegistry::new());
    let controller = Arc::new(SessionConcurrencyController::new(
        registry.clone(),
        ConcurrencyPolicy {
            max_sessions: 1,
            strategy: Strategy::RejectNew,
        },
    ));

    // Pre-register an existing session for user-1 so the next login is over
    // the cap of 1.
    controller.on_login("user-1", "pre-existing", 1).await;

    let sessions = Arc::new(FixedLoginSessionStore::new());
    let repo = Arc::new(InMemoryClientRegistrationRepository::new([reg]));
    let app = OAuth2LoginHandler::new(repo, sessions.clone()).with_concurrency(controller.clone());
    let router = app.router();
    let session = sessions.session();

    // authorize leg stashes state.
    router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth2/authorization/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let state = session.get_attribute(SESSION_KEY_STATE).await.unwrap();

    // callback: over cap under reject-new → 401 max_sessions, session wiped.
    let resp = router
        .oneshot(
            Request::builder()
                .uri(format!("/login/oauth2/code/acme?code=c&state={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body: Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "max_sessions");
    // The rejected session's context was not stored.
    assert_eq!(
        session.get_attribute(SESSION_KEY_SECURITY_CONTEXT).await,
        None
    );
    // The cap held: only the pre-existing session remains.
    assert_eq!(registry.count("user-1").await, 1);
}

#[tokio::test]
async fn login_admits_under_cap_and_logout_deregisters() {
    use firefly_security::oauth2::FixedLoginSessionStore;

    let provider = spawn_provider(json!({"sub": "user-2"})).await;
    let reg = ClientRegistration::new("acme", "cid")
        .client_secret("secret")
        .redirect_uri("https://app/cb")
        .scopes(&["openid"])
        .authorization_uri("https://idp/auth")
        .token_uri(format!("{provider}/token"))
        .user_info_uri(format!("{provider}/userinfo"))
        .use_pkce(false);

    let registry = Arc::new(MemorySessionRegistry::new());
    let controller = Arc::new(SessionConcurrencyController::new(
        registry.clone(),
        ConcurrencyPolicy {
            max_sessions: 2,
            strategy: Strategy::RejectNew,
        },
    ));

    let sessions = Arc::new(FixedLoginSessionStore::new());
    let repo = Arc::new(InMemoryClientRegistrationRepository::new([reg]));
    let router = OAuth2LoginHandler::new(repo, sessions.clone())
        .with_concurrency(controller.clone())
        .router();
    let session = sessions.session();

    router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/oauth2/authorization/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let state = session.get_attribute(SESSION_KEY_STATE).await.unwrap();

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/login/oauth2/code/acme?code=c&state={state}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND, "login admitted under cap");
    assert_eq!(registry.count("user-2").await, 1, "session registered");

    // Logout deregisters the principal's session.
    let resp = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FOUND);
    assert_eq!(registry.count("user-2").await, 0, "session deregistered");
}

/// Spawns a minimal OAuth2 provider mock (token + userinfo) and returns its
/// base URL.
async fn spawn_provider(user_info: Value) -> String {
    use axum::{Form, Json};
    use std::collections::HashMap;

    let ui = Arc::new(user_info);
    let ui_token = ui.clone();
    let app = Router::new()
        .route(
            "/token",
            axum::routing::post(move |Form(_f): Form<HashMap<String, String>>| {
                let _ = ui_token.clone();
                async move { Json(json!({"access_token": "AT-123", "token_type": "Bearer"})) }
            }),
        )
        .route(
            "/userinfo",
            get(move |headers: http::HeaderMap| {
                let ui = ui.clone();
                async move {
                    let ok = headers
                        .get(http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        == Some("Bearer AT-123");
                    if ok {
                        (StatusCode::OK, Json((*ui).clone()))
                    } else {
                        (StatusCode::UNAUTHORIZED, Json(json!({})))
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
