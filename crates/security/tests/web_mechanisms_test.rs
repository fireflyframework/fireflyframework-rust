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

//! End-to-end integration tests for the Spring Security **Tier 2** web
//! mechanisms, composed through a real `axum::Router` and driven with
//! `tower::ServiceExt::oneshot` (no sockets). These exercise the full stack —
//! the authentication mechanism populating the context, the `FilterChain` /
//! `SecurityFilterChains` authorizing against it, and the handler reading it —
//! which the per-module unit tests cover only in isolation.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use http_body_util::BodyExt;
use tower::ServiceExt;

use firefly_security::{
    Authentication, AuthenticationManager, BcryptPasswordEncoder, DaoAuthenticationProvider,
    FilterChain, HttpBasicLayer, InMemoryUserDetailsService, PasswordEncoder, PathRequestMatcher,
    ProviderManager, SecurityFilterChains, TokenBasedRememberMeServices, UserDetails,
};

/// A `ProviderManager` over an in-memory `alice`/`pw` user with `ROLE_USER`.
fn manager() -> Arc<dyn AuthenticationManager> {
    let hash = BcryptPasswordEncoder::with_rounds(4).hash("pw").unwrap();
    let uds = Arc::new(
        InMemoryUserDetailsService::new().with_user(UserDetails::new(
            "alice",
            hash,
            vec!["USER".into()],
        )),
    );
    let provider = Arc::new(DaoAuthenticationProvider::new(
        uds,
        Arc::new(BcryptPasswordEncoder::with_rounds(4)),
    ));
    Arc::new(ProviderManager::new(vec![provider]))
}

fn basic_header(user: &str, pass: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// HTTP Basic + RBAC `FilterChain` end-to-end: the Basic layer authenticates
/// and scopes the context, then the chain authorizes the protected route.
#[tokio::test]
async fn http_basic_then_filter_chain_authorizes_a_protected_route() {
    // Layers run outermost-last: Basic runs first (populates the context),
    // then the chain authorizes against it.
    let app: Router = Router::new()
        .route(
            "/api/me",
            get(|Extension(auth): Extension<Authentication>| async move { auth.principal }),
        )
        .layer(FilterChain::new().require("/api", &["USER"]).layer())
        .layer(HttpBasicLayer::new(manager()).realm("test"));

    // Valid credentials -> the handler runs and sees the principal.
    let ok = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .header(header::AUTHORIZATION, basic_header("alice", "pw"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(body_string(ok).await, "alice");

    // Bad credentials -> 401 Basic challenge, handler never runs.
    let bad = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .header(header::AUTHORIZATION, basic_header("alice", "wrong"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
    assert!(bad
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("Basic realm=\"test\""));

    // No credentials -> the Basic layer passes through and the chain denies
    // (deny-by-default for an authenticated-only route).
    let none = app
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(none.status(), StatusCode::UNAUTHORIZED);
}

/// Multiple filter chains end-to-end through a real Router: `/api/**` is locked
/// down (deny-by-default, no public rule) while the web surface is permitted.
#[tokio::test]
async fn security_filter_chains_route_by_matcher() {
    let security = SecurityFilterChains::new()
        .chain(
            PathRequestMatcher::new("/api"),
            FilterChain::new().any_request_authenticated(),
        )
        .any(FilterChain::new().any_request_permit())
        .layer();

    let app: Router = Router::new()
        .route("/api/data", get(|| async { "secret" }))
        .route("/", get(|| async { "home" }))
        .layer(security);

    // The web surface is public.
    let home = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(home.status(), StatusCode::OK);
    assert_eq!(body_string(home).await, "home");

    // /api/** requires authentication; an anonymous request is rejected by the
    // first (api) chain, not served as public.
    let api = app
        .oneshot(
            Request::builder()
                .uri("/api/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);
}

/// Remember-me end-to-end: a minted token auto-logs-in as a *remembered*
/// (not fully authenticated) principal, and a sensitive route can reject it.
#[tokio::test]
async fn remember_me_auto_login_is_not_fully_authenticated() {
    let uds = Arc::new(
        InMemoryUserDetailsService::new().with_user(UserDetails::new(
            "alice",
            "stored-hash",
            vec!["USER".into()],
        )),
    );
    let svc = TokenBasedRememberMeServices::new("server-key", uds);

    let token = svc.make_token("alice", "stored-hash");
    let auth = {
        use firefly_security::RememberMeServices;
        svc.auto_login(&token).await.expect("auto-login")
    };

    assert_eq!(auth.principal, "alice");
    assert!(auth.is_authenticated());
    // Remembered, so a route guarded on "fully authenticated" must reject it.
    assert!(auth.is_remembered());
    assert!(!auth.is_fully_authenticated());

    // A token signed with a different server key does not auto-login.
    let foreign = TokenBasedRememberMeServices::new(
        "other-key",
        Arc::new(
            InMemoryUserDetailsService::new().with_user(UserDetails::new(
                "alice",
                "stored-hash",
                vec![],
            )),
        ),
    )
    .make_token("alice", "stored-hash");
    use firefly_security::RememberMeServices;
    assert!(svc.auto_login(&foreign).await.is_none());
}
