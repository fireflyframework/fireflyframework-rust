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

//! End-to-end test for the outbound OAuth2 client
//! ([`OAuth2AuthorizedClientManager`]) against an in-process token endpoint:
//! the client-credentials grant obtains + caches a token, and an expired token
//! holding a refresh token is refreshed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Form, Json, Router};
use firefly_security::oauth2::{
    ClientRegistration, InMemoryClientRegistrationRepository,
    InMemoryOAuth2AuthorizedClientService, OAuth2AuthorizedClient, OAuth2AuthorizedClientManager,
    OAuth2AuthorizedClientService,
};
use serde_json::{json, Value};

/// A token endpoint: client-credentials issues a unique `cc-N` token (so a
/// cache hit is observable as an unchanged N); refresh issues `refreshed-1`.
async fn token_handler(
    State(counter): State<Arc<Mutex<u32>>>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<Value> {
    match form.get("grant_type").map(String::as_str) {
        Some("client_credentials") => {
            let mut n = counter.lock().unwrap();
            *n += 1;
            Json(json!({
                "access_token": format!("cc-{n}"),
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "api"
            }))
        }
        Some("refresh_token") => Json(json!({
            "access_token": "refreshed-1",
            "token_type": "Bearer",
            "expires_in": 3600
        })),
        _ => Json(json!({ "error": "unsupported_grant_type" })),
    }
}

async fn spawn_token_server() -> (String, Arc<Mutex<u32>>) {
    let counter = Arc::new(Mutex::new(0u32));
    let app = Router::new()
        .route("/token", post(token_handler))
        .with_state(counter.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/token"), counter)
}

fn registration(token_uri: &str) -> ClientRegistration {
    ClientRegistration::new("svc-reg", "svc-client")
        .client_secret("svc-secret")
        .scopes(&["api"])
        .token_uri(token_uri)
}

#[tokio::test]
async fn client_credentials_obtains_and_then_caches() {
    let (token_uri, counter) = spawn_token_server().await;
    let repo = Arc::new(InMemoryClientRegistrationRepository::new([registration(
        &token_uri,
    )]));
    let service = Arc::new(InMemoryOAuth2AuthorizedClientService::new());
    let manager = OAuth2AuthorizedClientManager::new(repo, service);

    // First call performs the grant.
    let first = manager
        .authorize_client_credentials("svc-reg")
        .await
        .expect("grant");
    assert_eq!(first.access_token, "cc-1");
    assert_eq!(first.scopes, vec!["api"]);

    // Second call returns the cached, still-valid token — no new grant.
    let second = manager
        .authorize_client_credentials("svc-reg")
        .await
        .expect("cached");
    assert_eq!(second.access_token, "cc-1");
    assert_eq!(
        *counter.lock().unwrap(),
        1,
        "token endpoint hit exactly once"
    );
}

#[tokio::test]
async fn expired_token_with_refresh_token_is_refreshed() {
    let (token_uri, _counter) = spawn_token_server().await;
    let repo = Arc::new(InMemoryClientRegistrationRepository::new([registration(
        &token_uri,
    )]));
    let service = Arc::new(InMemoryOAuth2AuthorizedClientService::new());

    // Pre-seed an expired client (principal = client id) holding a refresh token.
    service
        .save(OAuth2AuthorizedClient {
            registration_id: "svc-reg".into(),
            principal_name: "svc-client".into(),
            access_token: "stale".into(),
            refresh_token: Some("rt-x".into()),
            expires_at: Some(1), // long past
            scopes: vec!["api".into()],
        })
        .await;

    let manager = OAuth2AuthorizedClientManager::new(repo, service);
    let refreshed = manager
        .authorize_client_credentials("svc-reg")
        .await
        .expect("refresh");
    assert_eq!(refreshed.access_token, "refreshed-1");
    // The refresh response omitted a new refresh token → the old one is kept.
    assert_eq!(refreshed.refresh_token.as_deref(), Some("rt-x"));
}
