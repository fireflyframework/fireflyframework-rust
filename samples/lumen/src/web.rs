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

//! The HTTP surface and the **composition root** — the macro-generated
//! `#[rest_controller]` router plus the wiring that ties web + cqrs + eda +
//! eventsourcing + saga + security + actuator together (book chapters 4
//! "First Endpoints", 15 "Observability & Admin", 18 "Production").
//!
//! [`build_app`] assembles a [`LumenApp`] over in-memory infrastructure (the
//! default for tests and a no-infra `cargo run`); [`build_router`] returns just
//! its public router for the HTTP tests.
//!
//! ## Endpoints
//!
//! | Method & path                          | Handler                | Notes                       |
//! |----------------------------------------|------------------------|-----------------------------|
//! | `POST /api/v1/wallets`                 | [`WalletApi::open`]     | CQRS `OpenWallet` → 201      |
//! | `GET  /api/v1/wallets/:id`             | [`WalletApi::get`]      | CQRS `GetWallet` (cached)    |
//! | `POST /api/v1/wallets/:id/deposit`     | [`WalletApi::deposit`]  | CQRS `Deposit`               |
//! | `POST /api/v1/wallets/:id/withdraw`    | [`WalletApi::withdraw`] | CQRS `Withdraw`              |
//! | `POST /api/v1/transfers`               | [`WalletApi::transfer`] | Saga (debit→credit)          |
//! | `GET  /api/v1/wallets/:id/events`      | `stream_events`        | reactive stream (feature `streaming`) |

// `firefly::web::WebError` is a large enum by design; returning it from the
// small handler helpers is what makes `?`-into-`WebResult<T>` ergonomic across
// every handler, exactly as the framework intends.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::{Path, State};
#[cfg(feature = "streaming")]
use axum::response::Response;
use axum::Json;
use firefly::cqrs::QueryCache;
use firefly::eventsourcing::MemoryEventStore;
use firefly::prelude::*;
use firefly::starter_web::WebStack;
use firefly::web::{WebError, WebResult};
use serde::Deserialize;

use crate::commands::{Deposit, GetWallet, OpenWallet, Withdraw};
use crate::domain::{DomainError, WalletView};
use crate::ledger::{self, Ledger, ReadModel};
use crate::transfer::{run_transfer, TransferError, TransferRequest, TransferResult};

/// Lumen's application name (banner + `/actuator/info`).
pub const APP_NAME: &str = "lumen";

/// The released framework version, surfaced in the banner.
pub const VERSION: &str = firefly::VERSION;

/// The fully-assembled Lumen application: the web-tier stack (with the
/// security filter chain attached), the CQRS bus, the ledger application
/// service, and the read model. `main()` reads [`LumenApp::web`] for the
/// actuator surface + lifecycle; the HTTP tests read [`LumenApp::router`].
pub struct LumenApp {
    /// The web-tier starter (CORS / security-headers / correlation / metrics
    /// on by default), `Deref`-ing to the core.
    pub web: WebStack,
    /// The CQRS bus with the query cache + handlers wired.
    pub bus: Arc<Bus>,
    /// The shared application service (event store + broker).
    pub ledger: Ledger,
    /// The read model the projection feeds and `GetWallet` serves.
    pub read_model: Arc<ReadModel>,
    /// The query-cache handle, so a mutation can invalidate the cached
    /// `GetWallet` view for read-after-write consistency.
    pub query_cache: QueryCache,
}

impl LumenApp {
    /// Builds the public router: the macro-generated wallet routes wrapped in
    /// the web middleware chain **and** the JWT bearer + RBAC security layers.
    pub fn router(&self) -> axum::Router {
        let state = WalletApi {
            bus: Arc::clone(&self.bus),
            ledger: self.ledger.clone(),
            query_cache: self.query_cache.clone(),
        };
        #[allow(unused_mut)]
        let mut routes = WalletApi::routes(state.clone());
        #[cfg(feature = "streaming")]
        {
            routes = routes.merge(streaming_router(state));
        }
        #[cfg(not(feature = "streaming"))]
        let _ = state;
        let (bearer, _chain) = crate::security::security_layers();
        // The WebStack carries the FilterChain (set in build_app); layer the
        // bearer auth on the outside so the chain sees a populated
        // Authentication.
        self.web.apply_middleware(routes).layer(bearer)
    }
}

/// Assembles a [`LumenApp`] over **in-memory** infrastructure — the default
/// for tests and a no-infra run.
pub async fn build_app() -> LumenApp {
    let (_bearer, chain) = crate::security::security_layers();
    let web = WebStack::new(firefly::starter_web::CoreConfig {
        app_name: APP_NAME.into(),
        app_version: VERSION.into(),
        ..Default::default()
    })
    .with_security(chain);

    let bus = Arc::clone(&web.bus);
    let store: Arc<dyn firefly::eventsourcing::EventStore> = Arc::new(MemoryEventStore::new());
    let broker = Arc::clone(&web.broker);
    let read_model = Arc::new(ReadModel::default());

    // Read-side caching on the bus (honours GetWallet's 30s cache_ttl). The
    // handle is kept so a mutation can invalidate it for read-after-write.
    let query_cache = QueryCache::new();
    bus.use_middleware(query_cache.middleware());
    // Validation middleware enforces the `#[firefly(validate)]` checks.
    bus.use_middleware(firefly::cqrs::ValidationMiddleware::new());

    // Publish the handler + projection collaborators (free-fn macro handlers
    // and the `#[event_listener]` reach them through module-local statics).
    // `bind` returns the *effective* ledger/read-model — the first build's, on
    // every subsequent build — so the controller's saga, the projection, and
    // the free-fn handlers all share one ledger (store + broker) and one read
    // model. Everything downstream wires against the effective collaborators.
    let (ledger, read_model) = crate::commands::bind(
        Ledger::new(Arc::clone(&store), Arc::clone(&broker)),
        read_model,
    );
    ledger::bind_projection(Arc::clone(ledger.store()), Arc::clone(&read_model));
    crate::commands::register(&bus);
    // Subscribe the projection to the *effective* ledger's broker, so the
    // events the handlers publish are the events the projection consumes.
    ledger::subscribe_project_wallet_event(ledger.broker().as_ref())
        .await
        .expect("projection subscription");

    LumenApp {
        web,
        bus,
        ledger,
        read_model,
        query_cache,
    }
}

/// The testable composition root: the full public router of an in-memory
/// [`LumenApp`], wired with the web middleware + JWT security.
pub async fn build_router() -> axum::Router {
    build_app().await.router()
}

// ---------------------------------------------------------------------------
// REST controller — `#[rest_controller]` + `#[get]` / `#[post]`.
// ---------------------------------------------------------------------------

/// The wallet HTTP surface. It carries the [`Bus`] (CQRS dispatch) and the
/// [`Ledger`] (the saga + streaming endpoint drive it directly).
#[derive(Clone)]
pub struct WalletApi {
    /// The command/query bus the controller dispatches through.
    pub bus: Arc<Bus>,
    /// The application service the transfer saga and event stream use.
    pub ledger: Ledger,
    /// The query cache, invalidated after a mutation so a read-after-write
    /// never serves a stale balance within the 30s `GetWallet` TTL.
    pub query_cache: QueryCache,
}

/// A `{ "amount": <i64> }` request body for deposit / withdraw.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AmountBody {
    amount: i64,
}

/// `#[rest_controller(path = "...")]` generates `WalletApi::routes(state) ->
/// axum::Router`. Each method carries one verb mapping and returns
/// `WebResult<T>`, so a handler error renders as RFC 9457
/// `application/problem+json`.
#[rest_controller(path = "/api/v1")]
impl WalletApi {
    /// `POST /api/v1/wallets` — open a wallet. Validation failures surface as
    /// 422 problems; success answers `201 Created` with the view.
    #[post("/wallets")]
    async fn open(
        State(api): State<WalletApi>,
        Json(body): Json<OpenWallet>,
    ) -> WebResult<(axum::http::StatusCode, Json<WalletView>)> {
        let view: WalletView = api.bus.send(body).await.map_err(cqrs_to_web)?;
        Ok((axum::http::StatusCode::CREATED, Json(view)))
    }

    /// `GET /api/v1/wallets/:id` — fetch the read-model view (cached 30s). An
    /// unknown id renders as a 404 problem.
    #[get("/wallets/:id")]
    async fn get(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
    ) -> WebResult<Json<WalletView>> {
        let view: WalletView = api.bus.query(GetWallet { id }).await.map_err(cqrs_to_web)?;
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/deposit` — credit a wallet.
    #[post("/wallets/:id/deposit")]
    async fn deposit(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
        Json(body): Json<AmountBody>,
    ) -> WebResult<Json<WalletView>> {
        let cmd = Deposit {
            wallet_id: id,
            amount: body.amount,
        };
        let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
        api.query_cache.invalidate_type::<GetWallet>();
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/withdraw` — debit a wallet.
    #[post("/wallets/:id/withdraw")]
    async fn withdraw(
        State(api): State<WalletApi>,
        Path(id): Path<String>,
        Json(body): Json<AmountBody>,
    ) -> WebResult<Json<WalletView>> {
        let cmd = Withdraw {
            wallet_id: id,
            amount: body.amount,
        };
        let view: WalletView = api.bus.send(cmd).await.map_err(cqrs_to_web)?;
        api.query_cache.invalidate_type::<GetWallet>();
        Ok(Json(view))
    }

    /// `POST /api/v1/transfers` — run a money transfer as a saga. A clean
    /// rollback is a business failure surfaced as a 422 carrying the cause
    /// (e.g. "insufficient funds"); success answers `200 OK`.
    #[post("/transfers")]
    async fn transfer(
        State(api): State<WalletApi>,
        Json(body): Json<TransferRequest>,
    ) -> WebResult<Json<TransferResult>> {
        let result = run_transfer(&api.ledger, &body)
            .await
            .map_err(|e| match e {
                TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
                TransferError::Compensated(detail) => {
                    WebError::from(FireflyError::validation(detail))
                }
            })?;
        // A transfer touches both wallets' views; invalidate the family.
        api.query_cache.invalidate_type::<GetWallet>();
        Ok(Json(result))
    }
}

// ---------------------------------------------------------------------------
// Reactive streaming endpoint (feature `streaming`).
//
// `GET /api/v1/wallets/:id/events` lives on a separate, feature-gated router
// merged in `LumenApp::router` rather than inside the `#[rest_controller]`
// impl, so the macro-generated `routes()` never references a method that is
// compiled out when the feature is off.
// ---------------------------------------------------------------------------

/// Query params for the streaming endpoint: `?format=sse` switches from the
/// default NDJSON to Server-Sent Events.
#[cfg(feature = "streaming")]
#[derive(Debug, Default, Deserialize)]
pub struct StreamParams {
    /// `sse` to receive the stream as Server-Sent Events.
    #[serde(default)]
    pub format: Option<String>,
}

/// Builds the streaming sub-router (`GET /api/v1/wallets/:id/events`) over the
/// controller state.
#[cfg(feature = "streaming")]
fn streaming_router(api: WalletApi) -> axum::Router {
    axum::Router::new()
        .route(
            "/api/v1/wallets/:id/events",
            axum::routing::get(stream_events),
        )
        .with_state(api)
}

/// The **reactive streaming** handler. It builds a `Flux<WalletEvent>` over the
/// wallet's persisted stream and returns it as `application/x-ndjson` (one JSON
/// document per line), or as Server-Sent Events with `?format=sse`.
#[cfg(feature = "streaming")]
async fn stream_events(
    State(api): State<WalletApi>,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<StreamParams>,
) -> Response {
    use crate::domain::WalletEvent;
    use axum::response::IntoResponse;
    use firefly::reactive::Flux;
    use firefly::web::{NdJson, Sse};

    // `load_events` returns `Err(NotFound)` for an absent wallet, so the 404 is
    // decided before the streaming response head is committed.
    let events = match api.ledger.load_events(&id).await {
        Ok(events) => events,
        Err(e) => return WebError::from(domain_to_web(e)).into_response(),
    };
    let items: Vec<WalletEvent> = events.iter().map(WalletEvent::from_domain).collect();
    let flux = Flux::just(items);
    if params.format.as_deref() == Some("sse") {
        Sse(flux).into_response()
    } else {
        NdJson(flux).into_response()
    }
}

// ---------------------------------------------------------------------------
// Error mapping — CQRS / domain → RFC 9457 problem with the right status.
// ---------------------------------------------------------------------------

/// Maps a bus [`CqrsError`] onto the precise HTTP problem the domain implies:
/// a validation failure → 422, a not-found detail → 404, an
/// insufficient-funds / non-positive detail → 422, otherwise 500.
fn cqrs_to_web(err: CqrsError) -> WebError {
    match err {
        CqrsError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        CqrsError::Handler(detail) => {
            if detail.ends_with("not found") {
                WebError::from(FireflyError::not_found(detail))
            } else if detail == DomainError::InsufficientFunds.to_string()
                || detail == DomainError::NonPositiveAmount.to_string()
                || detail == DomainError::OwnerRequired.to_string()
            {
                WebError::from(FireflyError::validation(detail))
            } else {
                WebError::from(FireflyError::not_found(detail))
            }
        }
        other => WebError::from(FireflyError::internal(other.to_string())),
    }
}

/// Maps a [`DomainError`] onto the streaming endpoint's problem channel.
#[cfg(feature = "streaming")]
fn domain_to_web(e: DomainError) -> FireflyError {
    match e {
        DomainError::NotFound(_) => FireflyError::not_found(e.to_string()),
        _ => FireflyError::validation(e.to_string()),
    }
}
