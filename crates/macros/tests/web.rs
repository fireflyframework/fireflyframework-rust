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

//! Behavioral test for `#[rest_controller]`: the generated `routes(state)`
//! produces a real axum `Router` that we drive with `tower::ServiceExt::oneshot`
//! (no socket bound), and error returns render as RFC 7807 problems.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::Json;
use firefly::prelude::*;
use firefly::web::WebError;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

#[derive(Clone, Serialize, Deserialize)]
struct OrderView {
    id: String,
    total: u64,
}

#[derive(Deserialize)]
struct CreateOrder {
    total: u64,
}

/// The controller carries its shared state — here a shared counter standing in
/// for a repository / bus handle.
#[derive(Clone)]
struct OrderApi {
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    /// `GET /api/v1/orders/:id` — typed Path extractor + JSON response.
    #[get("/:id")]
    async fn get_order(
        State(_api): State<OrderApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<OrderView>> {
        if id == "missing" {
            return Err(WebError::from(FireflyError::not_found(format!(
                "order {id} not found"
            ))));
        }
        Ok(Json(OrderView { id, total: 42 }))
    }

    /// `POST /api/v1/orders` — JSON body in, JSON view out.
    #[post("")]
    async fn create_order(
        State(api): State<OrderApi>,
        Json(body): Json<CreateOrder>,
    ) -> WebResult<Json<OrderView>> {
        let id = api
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Json(OrderView {
            id: format!("order-{id}"),
            total: body.total,
        }))
    }
}

fn router() -> axum::Router {
    OrderApi::routes(OrderApi {
        next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
    })
}

async fn body_string(res: axum::response::Response) -> String {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn rest_controller_routes_get() {
    let res = router()
        .oneshot(
            Request::builder()
                .uri("/api/v1/orders/abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_string(res).await;
    let view: OrderView = serde_json::from_str(&body).unwrap();
    assert_eq!(view.id, "abc");
    assert_eq!(view.total, 42);
}

#[tokio::test]
async fn rest_controller_routes_post() {
    let res = router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/orders")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"total":99}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // axum's default `Json` response status is 200 OK.
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_string(res).await;
    let view: OrderView = serde_json::from_str(&body).unwrap();
    assert_eq!(view.total, 99);
    assert_eq!(view.id, "order-1");
}

#[test]
fn rest_controller_emits_route_metadata() {
    // The macro emits a `ROUTES` const describing every mapped method, for the
    // OpenAPI generator (phase 2).
    let routes = OrderApi::ROUTES;
    assert_eq!(routes.len(), 2);
    let get = routes
        .iter()
        .find(|r| r.method == "GET")
        .expect("GET route descriptor");
    assert_eq!(get.path, "/api/v1/orders/:id");
    assert_eq!(get.handler, "get_order");
    assert_eq!(get.controller, "OrderApi");
    let post = routes
        .iter()
        .find(|r| r.method == "POST")
        .expect("POST route descriptor");
    assert_eq!(post.path, "/api/v1/orders");

    // The same routes are discoverable across the crate graph via inventory.
    let discovered: Vec<_> = firefly::container::routes()
        .filter(|r| r.controller == "OrderApi")
        .collect();
    assert_eq!(discovered.len(), 2);
}

#[tokio::test]
async fn rest_controller_error_renders_as_problem() {
    let res = router()
        .oneshot(
            Request::builder()
                .uri("/api/v1/orders/missing")
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
        .unwrap_or("")
        .to_string();
    assert!(
        ct.contains("application/problem+json"),
        "error should render as an RFC 7807 problem, got content-type {ct:?}"
    );
}
