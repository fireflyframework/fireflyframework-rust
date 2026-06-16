// A single compile-pass case exercising every `#[http_client]` return shape and
// every binding rule against the real facade. If any expansion stops compiling,
// trybuild reports it here.

use std::sync::Arc;

use firefly::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct CreateOrder {
    pub sku: String,
    pub qty: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub sku: String,
}

// A custom error that converts from `ClientError`, for the `Result<T, E>` shape.
#[derive(Debug)]
pub struct ApiError(String);

impl From<ClientError> for ApiError {
    fn from(e: ClientError) -> Self {
        ApiError(e.to_string())
    }
}

#[http_client(path = "/api/v1/orders", name = "orders", accept = "application/json", bean)]
pub trait OrdersClient {
    // `:id` name-matches the `id` arg -> path var.
    #[get("/:id")]
    async fn get_order(&self, id: String) -> Result<Order, ClientError>;

    // Inferred query params; `Option` omits when `None`.
    #[get("/")]
    async fn list(&self, status: String, page: Option<u32>) -> Result<Vec<Order>, ClientError>;

    // Lone non-scalar arg -> JSON body; one explicit header.
    #[post("/")]
    async fn create(
        &self,
        #[header("X-Tenant")] tenant: String,
        order: CreateOrder,
    ) -> Result<Order, ClientError>;

    // 204 / empty body -> unit.
    #[delete("/:id")]
    async fn cancel(&self, id: String) -> Result<(), ClientError>;

    // Option success type.
    #[get("/:id/maybe")]
    async fn maybe(&self, id: String) -> Result<Option<Order>, ClientError>;

    // Explicit binding attributes + repeated query (Vec) + optional header.
    #[put("/:order_id")]
    async fn replace(
        &self,
        #[path("order_id")] order_id: String,
        #[query("tag")] tags: Vec<String>,
        #[header("X-Trace")] trace: Option<String>,
        #[body] order: CreateOrder,
    ) -> Result<Order, ClientError>;

    // Custom error type (E: From<ClientError>).
    #[get("/:id/strict")]
    async fn strict(&self, id: String) -> Result<Order, ApiError>;

    // The raw exchange escape hatch via the Result form.
    #[get("/:id/raw")]
    async fn raw(&self, id: String) -> Result<WebClientResponse, ClientError>;

    // Reactive-first: non-async Mono / Flux.
    #[get("/:id")]
    fn get_order_mono(&self, id: String) -> Mono<Order>;

    #[get("/stream")]
    fn stream(&self) -> Flux<Order>;

    // Generic verb form.
    #[request(method = "HEAD", path = "/:id")]
    async fn head(&self, id: String) -> Result<(), ClientError>;
}

fn main() {
    // Construction surfaces exist and the struct is named `<Trait>Impl`.
    let api = OrdersClientImpl::new("https://orders.svc");
    let _clone = api.clone();
    let web = firefly::client::new_web_client("https://orders.svc").build();
    let _injected = OrdersClientImpl::with_client(web);

    // The DI registrar exists (from `bean`).
    let _reg: fn(&Container) = OrdersClientImpl::firefly_register;

    // The trait object resolves through `dyn` (object-safe).
    fn _autowire(_: Arc<dyn OrdersClient>) {}

    // Reference the async + reactive methods so they are type-checked.
    let _f = OrdersClientImpl::get_order;
    let _m = OrdersClientImpl::get_order_mono;
    let _s = OrdersClientImpl::stream;
}
