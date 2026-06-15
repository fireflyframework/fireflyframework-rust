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

//! # macro-quickstart — the declarative Firefly Framework service
//!
//! The same orders behaviour as [`firefly-sample-orders`], re-expressed
//! through the `firefly-macros` declarative layer reached over the single
//! [`firefly`] facade. One dependency (`firefly = { workspace = true }`,
//! plus `axum`/`serde`/`tokio` that any service writes against anyway), one
//! prelude glob (`use firefly::prelude::*;`), and the whole CQRS + DI + web +
//! scheduling stack is in scope.
//!
//! Every framework integration here is a *declaration sitting next to the code
//! it describes* — there is no hand-rolled `Bus::register(...)`, no
//! `Router::new().route(...)`, no `Scheduler::fixed_rate(...)` builder. The
//! macros generate it:
//!
//! | Declaration | Macro | Generated wiring |
//! |-------------|-------|------------------|
//! | [`PlaceOrder`] | `#[derive(Command)]` | `impl Message` (with `#[firefly(validate)]` checks) |
//! | [`GetOrder`] | `#[derive(Query)]` | `impl Message` (with `cache_ttl`) |
//! | `OrderHandlers` | `#[derive(Service)]` + `#[handlers]` | a handler **bean** whose `#[command_handler]` / `#[query_handler]` / `#[scheduled]` methods autowire the `OrderStore` and are drained from the container |
//! | [`OrderApi`] impl | `#[rest_controller]` | `OrderApi::routes(state) -> axum::Router` |
//! | [`OrderStore`] | `#[derive(Component)]` | `OrderStore::firefly_register(container)` |
//!
//! [`build_router`] is the testable composition root: it registers the
//! `OrderStore` + `OrderHandlers` beans on a DI [`Container`], drains the bean
//! handlers onto a [`Bus`], and returns the macro-generated [`OrderApi`] router
//! over the resolved store — exactly the building blocks `main()` reuses. Every
//! component is a container-managed bean; there is no process-global.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use firefly::prelude::*;
use serde::{Deserialize, Serialize};

/// The released framework version, surfaced in the startup banner.
pub const VERSION: &str = firefly::VERSION;

// ===========================================================================
// Domain store — `#[derive(Component)]` puts it in the DI container.
// ===========================================================================

/// One stored order.
#[derive(Clone, Serialize)]
pub struct Order {
    /// Server-assigned identifier.
    pub id: String,
    /// Who placed it.
    pub customer: String,
    /// Ordered stock-keeping unit.
    pub sku: String,
    /// How many units.
    pub quantity: u32,
}

/// The in-memory orders store. `#[derive(Component)]` generates
/// `OrderStore::firefly_register(container)`, so a single
/// `register_all!(&container, [OrderStore])` makes it a resolvable singleton
/// bean — the Rust spelling of a Spring `@Component` / pyfly `@component`.
#[derive(Component, Default)]
#[firefly(scope = "singleton")]
pub struct OrderStore {
    inner: std::sync::Mutex<Vec<Order>>,
}

impl OrderStore {
    /// Appends an order, assigning it a sequential id, and returns it.
    pub fn insert(&self, customer: String, sku: String, quantity: u32) -> Order {
        let mut guard = self.inner.lock().expect("orders store poisoned");
        let order = Order {
            id: format!("order-{}", guard.len() + 1),
            customer,
            sku,
            quantity,
        };
        guard.push(order.clone());
        order
    }

    /// Looks an order up by id.
    pub fn get(&self, id: &str) -> Option<Order> {
        self.inner
            .lock()
            .expect("orders store poisoned")
            .iter()
            .find(|o| o.id == id)
            .cloned()
    }

    /// How many orders are stored.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("orders store poisoned").len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A snapshot of every stored order (drives the reactive stream endpoint).
    pub fn all(&self) -> Vec<Order> {
        self.inner.lock().expect("orders store poisoned").clone()
    }
}

// ===========================================================================
// CQRS messages — `#[derive(Command)]` / `#[derive(Query)]`.
// ===========================================================================

/// `POST /api/v1/orders` body / command. `#[derive(Command)]` generates the
/// `impl Message`; `#[firefly(validate)]` makes an empty/zero field fail
/// validation before the handler ever runs.
#[derive(Clone, Serialize, Deserialize, Command)]
pub struct PlaceOrder {
    /// Who is placing the order — required.
    #[firefly(validate)]
    pub customer: String,
    /// The SKU — required.
    #[firefly(validate)]
    pub sku: String,
    /// Units to buy — required (zero fails validation).
    #[firefly(validate)]
    pub quantity: u32,
}

/// `GET /api/v1/orders/:id` query. `#[firefly(cache_ttl = "30s")]` is reflected
/// on the generated `Message::cache_ttl`, so a `QueryCache` memoises reads for
/// 30 seconds.
#[derive(Clone, Serialize, Query)]
#[firefly(cache_ttl = "30s")]
pub struct GetOrder {
    /// The order id to fetch.
    pub id: String,
}

/// The wire shape both messages resolve to. `Bus` results must be
/// `Clone + Send + Sync + 'static`.
#[derive(Clone, Serialize, Deserialize)]
pub struct OrderView {
    /// Server-assigned identifier.
    pub id: String,
    /// Who placed it.
    pub customer: String,
    /// Ordered SKU.
    pub sku: String,
    /// Units bought.
    pub quantity: u32,
}

impl From<Order> for OrderView {
    fn from(o: Order) -> Self {
        Self {
            id: o.id,
            customer: o.customer,
            sku: o.sku,
            quantity: o.quantity,
        }
    }
}

// ===========================================================================
// Handler bean — a `#[derive(Service)]` whose methods are the CQRS handlers and
// the `#[scheduled]` task, autowiring the `OrderStore` from the DI container.
// ===========================================================================

/// The **handler bean** — Spring's `@Component` carrying the command / query
/// handlers and the `@Scheduled` task. Its only collaborator, the
/// [`OrderStore`], is `#[autowired]` from the container, and `#[handlers]`
/// registers each method on the bus / scheduler — so a handler reaches the store
/// through `self`, with no process-global.
#[derive(Service)]
struct OrderHandlers {
    /// The orders store every handler operates on (autowired).
    #[autowired]
    store: Arc<OrderStore>,
}

#[handlers]
impl OrderHandlers {
    /// Handles [`PlaceOrder`].
    #[command_handler]
    async fn place_order(&self, cmd: PlaceOrder) -> Result<OrderView, CqrsError> {
        let order = self.store.insert(cmd.customer, cmd.sku, cmd.quantity);
        Ok(order.into())
    }

    /// Handles [`GetOrder`]; a missing id is a `CqrsError::Handler`, which the
    /// controller renders as a 404 problem.
    #[query_handler]
    async fn get_order(&self, q: GetOrder) -> Result<OrderView, CqrsError> {
        self.store
            .get(&q.id)
            .map(OrderView::from)
            .ok_or_else(|| CqrsError::handler(format!("order {} not found", q.id)))
    }

    /// A housekeeping task — `#[scheduled]` on a bean method (Spring's
    /// `@Scheduled` on a `@Component`). Here it just observes the store; a real
    /// one would evict stale entries.
    #[scheduled(fixed_rate = "60s", initial_delay = "5s")]
    async fn sweep_stale_orders(&self) -> Result<(), std::io::Error> {
        let _kept = self.store.len();
        Ok(())
    }
}

// ===========================================================================
// REST controller — `#[rest_controller]` + `#[get]` / `#[post]`.
// ===========================================================================

/// The orders HTTP surface. It carries the [`Bus`] as shared state and
/// dispatches every request through CQRS — handlers, validation, and the query
/// cache all run inside the bus.
#[derive(Clone)]
pub struct OrderApi {
    /// The command/query bus the controller dispatches through.
    pub bus: Arc<Bus>,
    /// The orders store the reactive read endpoints stream from (resolved from
    /// the DI container in [`build_router`]).
    pub store: Arc<OrderStore>,
}

/// `#[rest_controller(path = "...")]` turns this `impl` block into a generated
/// `OrderApi::routes(state) -> axum::Router`. Each method carries one
/// `#[get]` / `#[post]` mapping and returns `WebResult<T>`, so handler errors
/// render as RFC 9457 `application/problem+json`.
#[rest_controller(path = "/api/v1/orders")]
impl OrderApi {
    /// `POST /api/v1/orders` — place an order. Validation failures surface as
    /// 422 problems; success answers `200 OK` with the created view.
    #[post("")]
    async fn create(
        State(api): State<OrderApi>,
        Json(body): Json<PlaceOrder>,
    ) -> WebResult<Json<OrderView>> {
        let view: OrderView = api
            .bus
            .send(body)
            .await
            .map_err(|e| WebError::from(FireflyError::validation(e.to_string())))?;
        Ok(Json(view))
    }

    /// `GET /api/v1/orders/:id` — fetch an order. An unknown id renders as a
    /// 404 problem; cached for 30s by the query cache.
    #[get("/:id")]
    async fn fetch(
        State(api): State<OrderApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<OrderView>> {
        let view: OrderView = api.bus.query(GetOrder { id }).await.map_err(|e| match e {
            CqrsError::Handler(detail) => WebError::from(FireflyError::not_found(detail)),
            other => WebError::from(FireflyError::internal(other.to_string())),
        })?;
        Ok(Json(view))
    }

    /// `GET /api/v1/orders/:id/reactive` — the same fetch, returned as a
    /// reactive `Mono<OrderView>`: an unknown id resolves the `Mono` empty,
    /// which renders a 404 problem (Spring WebFlux's `Mono<T>` controller
    /// return).
    #[get("/:id/reactive")]
    async fn fetch_reactive(
        State(api): State<OrderApi>,
        Path(id): Path<String>,
    ) -> MonoJson<OrderView> {
        match api.store.get(&id) {
            Some(order) => MonoJson(Mono::just(OrderView::from(order))),
            None => MonoJson(Mono::empty()),
        }
    }

    /// `GET /api/v1/orders/stream` — every order as a backpressured
    /// `application/x-ndjson` stream from a `Flux<OrderView>` (Spring WebFlux's
    /// streaming `Flux<T>` return).
    #[get("/stream")]
    async fn stream(State(api): State<OrderApi>) -> NdJson<OrderView> {
        NdJson(Flux::from_iter(
            api.store.all().into_iter().map(OrderView::from),
        ))
    }

    /// `GET /api/v1/orders/live` — the same orders as a `text/event-stream` of
    /// Server-Sent Events from a `Flux<OrderView>`.
    #[get("/live")]
    async fn live(State(api): State<OrderApi>) -> Sse<OrderView> {
        Sse(Flux::from_iter(
            api.store.all().into_iter().map(OrderView::from),
        ))
    }
}

// ===========================================================================
// Composition root — reuses the macro-generated wiring.
// ===========================================================================

/// Wires the service end to end and returns the macro-generated router.
///
/// 1. register the [`OrderStore`] + [`OrderHandlers`] beans on a DI
///    [`Container`] (their `firefly_register`s are generated by
///    `#[derive(Component)]` / `#[derive(Service)]`),
/// 2. install a [`QueryCache`] and drain the **bean** command/query handlers
///    onto a [`Bus`] (the framework resolves `OrderHandlers` from the container
///    and registers its `#[command_handler]` / `#[query_handler]` methods),
/// 3. return [`OrderApi::routes`] over the bus + the resolved store.
///
/// The same building blocks back `main()`; tests call this directly. Every
/// component is a container-managed bean — no process-global.
pub fn build_router() -> axum::Router {
    let container = Container::new();
    firefly::register_all!(&container, [OrderStore, OrderHandlers]);
    let store = container
        .resolve::<OrderStore>()
        .expect("OrderStore bean resolves");

    let bus = Arc::new(Bus::new());
    // The `#[firefly(validate)]` declarations on `PlaceOrder` run here; the
    // query cache honours `GetOrder`'s `#[firefly(cache_ttl = "30s")]`.
    bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());
    let cache = firefly::cqrs::QueryCache::new();
    bus.use_middleware(cache.middleware());
    firefly::cqrs::register_discovered_handler_beans(&bus, &container);

    OrderApi::routes(OrderApi { bus, store })
}

/// Registers the bean `#[scheduled]` task on a fresh scheduler and returns it —
/// `main()` starts it; tests assert it registered. The framework resolves
/// [`OrderHandlers`] from the container and schedules its `sweep_stale_orders`
/// method.
pub fn build_scheduler() -> Arc<Scheduler> {
    let container = Container::new();
    firefly::register_all!(&container, [OrderStore, OrderHandlers]);
    let scheduler = Arc::new(Scheduler::new());
    firefly::scheduling::register_discovered_scheduled_beans(&scheduler, &container);
    scheduler
}
