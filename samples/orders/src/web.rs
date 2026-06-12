//! Router composition and HTTP handlers — the port of the Go sample's
//! `web` package (`package main`: `handlers.go` + the wiring half of
//! `main.go`; the process entry point lives in `src/main.rs`).
//!
//! [`build_router`] is the testable composition root (Go's
//! `BuildHandler()`): the full public router wired with idempotency,
//! problem rendering, correlation, query caching, and the orders CQRS
//! handlers. `main()` uses the same building blocks plus the actuator
//! admin router.
//!
//! ## Error mapping
//!
//! In Go, handler errors flow through the bus as `error` values, so a
//! `kernel.NewNotFound` raised in `core` reaches the web tier intact.
//! The Rust bus's typed error channel is [`CqrsError`], so this module
//! restores the kernel error family at the HTTP boundary:
//!
//! - `POST`: any dispatch error becomes a 422 validation problem —
//!   Go's `if !kernel.IsFirefly(err) { return kernel.NewValidation(err.Error()) }`
//!   (no Firefly error can flow out of the Rust bus).
//! - `GET`: a handler error is the get-order handler's not-found (the
//!   only domain failure it raises) and becomes a 404; anything else
//!   renders as a 500 internal problem, exactly like Go's `WriteError`
//!   fallback.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use firefly_cqrs::{Bus, CqrsError, QueryCache};
use firefly_kernel::FireflyError;
use firefly_starter_core::{Core, CoreConfig};
use firefly_web::{WebError, WebResult};
use http::header::{CONTENT_TYPE, LOCATION};
use http::{HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};

use crate::core::register;
use crate::models::MemoryRepository;

/// The sample's application name — Go's `AppName: "orders-sample"`.
pub const APP_NAME: &str = "orders-sample";

/// Default bind address of the public API server.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Default bind address of the admin (actuator) server. Management
/// endpoints bind on a separate port so `/actuator/*` never leaks onto
/// the public network unintentionally — same rationale as the Go
/// sample.
pub const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:8081";

/// Wires the starter core for this sample — Go's
/// `startercore.New(startercore.Config{AppName: "orders-sample",
/// AppVersion: kernel.Version, StarterName: "starter-core"})`.
pub fn build_core() -> Core {
    Core::new(CoreConfig {
        app_name: APP_NAME.into(),
        app_version: crate::VERSION.into(),
        starter_name: "starter-core".into(),
        ..CoreConfig::default()
    })
}

/// Installs the query cache on the core's bus and registers the orders
/// CQRS handlers over a fresh in-memory repository — the shared wiring
/// of Go's `BuildHandler()` and `main()`. Returns the cache handle so
/// callers can invalidate after out-of-band mutations (Go discards it).
pub fn wire_orders(core: &Core) -> QueryCache {
    let query_cache = QueryCache::new();
    core.bus.use_middleware(query_cache.middleware());
    register(&core.bus, Arc::new(MemoryRepository::new()));
    query_cache
}

/// The public API routes (`POST /api/v1/orders`,
/// `GET /api/v1/orders/:id`) over the given bus — Go's `http.ServeMux`
/// with the two typed handlers. Apply
/// [`Core::apply_middleware`] on top for the canonical chain.
pub fn api_router(bus: Arc<Bus>) -> Router {
    Router::new()
        .route("/api/v1/orders", post(place_order))
        .route("/api/v1/orders/:id", get(get_order))
        .with_state(bus)
}

/// The testable composition root — Go's `BuildHandler()`: returns the
/// full router wired with idempotency, problem rendering, correlation,
/// query caching, and the orders CQRS handlers. The `main()` entry
/// point uses this same wiring.
pub fn build_router() -> Router {
    let core = build_core();
    let _query_cache = wire_orders(&core);
    core.apply_middleware(api_router(Arc::clone(&core.bus)))
}

/// `POST /api/v1/orders` — Go's `placeOrderHandler`. Decodes the JSON
/// body with Go's lenient `json.Decoder` semantics (a 400 problem on
/// malformed JSON, message `invalid json: …`), dispatches the command,
/// and answers `201 Created` with a `Location` header and the DTO body.
async fn place_order(State(bus): State<Arc<Bus>>, body: Bytes) -> WebResult<Response> {
    // Go's `json.NewDecoder(r.Body).Decode(&req)` reads only the first
    // JSON value off the stream (bytes after it are never read, so
    // trailing data is ignored) and treats a top-level `null` as a
    // successful no-op decode that leaves the zero-value struct for
    // domain validation to reject (422, not 400). A streaming
    // `serde_json::Deserializer` — without `end()` — reproduces the
    // first-value-only read, and `Option<T>` maps `null` to the default.
    let mut decoder = serde_json::Deserializer::from_slice(&body);
    let req: crate::interfaces::PlaceOrderRequest = Option::deserialize(&mut decoder)
        .map(Option::unwrap_or_default)
        .map_err(|e| WebError::from(FireflyError::bad_request(format!("invalid json: {e}"))))?;
    let out: crate::interfaces::OrderDto = bus
        .send(req)
        .await
        // Go: `if !kernel.IsFirefly(err) { return kernel.NewValidation(err.Error()) }` —
        // the Rust bus only surfaces CqrsError here, so every dispatch
        // failure renders as a 422 validation problem.
        .map_err(|e| WebError::from(FireflyError::validation(e.to_string())))?;
    let location = format!("/api/v1/orders/{}", out.id);
    let mut res = json_response(StatusCode::CREATED, &out);
    if let Ok(value) = HeaderValue::from_str(&location) {
        res.headers_mut().insert(LOCATION, value);
    }
    Ok(res)
}

/// `GET /api/v1/orders/:id` — Go's `getOrderHandler`. Dispatches the
/// cached query and answers `200 OK` with the DTO body; a handler error
/// (the get-order handler's not-found) renders as a 404 problem.
async fn get_order(State(bus): State<Arc<Bus>>, Path(id): Path<String>) -> WebResult<Response> {
    let out: crate::interfaces::OrderDto = bus
        .query(crate::interfaces::GetOrderQuery { id })
        .await
        .map_err(|e| match e {
            // Go: core returned kernel.NewNotFound through the bus.
            CqrsError::Handler(detail) => WebError::from(FireflyError::not_found(detail)),
            // Go: WriteError renders any non-Firefly error as 500.
            other => WebError::from(FireflyError::internal(other.to_string())),
        })?;
    Ok(json_response(StatusCode::OK, &out))
}

/// Renders `value` as compact JSON with a trailing newline —
/// byte-for-byte what Go's `json.NewEncoder(w).Encode(out)` writes —
/// under `Content-Type: application/json`.
fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    let mut body = serde_json::to_vec(value).unwrap_or_default();
    body.push(b'\n');
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}
