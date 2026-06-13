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

//! End-to-end proof that the macro-generated routes and handlers work: the
//! `#[rest_controller]` router is driven in-process via `tower::oneshot`
//! (no socket bound), each request flowing through the `#[command_handler]` /
//! `#[query_handler]` over the `Bus`, and errors render as RFC 9457 problems.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use firefly_sample_macro_quickstart::{build_router, build_scheduler, OrderView};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn body_string(res: axum::response::Response) -> String {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// `POST` then `GET` round-trips through the macro-generated controller and
/// CQRS handlers.
#[tokio::test]
async fn place_then_fetch_round_trips() {
    let router = build_router();

    // POST /api/v1/orders — dispatches the `#[command_handler]`.
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/orders")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"customer":"alice","sku":"SKU-1","quantity":2}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let placed: OrderView = serde_json::from_str(&body_string(res).await).unwrap();
    assert_eq!(placed.customer, "alice");
    assert_eq!(placed.sku, "SKU-1");
    assert_eq!(placed.quantity, 2);
    assert_eq!(placed.id, "order-1");

    // GET /api/v1/orders/order-1 — dispatches the `#[query_handler]`.
    let res = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/orders/{}", placed.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let fetched: OrderView = serde_json::from_str(&body_string(res).await).unwrap();
    assert_eq!(fetched.id, placed.id);
    assert_eq!(fetched.customer, "alice");
}

/// An unknown id renders as a 404 RFC 9457 problem (the handler's not-found
/// flowing through `WebError`).
#[tokio::test]
async fn unknown_id_is_problem_404() {
    let res = build_router()
        .oneshot(
            Request::builder()
                .uri("/api/v1/orders/order-999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.contains("application/problem+json"),
        "error should render as an RFC 9457 problem, got {ct:?}"
    );
}

/// `#[firefly(validate)]` rejects an empty `customer` before the handler runs:
/// the `ValidationMiddleware` short-circuits dispatch, which the controller
/// maps to a 422 problem.
#[tokio::test]
async fn invalid_command_is_problem_422() {
    let res = build_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/orders")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"customer":"","sku":"SKU-1","quantity":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        ct.contains("application/problem+json"),
        "validation failure should render as a problem, got {ct:?}"
    );
}

/// The `#[scheduled]` macro generated a registration helper that puts a named
/// task on the scheduler.
#[test]
fn scheduled_task_registers() {
    let scheduler = build_scheduler();
    let names: Vec<String> = scheduler.tasks().into_iter().map(|t| t.name).collect();
    assert!(
        names.contains(&"sweep_stale_orders".to_string()),
        "the #[scheduled] task should be registered, got {names:?}"
    );
}
