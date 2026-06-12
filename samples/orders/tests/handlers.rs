//! Port of the Go sample's `web/handlers_test.go` — the full stack
//! (`build_router()`, Go's `BuildHandler()`) driven in-process via
//! `tower::ServiceExt::oneshot`, asserting on the wire shape.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use firefly_cqrs::QueryCache;
use firefly_kernel::{
    ProblemDetail, HEADER_CORRELATION_ID, HEADER_IDEMPOTENCY_KEY, PROBLEM_CONTENT_TYPE,
    TYPE_IDEMPOTENCY, TYPE_NOT_FOUND, TYPE_VALIDATION,
};
use firefly_sample_orders::core::register;
use firefly_sample_orders::interfaces::{OrderDto, PlaceOrderRequest};
use firefly_sample_orders::models::{MemoryRepository, Order, Repository, RepositoryError};
use firefly_sample_orders::web::{api_router, build_core, build_router};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn place_request() -> PlaceOrderRequest {
    PlaceOrderRequest {
        customer: "alice".into(),
        sku: "SKU-1".into(),
        quantity: 2,
        total: 19.99,
    }
}

fn post_orders(body: &[u8], idempotency_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::post("/api/v1/orders").header("Content-Type", "application/json");
    if let Some(key) = idempotency_key {
        builder = builder.header(HEADER_IDEMPOTENCY_KEY, key);
    }
    builder.body(Body::from(body.to_vec())).unwrap()
}

fn get_order(id: &str) -> Request<Body> {
    Request::get(format!("/api/v1/orders/{id}"))
        .body(Body::empty())
        .unwrap()
}

async fn body_bytes(res: Response) -> Vec<u8> {
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

fn content_type(res: &Response) -> &str {
    res.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

/// Go: TestPlaceAndGetOrder.
#[tokio::test]
async fn place_and_get_order() {
    let app = build_router();
    let body = serde_json::to_vec(&place_request()).unwrap();

    let res = app
        .clone()
        .oneshot(post_orders(&body, Some("abc-123")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert!(
        res.headers().contains_key(HEADER_CORRELATION_ID),
        "correlation id missing on response"
    );
    let out: OrderDto = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert!(!out.id.is_empty(), "dto: {out:?}");
    assert_eq!(out.status, "placed", "dto: {out:?}");
    assert_eq!(out.customer, "alice", "dto: {out:?}");

    // Idempotent replay returns same id.
    let res2 = app
        .clone()
        .oneshot(post_orders(&body, Some("abc-123")))
        .await
        .unwrap();
    assert_eq!(
        res2.headers()
            .get("Idempotent-Replay")
            .and_then(|v| v.to_str().ok()),
        Some("true"),
        "expected idempotent replay"
    );
    let out2: OrderDto = serde_json::from_slice(&body_bytes(res2).await).unwrap();
    assert_eq!(out2.id, out.id, "ids differ: {} vs {}", out.id, out2.id);

    // GET returns the same order.
    let res3 = app.clone().oneshot(get_order(&out.id)).await.unwrap();
    assert_eq!(res3.status(), StatusCode::OK);
    let got: OrderDto = serde_json::from_slice(&body_bytes(res3).await).unwrap();
    assert_eq!(got.id, out.id, "get id mismatch");
}

/// Go: TestPlaceOrderValidation.
#[tokio::test]
async fn place_order_validation() {
    let app = build_router();
    let body = br#"{"customer":"","sku":"","quantity":0,"total":0}"#;

    let res = app.oneshot(post_orders(body, None)).await.unwrap();
    assert!(
        !res.status().is_success(),
        "expected 4xx, got {}",
        res.status()
    );
    assert!(
        content_type(&res).starts_with(PROBLEM_CONTENT_TYPE),
        "content-type: {}",
        content_type(&res)
    );
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert!(
        pd.status == StatusCode::UNPROCESSABLE_ENTITY.as_u16()
            || pd.status == StatusCode::BAD_REQUEST.as_u16(),
        "status: {}",
        pd.status
    );
    // Rust-specific strengthening: the exact problem shape.
    assert_eq!(pd.status, 422);
    assert_eq!(pd.problem_type, TYPE_VALIDATION);
    assert_eq!(pd.detail, "customer is required");
}

/// Go: TestGetOrderNotFound.
#[tokio::test]
async fn get_order_not_found() {
    let app = build_router();

    let res = app.oneshot(get_order("missing")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(
        content_type(&res).starts_with(PROBLEM_CONTENT_TYPE),
        "content-type: {}",
        content_type(&res)
    );
    // Rust-specific strengthening: Go's kernel.NewNotFound detail.
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(pd.problem_type, TYPE_NOT_FOUND);
    assert_eq!(pd.detail, "order missing not found");
}

/// Malformed JSON renders the Go handler's 400 problem
/// (`invalid json: …`).
#[tokio::test]
async fn place_order_invalid_json() {
    let app = build_router();

    let res = app.oneshot(post_orders(b"{not json", None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(content_type(&res).starts_with(PROBLEM_CONTENT_TYPE));
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert!(
        pd.detail.starts_with("invalid json: "),
        "detail: {}",
        pd.detail
    );
}

/// Reusing an Idempotency-Key with a different payload is a 409
/// idempotency-conflict problem — the framework contract the sample
/// inherits from `firefly-web`.
#[tokio::test]
async fn idempotency_key_reuse_with_different_payload_conflicts() {
    let app = build_router();
    let body = serde_json::to_vec(&place_request()).unwrap();

    let first = app
        .clone()
        .oneshot(post_orders(&body, Some("key-1")))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);

    let other = serde_json::to_vec(&PlaceOrderRequest {
        customer: "bob".into(),
        ..place_request()
    })
    .unwrap();
    let res = app
        .clone()
        .oneshot(post_orders(&other, Some("key-1")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let pd: ProblemDetail = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(pd.problem_type, TYPE_IDEMPOTENCY);
}

/// The 201 response carries the Go handlers' headers and trailing
/// newline (`json.Encoder` byte parity).
#[tokio::test]
async fn place_order_response_wire_shape() {
    let app = build_router();
    let body = serde_json::to_vec(&place_request()).unwrap();

    let res = app.oneshot(post_orders(&body, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(content_type(&res), "application/json");
    let location = res
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let raw = body_bytes(res).await;
    assert_eq!(raw.last(), Some(&b'\n'), "Go json.Encoder trailing newline");
    let out: OrderDto = serde_json::from_slice(&raw).unwrap();
    assert_eq!(location, format!("/api/v1/orders/{}", out.id));
}

/// A counting repository that proves which requests reach the handler
/// versus being served by the idempotency replay or the query cache.
#[derive(Default)]
struct CountingRepository {
    inner: MemoryRepository,
    saves: AtomicU32,
    gets: AtomicU32,
}

#[async_trait]
impl Repository for CountingRepository {
    async fn save(&self, order: Order) -> Result<Order, RepositoryError> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        self.inner.save(order).await
    }

    async fn get(&self, id: &str) -> Result<Order, RepositoryError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }
}

/// The Go README's contract, proven end to end: the replayed POST never
/// reaches the handler again, and repeated GETs within the 30 s TTL are
/// served from the CQRS query cache (one repository hit).
#[tokio::test]
async fn replay_skips_handler_and_get_hits_query_cache() {
    // Same wiring as build_router(), with an observable repository.
    let core = build_core();
    let query_cache = QueryCache::new();
    core.bus.use_middleware(query_cache.middleware());
    let repo = Arc::new(CountingRepository::default());
    register(&core.bus, Arc::clone(&repo) as Arc<dyn Repository>);
    let app = core.apply_middleware(api_router(Arc::clone(&core.bus)));

    let body = serde_json::to_vec(&place_request()).unwrap();

    // First POST runs the handler.
    let res = app
        .clone()
        .oneshot(post_orders(&body, Some("cache-key")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let out: OrderDto = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(repo.saves.load(Ordering::SeqCst), 1);

    // Replay serves the captured response: still exactly one save.
    let res = app
        .clone()
        .oneshot(post_orders(&body, Some("cache-key")))
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("Idempotent-Replay")
            .and_then(|v| v.to_str().ok()),
        Some("true")
    );
    assert_eq!(repo.saves.load(Ordering::SeqCst), 1, "handler ran twice");

    // First GET goes to the handler…
    let res = app.clone().oneshot(get_order(&out.id)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(repo.gets.load(Ordering::SeqCst), 1);

    // …subsequent reads within 30 s are served from the query cache.
    let res = app.clone().oneshot(get_order(&out.id)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let got: OrderDto = serde_json::from_slice(&body_bytes(res).await).unwrap();
    assert_eq!(got, out);
    assert_eq!(
        repo.gets.load(Ordering::SeqCst),
        1,
        "second GET bypassed the query cache"
    );

    // Invalidating the query family sends the next GET back to the
    // repository — the handle Go's BuildHandler discards, exercised.
    query_cache.invalidate_type::<firefly_sample_orders::interfaces::GetOrderQuery>();
    let res = app.clone().oneshot(get_order(&out.id)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(repo.gets.load(Ordering::SeqCst), 2);
}

/// Failed lookups are never cached: a 404 followed by a fresh GET still
/// reaches the repository each time (Go's "failed dispatches are never
/// cached" cache contract, observed through the full router).
#[tokio::test]
async fn not_found_responses_are_not_cached() {
    let core = build_core();
    let query_cache = QueryCache::new();
    core.bus.use_middleware(query_cache.middleware());
    let repo = Arc::new(CountingRepository::default());
    register(&core.bus, Arc::clone(&repo) as Arc<dyn Repository>);
    let app = core.apply_middleware(api_router(Arc::clone(&core.bus)));

    for expected_gets in 1..=2 {
        let res = app.clone().oneshot(get_order("ghost")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(repo.gets.load(Ordering::SeqCst), expected_gets);
    }
}
