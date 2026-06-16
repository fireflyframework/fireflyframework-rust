// In-process axum round-trip for `#[http_client]`.
//
// The mirror-image parity test: a generated `<Trait>Impl` client is pointed at a
// tiny axum server bound on an ephemeral port, and each return shape + binding is
// asserted end-to-end on the wire — path-var substituted & percent-encoded,
// query present / omitted for `Option`, header set, body serialized, `Vec`
// decoded, `()` on a 204, a 404 surfacing as `ClientError::Problem` whose
// `.is_not_found()` is true, and an NDJSON `Flux` collected via `.collect_list()`.
//
// This proves the deliberate `:id` client<->server convention lines up over HTTP,
// using the same firefly facade a real consumer compiles against.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use firefly::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
struct Order {
    id: String,
    sku: String,
}

#[derive(Serialize)]
struct CreateOrder {
    sku: String,
    qty: u32,
}

// ---------------------------------------------------------------------------
// The client under test — exercises every binding + return shape.
// ---------------------------------------------------------------------------

#[http_client(path = "/api/v1/orders")]
trait OrdersClient {
    // Path var (name-matched), percent-encoded.
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    // Query params: `status` always present, `page` (Option) omitted when None.
    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    // Explicit header + JSON body.
    #[post("/")]
    async fn create(
        &self,
        #[header("X-Tenant")] tenant: String,
        order: CreateOrder,
    ) -> Result<Order, ClientError>;

    // 204 -> unit.
    #[delete("/:id")]
    async fn cancel(&self, id: String) -> Result<(), ClientError>;

    // 404 -> ClientError::Problem.
    #[get("/missing/:id")]
    async fn get_missing(&self, id: String) -> Result<Order, ClientError>;

    // 204/empty -> Ok(None) (the `Option<T>` empty-body fold).
    #[get("/opt/:id")]
    async fn find_opt(&self, id: String) -> Result<Option<Order>, ClientError>;

    // Custom error type via `E: From<ClientError>` — a 404 maps through map_err.
    #[get("/missing/:id")]
    async fn get_custom(&self, id: String) -> Result<Order, CustomError>;

    // Reactive NDJSON stream.
    #[get("/stream")]
    fn stream(&self) -> Flux<Order>;
}

// A bespoke error wrapping `ClientError`, proving the `map_err(From::from)` fold
// routes a failure through a user-defined error type.
#[derive(Debug)]
struct CustomError(ClientError);

impl From<ClientError> for CustomError {
    fn from(e: ClientError) -> Self {
        CustomError(e)
    }
}

// ---------------------------------------------------------------------------
// The in-process server: handlers echo back what they received so the client's
// wire encoding is observable in the asserted response.
// ---------------------------------------------------------------------------

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    // The server sees the *decoded* path segment, so a percent-encoded `a/b`
    // arrives back as `a/b` here — proving the client escaped it into one
    // segment rather than injecting `/api/v1/orders/a/b`.
    Json(Order {
        id,
        sku: "SKU-1".into(),
    })
}

#[derive(Deserialize)]
struct ListQuery {
    status: String,
    page: Option<u32>,
}

async fn list(Query(q): Query<ListQuery>) -> Json<Vec<Order>> {
    // Echo the received query into the response so the test can assert which
    // params were sent.
    Json(vec![Order {
        id: format!("status={}", q.status),
        sku: format!("page={:?}", q.page),
    }])
}

async fn create(headers: HeaderMap, Json(body): Json<serde_json::Value>) -> Json<Order> {
    let tenant = headers
        .get("X-Tenant")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("<none>")
        .to_string();
    Json(Order {
        id: tenant,
        sku: body["sku"].as_str().unwrap_or_default().to_string(),
    })
}

async fn cancel(Path(_id): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

// 204 / empty body — the client folds it to `Ok(None)` / `Ok(vec![])` per the
// success type.
async fn empty_body() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn get_missing(Path(_id): Path<String>) -> Response {
    // An RFC 7807 problem body, so the client decodes it into a FireflyError.
    (
        StatusCode::NOT_FOUND,
        [("content-type", "application/problem+json")],
        Json(json!({
            "type": "ORDER_NOT_FOUND",
            "title": "Not Found",
            "status": 404,
            "detail": "no such order",
        })),
    )
        .into_response()
}

async fn stream() -> Response {
    let body = "{\"id\":\"1\",\"sku\":\"A\"}\n{\"id\":\"2\",\"sku\":\"B\"}\n";
    (
        StatusCode::OK,
        [("content-type", "application/x-ndjson")],
        body,
    )
        .into_response()
}

/// Spawns the server on an ephemeral port and returns its base URL.
async fn spawn_server() -> String {
    let app = Router::new()
        .route("/api/v1/orders/stream", get(stream))
        .route("/api/v1/orders/missing/:id", get(get_missing))
        .route("/api/v1/orders/opt/:id", get(empty_body))
        .route("/api/v1/orders/:id", get(get_order).delete(cancel))
        // `#[get("/")]` over base `/api/v1/orders` joins to `/api/v1/orders`
        // (the shared `join_path` trims the trailing slash), exactly as the
        // `#[rest_controller]` server side does — so the route has no trailing
        // slash here either.
        .route("/api/v1/orders", get(list).post(create));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn round_trip_get_with_encoded_path_var() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);

    // A plain id round-trips.
    let order = api.get_order("42".into()).await.expect("get_order");
    assert_eq!(order.id, "42");
    assert_eq!(order.sku, "SKU-1");

    // A slash in the id is percent-encoded into a single segment (decoded back
    // to `a/b` server-side), not injected as an extra path segment.
    let order = api.get_order("a/b".into()).await.expect("get encoded");
    assert_eq!(order.id, "a/b");
}

#[tokio::test]
async fn round_trip_query_present_and_omitted() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);

    // `page = Some` -> both query params present.
    let with_page = api
        .list("open".into(), Some(2))
        .await
        .expect("list with page");
    assert_eq!(with_page.len(), 1);
    assert_eq!(with_page[0].id, "status=open");
    assert_eq!(with_page[0].sku, "page=Some(2)");

    // `page = None` -> the `page` query param is omitted entirely.
    let without_page = api.list("open".into(), None).await.expect("list no page");
    assert_eq!(without_page[0].sku, "page=None");
}

#[tokio::test]
async fn round_trip_header_and_body() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);

    let created = api
        .create(
            "acme".into(),
            CreateOrder {
                sku: "SKU-9".into(),
                qty: 3,
            },
        )
        .await
        .expect("create");
    // The header was sent (echoed into `id`) and the body serialized (echoed
    // into `sku`).
    assert_eq!(created.id, "acme");
    assert_eq!(created.sku, "SKU-9");
}

#[tokio::test]
async fn round_trip_unit_on_204() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);
    // A 204 No Content folds to `Ok(())`.
    api.cancel("42".into()).await.expect("cancel");
}

#[tokio::test]
async fn round_trip_404_is_not_found_problem() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);

    let err = api
        .get_missing("99".into())
        .await
        .expect_err("expected a 404");
    match &err {
        ClientError::Problem(fe) => {
            assert_eq!(fe.status, 404);
            assert_eq!(fe.code, "ORDER_NOT_FOUND");
        }
        other => panic!("expected ClientError::Problem, got {other:?}"),
    }
    assert!(err.is_not_found(), "404 should classify as not-found");
}

#[tokio::test]
async fn round_trip_empty_body_folds_to_option_none() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);
    // A 204 / empty body on a `Result<Option<T>, _>` method folds to `Ok(None)`.
    let found = api.find_opt("42".into()).await.expect("find_opt ok");
    assert_eq!(found, None);
}

// NOTE: the `Result<Vec<T>, _>` empty-body fold (`Ok(vec![])`) is intentionally
// *not* covered by a wire test. The `WebClient` decodes an empty 2xx body as JSON
// `null` (so an empty `Option<T>` body deserializes straight to `None`, asserted
// above), but `null` is not a valid sequence, so `body_to_mono::<Vec<T>>` fails
// with `CLIENT_DECODE` before the macro's `Ok(vec![])` empty arm can run. That
// fold arm only fires when the `Mono` completes *truly* empty (`Ok(None)`), which
// `body_to_mono` never does — it always decodes a value. Exercising it would
// require a server returning a literal `[]`, which is not an empty body. The
// macro's `VecEmpty` arm is still unit-covered by the trybuild/expansion corpus.

#[tokio::test]
async fn round_trip_custom_error_maps_through_map_err() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);
    // A 404 on a `Result<T, CustomError>` method routes through
    // `map_err(<CustomError as From<ClientError>>::from)`, so the failure arrives
    // as the user's error type wrapping the classified `ClientError::Problem`.
    let err = api
        .get_custom("99".into())
        .await
        .expect_err("expected a 404 mapped to CustomError");
    let CustomError(inner) = err;
    match &inner {
        ClientError::Problem(fe) => assert_eq!(fe.status, 404),
        other => panic!("expected ClientError::Problem, got {other:?}"),
    }
    assert!(inner.is_not_found(), "404 should classify as not-found");
}

#[tokio::test]
async fn round_trip_flux_ndjson_stream() {
    let base = spawn_server().await;
    let api = OrdersClientImpl::new(base);

    let orders = api
        .stream()
        .collect_list()
        .block()
        .await
        .expect("stream ok")
        .expect("non-empty list");
    assert_eq!(
        orders,
        vec![
            Order {
                id: "1".into(),
                sku: "A".into()
            },
            Order {
                id: "2".into(),
                sku: "B".into()
            },
        ]
    );
}

// ---------------------------------------------------------------------------
// DI: the `bean` flag binds `dyn Trait`, so the trait object resolves from the
// container after a shared `WebClient` bean is registered. Proves the autowire
// thunk wires the trait object end-to-end.
// ---------------------------------------------------------------------------

#[http_client(path = "/api/v1/orders", bean)]
trait OrdersBeanClient {
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;
}

#[tokio::test]
async fn di_resolves_trait_object_through_bean_bind() {
    let base = spawn_server().await;

    let container = Container::new();
    // A `WebClient` bean pointed at the in-process server — the dependency the
    // generated `firefly_register` thunk resolves.
    container.register_instance(firefly::client::new_web_client(base).build());
    // `scan()` discovers the `#[http_client(bean)]` inventory thunk and runs its
    // registrar (binding `dyn OrdersBeanClient`).
    container.scan();

    // The trait object resolves via the `bean`/bind seam ...
    let client: Arc<dyn OrdersBeanClient> =
        Container::resolve::<dyn OrdersBeanClient>(&container).expect("resolve dyn");
    // ... and a call through it round-trips over HTTP.
    let order = client.get_order("7".into()).await.expect("get via dyn");
    assert_eq!(order.id, "7");
    assert_eq!(order.sku, "SKU-1");
}
