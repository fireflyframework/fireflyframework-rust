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

//! End-to-end test for RFC 7662 opaque-token introspection
//! ([`RemoteTokenIntrospector`]) against an in-process axum introspection
//! endpoint — covering the real HTTP round-trip and the `Verifier` drop-in.

use std::collections::HashMap;

use axum::routing::post;
use axum::{Form, Json, Router};
use firefly_security::oauth2::{RemoteTokenIntrospector, TokenIntrospector};
use firefly_security::Verifier;
use serde_json::{json, Value};

/// A minimal RFC 7662 endpoint: `good-token` is active, everything else isn't.
async fn introspect_handler(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
    if form.get("token").map(String::as_str) == Some("good-token") {
        Json(json!({
            "active": true,
            "sub": "u1",
            "username": "alice",
            "scope": "read write",
            "client_id": "svc"
        }))
    } else {
        Json(json!({ "active": false }))
    }
}

/// Spawns the mock introspection server and returns its `/introspect` URL.
async fn spawn_introspection_server() -> String {
    let app = Router::new().route("/introspect", post(introspect_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/introspect")
}

#[tokio::test]
async fn introspects_active_token_and_rejects_inactive() {
    let uri = spawn_introspection_server().await;
    let introspector = RemoteTokenIntrospector::new(uri, "rs-client", "rs-secret");

    // An active token resolves to the authenticated principal + authorities.
    let auth = introspector
        .introspect("good-token")
        .await
        .expect("active token");
    assert_eq!(auth.principal, "u1");
    assert_eq!(auth.username, "alice");
    assert!(auth.has_authority("read"));
    assert!(auth.has_authority("write"));

    // The same introspector is a drop-in resource-server Verifier.
    let via_verifier = introspector.verify("good-token").await.expect("verify");
    assert_eq!(via_verifier.principal, "u1");

    // An inactive / unknown token fails closed, both ways.
    assert!(introspector.introspect("revoked").await.is_err());
    assert!(introspector.verify("revoked").await.is_err());
}
