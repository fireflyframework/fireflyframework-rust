//! Router composition and the reactive HTTP handlers — the composition root
//! that wires web + reactive + cqrs + eda + eventsourcing + saga + security
//! together.
//!
//! [`build_app`] assembles a [`BankingApp`] (the [`WebStack`], the CQRS
//! [`Bus`], the [`Bank`] application service, the read-model repository, and
//! the started projection) over the **in-memory** infrastructure — the
//! default used by every in-process test and a no-infra `cargo run`.
//! [`build_app_with`] takes an explicit [`EventStore`] / [`Broker`] /
//! repository, so the `main()` entry point (and the real-infra e2e test) can
//! swap in the Postgres reactive repo + Kafka broker.
//!
//! ## Endpoints
//!
//! | Method & path                              | Handler            | Reactive surface                  |
//! |--------------------------------------------|--------------------|-----------------------------------|
//! | `POST /api/v1/accounts`                    | [`open_account`]   | `Bus::send_mono` → `Mono<Json>`   |
//! | `POST /api/v1/accounts/:id/deposit`        | [`deposit`]        | `Bus::send_mono` → `Mono<Json>`   |
//! | `POST /api/v1/accounts/:id/withdraw`       | [`withdraw`]       | `Bus::send_mono` → `Mono<Json>`   |
//! | `POST /api/v1/transfers`                   | [`transfer`]       | saga → `Mono<Json>`               |
//! | `GET  /api/v1/accounts/:id`                | [`get_account`]    | `Bus::query_mono` → `Mono<Json>`  |
//! | `GET  /api/v1/accounts/:id/events`         | [`stream_events`]  | `Flux` → `application/x-ndjson` / SSE |

// `firefly_web::WebError` (the framework's RFC 7807 handler-error type) is a
// large enum by design; returning it from the small-`Ok` body-decode /
// mono-resolve helpers is what makes `?`-into-`WebResult<Response>` ergonomic
// across every handler, exactly as the framework intends. Boxing it here
// would degrade that contract for no real benefit, so the `result_large_err`
// lint is allowed module-wide.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use firefly_cqrs::{Bus, QueryCache};
use firefly_eda::Broker;
use firefly_eventsourcing::{EventStore, MemoryEventStore};
use firefly_kernel::FireflyError;
use firefly_reactive::{Flux, Mono};
use firefly_starter_web::{CoreConfig, WebStack};
use firefly_web::{NdJson, Sse, WebError, WebResult};
use http::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use http::{HeaderValue, StatusCode};
use serde::{Deserialize, Serialize};

use crate::commands::{register, Bank, Deposit, GetAccount, OpenAccount, Withdraw};
use crate::domain::{AccountEvent, AccountView, DomainError};
use crate::projections;
use crate::repository::{new_in_memory, AccountRepository};
use crate::saga::{run_transfer, TransferError, TransferRequest, TransferResult};

/// The sample's application name (banner + `/actuator/info`).
pub const APP_NAME: &str = "reactive-banking";

/// Default bind address of the public API server.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Default bind address of the admin (actuator) server.
pub const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:8081";

/// The fully-assembled banking application: every wired collaborator plus
/// the shared query cache. [`BankingApp::router`] builds the public router;
/// the `main()` entry point also reads [`BankingApp::web`] for the actuator
/// surface and lifecycle.
pub struct BankingApp {
    /// The web-tier starter (CORS / security-headers / correlation /
    /// idempotency / metrics on by default), `Deref`-ing to the core.
    pub web: WebStack,
    /// The CQRS bus with the query cache + handlers wired.
    pub bus: Arc<Bus>,
    /// The shared application service (event store + broker).
    pub bank: Bank,
    /// The reactive read-model repository.
    pub repo: AccountRepository,
    /// The query-cache handle (so callers can invalidate after mutations).
    pub query_cache: QueryCache,
}

impl BankingApp {
    /// Builds the public router: the banking routes wrapped in the web
    /// middleware chain **and** the JWT bearer + RBAC security layers.
    pub fn router(&self) -> Router {
        let routes = api_router(HandlerState {
            bus: Arc::clone(&self.bus),
            bank: self.bank.clone(),
            query_cache: self.query_cache.clone(),
        });
        let (bearer, _chain) = crate::security::security_layers();
        // The WebStack carries the FilterChain (set in build_app); layer the
        // bearer auth on the outside so the chain sees a populated
        // Authentication.
        self.web.apply_middleware(routes).layer(bearer)
    }
}

/// Shared handler state: the reactive CQRS bus, the application service the
/// saga and streaming endpoint drive directly, and the query cache (so a
/// mutation can invalidate the cached read-model view for read-after-write
/// consistency under the 30 s `GetAccount` TTL).
#[derive(Clone)]
struct HandlerState {
    bus: Arc<Bus>,
    bank: Bank,
    query_cache: QueryCache,
}

/// Assembles a [`BankingApp`] over **in-memory** infrastructure (event
/// store + broker + reactive memory repo) — the default for tests and a
/// no-infra run.
pub async fn build_app() -> BankingApp {
    build_app_with(
        Arc::new(MemoryEventStore::new()),
        WebStack::new,
        new_in_memory(),
    )
    .await
}

/// Assembles a [`BankingApp`] over an explicit [`EventStore`] and read-model
/// repository, taking a `web_factory` so the caller controls broker wiring
/// (the in-memory broker comes from [`WebStack`]/`Core` by default; pass a
/// `CoreConfig` carrying a Kafka broker for the real-infra path).
///
/// The projection is started against the resulting broker before the router
/// is returned, so published events flow into the read model immediately.
pub async fn build_app_with(
    store: Arc<dyn EventStore>,
    web_factory: impl FnOnce(CoreConfig) -> WebStack,
    repo: AccountRepository,
) -> BankingApp {
    let (_bearer, chain) = crate::security::security_layers();
    let web = web_factory(CoreConfig {
        app_name: APP_NAME.into(),
        app_version: crate::VERSION.into(),
        ..CoreConfig::default()
    })
    .with_security(chain);

    let bus = Arc::clone(&web.bus);
    let broker: Arc<dyn Broker> = Arc::clone(&web.broker);
    let bank = Bank::new(Arc::clone(&store), Arc::clone(&broker));

    // Read-side caching on the bus.
    let query_cache = QueryCache::new();
    bus.use_middleware(query_cache.middleware());

    // Wire the CQRS handlers and start the read-model projection.
    register(&bus, bank.clone(), Arc::clone(&repo));
    projections::start(&broker, Arc::clone(&store), Arc::clone(&repo))
        .await
        .expect("projection subscription");

    BankingApp {
        web,
        bus,
        bank,
        repo,
        query_cache,
    }
}

/// The bare public routes (no middleware), for composition + tests.
fn api_router(state: HandlerState) -> Router {
    Router::new()
        .route("/api/v1/accounts", post(open_account))
        .route("/api/v1/accounts/:id", get(get_account))
        .route("/api/v1/accounts/:id/events", get(stream_events))
        .route("/api/v1/accounts/:id/deposit", post(deposit))
        .route("/api/v1/accounts/:id/withdraw", post(withdraw))
        .route("/api/v1/transfers", post(transfer))
        .with_state(state)
}

/// The testable composition root: the full public router of an in-memory
/// [`BankingApp`], wired with the web middleware + JWT security.
pub async fn build_router() -> Router {
    build_app().await.router()
}

// --------------------------------------------------------------------
// Mutating handlers — reactive CQRS dispatch (Mono<Json>)
// --------------------------------------------------------------------

/// `POST /api/v1/accounts` — open an account. Reads the JSON body, dispatches
/// the [`OpenAccount`] command via [`Bus::send_mono`], and answers
/// `201 Created` with a `Location` header and the [`AccountView`] body.
async fn open_account(State(state): State<HandlerState>, body: Bytes) -> WebResult<Response> {
    let req: OpenAccount = decode_body(&body)?;
    let view: AccountView = resolve(state.bus.send_mono(req)).await?;
    let location = format!("/api/v1/accounts/{}", view.id);
    let mut res = json_response(StatusCode::CREATED, &view);
    if let Ok(value) = HeaderValue::from_str(&location) {
        res.headers_mut().insert(LOCATION, value);
    }
    Ok(res)
}

/// `POST /api/v1/accounts/:id/deposit` — credit an account.
async fn deposit(
    State(state): State<HandlerState>,
    Path(id): Path<String>,
    body: Bytes,
) -> WebResult<Response> {
    let amount: AmountBody = decode_body(&body)?;
    let cmd = Deposit {
        account_id: id,
        amount: amount.amount,
    };
    let view: AccountView = resolve(state.bus.send_mono(cmd)).await?;
    // A write invalidates the cached read-model view so a read-after-write
    // never serves a stale balance within the 30 s GetAccount TTL.
    state.query_cache.invalidate_type::<GetAccount>();
    Ok(json_response(StatusCode::OK, &view))
}

/// `POST /api/v1/accounts/:id/withdraw` — debit an account.
async fn withdraw(
    State(state): State<HandlerState>,
    Path(id): Path<String>,
    body: Bytes,
) -> WebResult<Response> {
    let amount: AmountBody = decode_body(&body)?;
    let cmd = Withdraw {
        account_id: id,
        amount: amount.amount,
    };
    let view: AccountView = resolve(state.bus.send_mono(cmd)).await?;
    state.query_cache.invalidate_type::<GetAccount>();
    Ok(json_response(StatusCode::OK, &view))
}

/// `POST /api/v1/transfers` — run a money transfer as a saga. On a clean
/// rollback the response is `422` carrying the failing leg's detail; on
/// success it is `200` with the [`TransferResult`].
async fn transfer(State(state): State<HandlerState>, body: Bytes) -> WebResult<Response> {
    let req: TransferRequest = decode_body(&body)?;
    let result: TransferResult = run_transfer(&state.bank, &req).await.map_err(|e| match e {
        TransferError::Invalid(detail) => WebError::from(FireflyError::validation(detail)),
        // A compensated transfer is a business failure, not a server error:
        // surface it as a 422 carrying the cause (e.g. "insufficient funds").
        TransferError::Compensated(detail) => WebError::from(FireflyError::validation(detail)),
    })?;
    // A transfer touches both accounts' views; invalidate the family.
    state.query_cache.invalidate_type::<GetAccount>();
    Ok(json_response(StatusCode::OK, &result))
}

// --------------------------------------------------------------------
// Read handler — reactive query (Mono<Json>)
// --------------------------------------------------------------------

/// `GET /api/v1/accounts/:id` — fetch the read-model view via
/// [`Bus::query_mono`], answering `200 OK` with the [`AccountView`] or a
/// `404` problem.
async fn get_account(
    State(state): State<HandlerState>,
    Path(id): Path<String>,
) -> WebResult<Response> {
    let view: AccountView = resolve(state.bus.query_mono(GetAccount { id })).await?;
    Ok(json_response(StatusCode::OK, &view))
}

// --------------------------------------------------------------------
// Reactive streaming handler — Flux → NDJSON / SSE
// --------------------------------------------------------------------

/// Query params for [`stream_events`]: `?format=sse` switches the stream
/// from the default NDJSON to Server-Sent Events.
#[derive(Debug, Default, Deserialize)]
struct StreamParams {
    #[serde(default)]
    format: Option<String>,
}

/// `GET /api/v1/accounts/:id/events` — the **reactive streaming** endpoint.
///
/// It builds a [`Flux<AccountEvent>`] over the account's persisted event
/// stream and returns it as `application/x-ndjson` (one JSON document per
/// line, flushed incrementally with backpressure) — the Rust analog of a
/// WebFlux handler returning `Flux<T>` with
/// `produces = APPLICATION_NDJSON_VALUE`. Pass `?format=sse` (or
/// `Accept: text/event-stream`) for the SSE wire format instead.
///
/// The `Flux` is lazy and never buffers the whole stream, so a slow client
/// throttles the producer through axum's body backpressure — genuine
/// reactive server push.
///
/// The stream's *first* item is resolved here, **before** the response head
/// is committed, so a not-found (or any other) failure on the opening signal
/// becomes the correct RFC 7807 problem (`404` for a missing account, matching
/// the non-streaming `GET /api/v1/accounts/:id`) rather than a `200` with an
/// empty NDJSON/SSE body. Once the first event has been peeked successfully,
/// it is pushed back ahead of the remaining stream so the body still flushes
/// incrementally with full backpressure — only the opening signal is eager.
async fn stream_events(
    State(state): State<HandlerState>,
    Path(id): Path<String>,
    Query(params): Query<StreamParams>,
    headers: http::HeaderMap,
) -> Response {
    let flux = match peek_event_flux(state.bank, id).await {
        Ok(flux) => flux,
        // A terminal error on the opening signal (e.g. the account does not
        // exist → 404) is rendered as a problem response, not masked as a
        // successful empty stream.
        Err(err) => return WebError::from(err).into_response(),
    };
    let wants_sse = params.format.as_deref() == Some("sse")
        || headers
            .get(ACCEPT)
            .and_then(|v| v.to_str().ok())
            .map(|a| a.contains("text/event-stream"))
            .unwrap_or(false);
    if wants_sse {
        Sse(flux).into_response()
    } else {
        NdJson(flux).into_response()
    }
}

/// Resolves the streaming [`Flux`]'s opening signal so a first-poll error can
/// be turned into an HTTP problem before the streaming response head is
/// committed.
///
/// - The stream errors on its first item → `Err(FireflyError)` (the caller
///   renders it as the matching problem, e.g. `404` for a missing account).
/// - The stream is empty → an empty [`Flux`] (a `200` empty stream — an
///   existing account always has at least its `AccountOpened` event, so this
///   is only the benign empty case).
/// - The stream yields a first event → a [`Flux`] that re-emits that event and
///   then continues lazily with the remainder, preserving backpressure for
///   every subsequent event.
async fn peek_event_flux(bank: Bank, id: String) -> Result<Flux<AccountEvent>, FireflyError> {
    use futures::StreamExt;
    let mut stream = account_event_flux(bank, id).into_stream();
    match stream.next().await {
        Some(Ok(first)) => Ok(Flux::from_stream(
            futures::stream::once(async move { Ok(first) }).chain(stream),
        )),
        Some(Err(err)) => Err(err),
        None => Ok(Flux::empty()),
    }
}

/// Builds the lazy [`Flux`] of [`AccountEvent`]s for `id`: it loads the
/// account's stream from the event store on subscription and emits each
/// event in order, or terminates with a `404` [`FireflyError`] when the
/// account does not exist.
fn account_event_flux(bank: Bank, id: String) -> Flux<AccountEvent> {
    Flux::from_stream(async_stream_events(bank, id))
}

/// The async stream backing [`account_event_flux`] — a `try_stream` that
/// loads then yields each projected event.
fn async_stream_events(
    bank: Bank,
    id: String,
) -> impl futures::Stream<Item = Result<AccountEvent, FireflyError>> {
    async_stream::try_stream! {
        let events = bank.load_events(&id).await.map_err(domain_to_firefly)?;
        for event in &events {
            yield AccountEvent::from_domain(event);
        }
    }
}

// --------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------

/// A `{ "amount": <i64> }` request body for deposit / withdraw.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AmountBody {
    amount: i64,
}

/// Decodes a JSON request body with the lenient "first value only / null →
/// default" semantics the orders sample documents (Go's `json.Decoder`),
/// so domain validation — not decoding — rejects incomplete commands.
fn decode_body<T>(body: &Bytes) -> Result<T, WebError>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let mut decoder = serde_json::Deserializer::from_slice(body);
    Option::deserialize(&mut decoder)
        .map(Option::unwrap_or_default)
        .map_err(|e| WebError::from(FireflyError::bad_request(format!("invalid json: {e}"))))
}

/// Awaits a [`Mono`] result, mapping the reactive [`FireflyError`] terminal
/// signal back onto an HTTP problem with the right status. A CQRS handler
/// error carrying a domain message is re-typed to its precise status (404 /
/// 422) so the wire shape matches the framework's RFC 7807 contract.
async fn resolve<T>(mono: Mono<T>) -> Result<T, WebError>
where
    T: Send + 'static,
{
    match mono.into_future().await {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(WebError::from(FireflyError::not_found("account not found"))),
        Err(err) => Err(WebError::from(retype_handler_error(err))),
    }
}

/// Re-types a generic reactive error (a CQRS handler failure surfaces as a
/// 500 `FireflyError` with the domain message as its detail) into the
/// precise status the domain implies: a not-found message → 404, an
/// insufficient-funds / validation message → 422.
fn retype_handler_error(err: FireflyError) -> FireflyError {
    let detail = err.detail.clone();
    if detail.ends_with("not found") {
        FireflyError::not_found(detail)
    } else if detail == DomainError::InsufficientFunds.to_string()
        || detail == DomainError::NonPositiveAmount.to_string()
    {
        FireflyError::validation(detail)
    } else if err.status == 500 && !detail.is_empty() {
        // A domain rule violation routed through the bus handler channel.
        FireflyError::validation(detail)
    } else {
        err
    }
}

/// Maps a [`DomainError`] onto the streaming `Flux`'s [`FireflyError`]
/// terminal channel.
fn domain_to_firefly(e: DomainError) -> FireflyError {
    match e {
        DomainError::NotFound(_) => FireflyError::not_found(e.to_string()),
        _ => FireflyError::validation(e.to_string()),
    }
}

/// Renders `value` as compact JSON with a trailing newline under
/// `Content-Type: application/json` (byte-parity with the orders sample's
/// `json.Encoder` output).
fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    let mut body = serde_json::to_vec(value).unwrap_or_default();
    body.push(b'\n');
    let mut res = Response::new(Body::from(body));
    *res.status_mut() = status;
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    res
}
