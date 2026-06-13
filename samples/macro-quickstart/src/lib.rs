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
//! | [`place_order`] | `#[command_handler]` | `register_place_order(bus)` |
//! | [`get_order`] | `#[query_handler]` | `register_get_order(bus)` |
//! | [`OrderApi`] impl | `#[rest_controller]` | `OrderApi::routes(state) -> axum::Router` |
//! | [`OrderStore`] | `#[derive(Component)]` | `OrderStore::firefly_register(container)` |
//! | [`sweep_stale_orders`] | `#[scheduled]` | `schedule_sweep_stale_orders(scheduler)` |
//!
//! [`build_router`] is the testable composition root: it resolves the store
//! from the DI [`Container`], registers the handlers on a [`Bus`], and returns
//! the macro-generated [`OrderApi`] router — exactly the building blocks
//! `main()` reuses.

use std::sync::{Arc, OnceLock};

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
}

/// The store the `#[command_handler]` / `#[query_handler]` / `#[scheduled]`
/// free fns operate on. Rust free fns cannot capture wiring state, so the
/// resolved [`OrderStore`] bean is published here once at startup — the
/// macros then call straight through it.
static STORE: OnceLock<Arc<OrderStore>> = OnceLock::new();

/// Returns the wired store, or a fresh empty one if `build_router` has not run
/// yet (keeps the scheduled task and handlers total).
fn store() -> Arc<OrderStore> {
    STORE
        .get_or_init(|| Arc::new(OrderStore::default()))
        .clone()
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
// CQRS handlers — `#[command_handler]` / `#[query_handler]`.
// ===========================================================================

/// Handles [`PlaceOrder`]. `#[command_handler]` generates
/// `register_place_order(bus)`; dispatching a `PlaceOrder` routes here.
#[command_handler]
pub async fn place_order(cmd: PlaceOrder) -> Result<OrderView, CqrsError> {
    let order = store().insert(cmd.customer, cmd.sku, cmd.quantity);
    Ok(order.into())
}

/// Handles [`GetOrder`]. `#[query_handler]` generates `register_get_order(bus)`;
/// a missing id is a `CqrsError::Handler`, which the controller renders as a
/// 404 problem.
#[query_handler]
pub async fn get_order(q: GetOrder) -> Result<OrderView, CqrsError> {
    store()
        .get(&q.id)
        .map(OrderView::from)
        .ok_or_else(|| CqrsError::handler(format!("order {} not found", q.id)))
}

// ===========================================================================
// Scheduled task — `#[scheduled]`.
// ===========================================================================

/// A housekeeping task. `#[scheduled(fixed_rate = "60s")]` generates
/// `schedule_sweep_stale_orders(scheduler)`; the framework calls it on a tick.
/// (Here it just observes the store; a real one would evict stale entries.)
#[scheduled(fixed_rate = "60s", initial_delay = "5s")]
pub async fn sweep_stale_orders() -> Result<(), std::io::Error> {
    let _kept = store().len();
    Ok(())
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
}

// ===========================================================================
// Composition root — reuses the macro-generated wiring.
// ===========================================================================

/// Wires the service end to end and returns the macro-generated router.
///
/// 1. resolve the [`OrderStore`] bean from the DI [`Container`] (its
///    `firefly_register` was generated by `#[derive(Component)]`),
/// 2. publish it for the free-fn handlers / scheduled task,
/// 3. install a [`QueryCache`] and register the generated handlers on a
///    [`Bus`] (`register_place_order` / `register_get_order`),
/// 4. return [`OrderApi::routes`] — the router `#[rest_controller]` generated.
///
/// The same building blocks back `main()`; tests call this directly.
pub fn build_router() -> axum::Router {
    let container = Container::new();
    firefly::register_all!(&container, [OrderStore]);
    let store = container
        .resolve::<OrderStore>()
        .expect("OrderStore bean resolves");
    let _ = STORE.set(store);

    let bus = Arc::new(Bus::new());
    // The `#[firefly(validate)]` declarations on `PlaceOrder` run here; the
    // query cache honours `GetOrder`'s `#[firefly(cache_ttl = "30s")]`.
    bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());
    let cache = firefly::cqrs::QueryCache::new();
    bus.use_middleware(cache.middleware());
    register_place_order(&bus);
    register_get_order(&bus);

    OrderApi::routes(OrderApi { bus })
}

/// Registers the [`sweep_stale_orders`] task on a fresh scheduler and returns
/// it — `main()` starts it; tests assert it registered.
pub fn build_scheduler() -> Arc<Scheduler> {
    let scheduler = Arc::new(Scheduler::new());
    schedule_sweep_stale_orders(&scheduler);
    scheduler
}
