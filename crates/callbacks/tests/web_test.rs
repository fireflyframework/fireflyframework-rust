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

//! REST admin handler tests — the Go module's `web/handler_test.go`
//! ported 1:1 (TestAdminCRUD), plus Rust-specific coverage of the error
//! bodies, 405s, and the attempts listing. Everything runs in-process
//! via `tower::ServiceExt::oneshot`.

use std::sync::Arc;

use axum::body::Body;
use axum::Router;
use http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use firefly_callbacks::{handler, Attempt, MemoryStore, Store, Target};

fn app(store: Arc<MemoryStore>) -> Router {
    handler(store)
}

async fn call(app: Router, method: Method, uri: &str, body: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("oneshot");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body");
    (
        status,
        String::from_utf8_lossy(&bytes.to_bytes()).into_owned(),
    )
}

// --- Go: TestAdminCRUD -------------------------------------------------------

#[tokio::test]
async fn admin_crud() {
    let store = Arc::new(MemoryStore::new());

    let body = serde_json::to_string(&Target {
        id: "t1".into(),
        url: "https://example.com/cb".into(),
        active: true,
        ..Target::default()
    })
    .unwrap();
    let (status, created) = call(
        app(store.clone()),
        Method::POST,
        "/callbacks/targets",
        Some(&body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create status");
    let created: Target = serde_json::from_str(&created).unwrap();
    assert_eq!(created.id, "t1");

    let (status, listed) = call(app(store.clone()), Method::GET, "/callbacks/targets", None).await;
    assert_eq!(status, StatusCode::OK);
    let list: Vec<Target> = serde_json::from_str(&listed).unwrap();
    assert_eq!(list.len(), 1, "list: {list:?}");
    assert_eq!(list[0].id, "t1");

    let (status, _) = call(
        app(store.clone()),
        Method::DELETE,
        "/callbacks/targets/t1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete status");
    assert!(store.list_targets().await.unwrap().is_empty());
}

// --- Rust-specific coverage ----------------------------------------------------

#[tokio::test]
async fn get_target_round_trips_and_404s_with_go_error_body() {
    let store = Arc::new(MemoryStore::new());
    store
        .upsert_target(Target {
            id: "t1".into(),
            url: "https://example.com/cb".into(),
            secret: "hidden".into(),
            active: true,
            ..Target::default()
        })
        .await
        .unwrap();

    let (status, body) = call(
        app(store.clone()),
        Method::GET,
        "/callbacks/targets/t1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("hidden"),
        "secret must never leak over the admin API: {body}"
    );
    let t: Target = serde_json::from_str(&body).unwrap();
    assert_eq!(t.url, "https://example.com/cb");

    // Missing target: Go's http.Error(w, err.Error(), 404).
    let (status, body) = call(
        app(store.clone()),
        Method::GET,
        "/callbacks/targets/missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "firefly/callbacks: not found\n");

    let (status, body) = call(
        app(store),
        Method::DELETE,
        "/callbacks/targets/missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, "firefly/callbacks: not found\n");
}

#[tokio::test]
async fn upsert_rejects_invalid_json_with_400() {
    let store = Arc::new(MemoryStore::new());
    let (status, _) = call(
        app(store.clone()),
        Method::POST,
        "/callbacks/targets",
        Some("{not json"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(store.list_targets().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_methods_answer_405() {
    let store = Arc::new(MemoryStore::new());
    let (status, body) = call(app(store.clone()), Method::PUT, "/callbacks/targets", None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body, "method not allowed\n");

    let (status, body) = call(app(store), Method::POST, "/callbacks/targets/t1", None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body, "method not allowed\n");
}

#[tokio::test]
async fn attempts_listing_matches_go_wire_shape() {
    let store = Arc::new(MemoryStore::new());

    // Go parity: an event with no recorded attempts answers `null`
    // (MemoryStore's nil slice through encoding/json), not `[]`, and
    // json.Encoder.Encode terminates the document with '\n'.
    let (status, body) = call(
        app(store.clone()),
        Method::GET,
        "/callbacks/attempts/unknown",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "null\n");

    store
        .record_attempt(Attempt {
            id: "a1".into(),
            event_id: "ev1".into(),
            target_id: "t1".into(),
            status: 200,
            attempt: 1,
            ..Attempt::default()
        })
        .await
        .unwrap();

    let (status, body) = call(app(store), Method::GET, "/callbacks/attempts/ev1", None).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["eventId"], "ev1");
    assert_eq!(rows[0]["targetId"], "t1");
    assert_eq!(rows[0]["status"], 200);
    assert_eq!(rows[0]["attempt"], 1);
}

// Regression (bug: trailing newline dropped from admin JSON bodies):
// Go's writeJSON uses json.NewEncoder(w).Encode(v), which terminates
// every document with '\n' — so the wire bytes are "[...]\n",
// "{...}\n", and "null\n", never the bare document.
#[tokio::test]
async fn json_responses_end_with_newline_like_go_json_encoder() {
    let store = Arc::new(MemoryStore::new());

    // Empty targets listing: Go's MemoryStore returns a non-nil empty
    // slice, so the body is "[]\n".
    let (status, body) = call(app(store.clone()), Method::GET, "/callbacks/targets", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "[]\n");

    let payload = serde_json::to_string(&Target {
        id: "t1".into(),
        url: "https://example.com/cb".into(),
        active: true,
        ..Target::default()
    })
    .unwrap();
    let (status, body) = call(
        app(store.clone()),
        Method::POST,
        "/callbacks/targets",
        Some(&payload),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body.ends_with("}\n"), "created target body: {body:?}");

    let (status, body) = call(
        app(store.clone()),
        Method::GET,
        "/callbacks/targets/t1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.ends_with("}\n"), "get target body: {body:?}");

    let (status, body) = call(app(store.clone()), Method::GET, "/callbacks/targets", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.ends_with("]\n"), "targets listing body: {body:?}");

    store
        .record_attempt(Attempt {
            id: "a1".into(),
            event_id: "ev1".into(),
            target_id: "t1".into(),
            status: 200,
            attempt: 1,
            ..Attempt::default()
        })
        .await
        .unwrap();
    let (status, body) = call(app(store), Method::GET, "/callbacks/attempts/ev1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.ends_with("]\n"), "attempts listing body: {body:?}");
}

#[tokio::test]
async fn upsert_response_carries_created_target_as_json() {
    let store = Arc::new(MemoryStore::new());
    let (status, body) = call(
        app(store),
        Method::POST,
        "/callbacks/targets",
        Some(r#"{"id":"t9","url":"https://x.example","active":true,"eventTypes":["a.b"]}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let saved: Target = serde_json::from_str(&body).unwrap();
    assert_eq!(saved.id, "t9");
    assert_eq!(saved.event_types, vec!["a.b".to_string()]);
    assert!(saved.active);
}
