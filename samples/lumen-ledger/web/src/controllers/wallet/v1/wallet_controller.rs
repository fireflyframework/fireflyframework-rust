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

//! The [`WalletController`] `@RestController` (`<domain>/v1`).
//!
//! A `#[derive(Controller)]` DI bean that autowires the `dyn WalletService`
//! port from the `-core` crate and is auto-mounted by `#[rest_controller]`.
//! Every input is validated/extracted at the edge — `Valid<T>` for JSON bodies,
//! the framework's problem-rendering `Path` / `Query` extractors for path/query
//! params — and each [`ServiceError`] becomes the precise RFC 9457 problem
//! (`404` / `409` / `422` / `500`) via [`service_to_web`].

// `firefly::web::WebError` is a large enum by design; returning it from these
// handlers is what makes `?`-into-`WebResult<T>` ergonomic.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use firefly::data::Page;
use firefly::prelude::*;
use firefly::web::{PageRequest, Path, Query, Valid, WebError, WebResult};
use lumen_ledger_core::{ServiceError, WalletService};
use lumen_ledger_interfaces::{
    AmountRequest, CreateWalletRequest, TransferRequest, WalletFilter, WalletResponse, WalletStatus,
};
use serde::Deserialize;
use uuid::Uuid;

/// The wallet HTTP surface — a `#[derive(Controller)]` bean whose `dyn
/// WalletService` collaborator is autowired from the container.
#[derive(Clone, Controller)]
pub struct WalletController {
    /// The application service port (provided by `WalletServiceImpl` in `-core`).
    #[autowired]
    service: Arc<dyn WalletService>,
}

/// `?owner=…` query for the by-owner list endpoint.
#[derive(Debug, Deserialize, Schema)]
struct OwnerQuery {
    /// The owner whose wallets to list. **Required** — the listing is
    /// deliberately owner-scoped (never an unauthenticated list-everything).
    #[serde(default)]
    owner: Option<String>,
}

/// `?status=` filter for the paged list endpoint. Pagination (`page`, `size`,
/// `sort`) is bound separately by the framework's `PageRequest` resolver, so
/// this query only carries the domain filter.
#[derive(Debug, Deserialize, Schema)]
struct StatusQuery {
    /// The status to filter by (defaults to `active`).
    #[serde(default)]
    status: WalletStatus,
}

/// JSON body for the status-transition endpoint.
#[derive(Debug, Deserialize, Schema)]
struct StatusBody {
    /// The new status.
    status: WalletStatus,
}

/// `#[rest_controller]` generates `WalletController::routes(state)`; each method
/// carries one verb mapping and returns `WebResult<T>` so an error renders as
/// RFC 9457 `application/problem+json`.
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletController {
    /// `POST /api/v1/wallets` — open a wallet (`Valid<…>` enforces every DTO
    /// constraint, rendering `422` on a blank owner / bad currency / negative
    /// opening balance).
    #[post(
        "/wallets",
        summary = "Open a wallet",
        description = "Opens a new wallet for an owner with an optional opening balance.",
        status = 201,
        header(
            "Idempotency-Key",
            description = "optional client-supplied key to make retries safe"
        )
    )]
    async fn open(
        State(api): State<WalletController>,
        headers: axum::http::HeaderMap,
        Valid(body): Valid<CreateWalletRequest>,
    ) -> WebResult<(StatusCode, Json<WalletResponse>)> {
        // The declared `Idempotency-Key` header is readable here exactly like any
        // axum header (a real handler would dedupe on it); declaring it makes it a
        // first-class parameter in Swagger UI / ReDoc so callers can supply it.
        let _idempotency_key = headers
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok());
        let view = api.service.create(body).await.map_err(service_to_web)?;
        Ok((StatusCode::CREATED, Json(view)))
    }

    /// `GET /api/v1/wallets?owner=…` — list a single owner's wallets.
    ///
    /// **`owner` is required, by design.** The collection is deliberately scoped
    /// to one owner rather than returning every wallet: an *unfiltered* listing
    /// would be an unauthenticated enumeration of every account holder and their
    /// balance (an IDOR / broken-access-control). A real service must gate such a
    /// listing behind authentication + an admin authority (e.g.
    /// `#[firefly::pre_authorize(role = "ADMIN")]`) and otherwise scope it to the
    /// caller's *own* wallets. This sample intentionally omits the auth layer
    /// (it demonstrates the data/web/transactional stack; access control is its
    /// own [security chapter]), so it keeps the surface scoped instead of
    /// exposing a `list_all`. A missing `owner` is a clear **400** problem.
    #[get("/wallets", summary = "List a single owner's wallets (?owner= required)")]
    async fn list(
        State(api): State<WalletController>,
        Query(query): Query<OwnerQuery>,
    ) -> WebResult<Json<Vec<WalletResponse>>> {
        let owner = query
            .owner
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .ok_or_else(|| {
                WebError::from(FireflyError::bad_request(
                    "the `owner` query parameter is required, e.g. `GET /api/v1/wallets?owner=ada`",
                ))
            })?;
        let views = api
            .service
            .list_by_owner(owner)
            .await
            .map_err(service_to_web)?;
        Ok(Json(views))
    }

    /// `GET /api/v1/wallets/search?owner=&currency=&status=&minBalance=&maxBalance=`
    /// — filter wallets by any combination of criteria (AND-combined). Every
    /// field is an **optional query parameter** (each rendered in Swagger UI from
    /// the `WalletFilter` schema); the `@Service` translates the set into a
    /// framework `Specification` the repository compiles to a dialect-aware
    /// `WHERE` — the Spring Data `JpaSpecificationExecutor` analog.
    ///
    /// At least one criterion is **required** (a no-filter search would be an
    /// unscoped enumeration → **422**). Like the rest of this auth-free demo, the
    /// result is not authorization-scoped; a real service would scope it to the
    /// caller (admin sees all, others their own) — see the `list` note above.
    #[get("/wallets/search", summary = "Search wallets by filter criteria")]
    async fn search(
        State(api): State<WalletController>,
        Query(filter): Query<WalletFilter>,
    ) -> WebResult<Json<Vec<WalletResponse>>> {
        let views = api.service.search(filter).await.map_err(service_to_web)?;
        Ok(Json(views))
    }

    /// `GET /api/v1/wallets/page?status=&page=&size=&sort=` — a Spring Data
    /// `Page<T>` of wallets in a status. Pagination is bound by the framework's
    /// `PageRequest` argument resolver (`page`/`size`/`sort`, e.g.
    /// `?sort=balance,desc`), exactly like a Spring `Pageable` parameter.
    #[get("/wallets/page", summary = "List wallets by status (paged)")]
    async fn list_paged(
        State(api): State<WalletController>,
        Query(query): Query<StatusQuery>,
        PageRequest(pageable): PageRequest,
    ) -> WebResult<Json<Page<WalletResponse>>> {
        let page = api
            .service
            .list_by_status(query.status, pageable)
            .await
            .map_err(service_to_web)?;
        Ok(Json(page))
    }

    /// `GET /api/v1/wallets/:id` — fetch one wallet (404 when unknown, 400 on a
    /// malformed UUID).
    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api.service.get(id).await.map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `PATCH /api/v1/wallets/:id/status` — transition a wallet's lifecycle.
    #[patch("/wallets/:id/status", summary = "Change a wallet's status")]
    async fn set_status(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
        Json(body): Json<StatusBody>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api
            .service
            .set_status(id, body.status)
            .await
            .map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/deposit` — credit a wallet (`Valid<…>` rejects a
    /// non-positive amount as 422 before the service runs).
    #[post("/wallets/:id/deposit", summary = "Deposit funds", status = 200)]
    async fn deposit(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
        Valid(body): Valid<AmountRequest>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api
            .service
            .deposit(id, body.amount)
            .await
            .map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/withdraw` — debit a wallet (422 on overdraft).
    #[post("/wallets/:id/withdraw", summary = "Withdraw funds", status = 200)]
    async fn withdraw(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
        Valid(body): Valid<AmountRequest>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api
            .service
            .withdraw(id, body.amount)
            .await
            .map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/transfer` — **atomically** move funds from this
    /// wallet to another (`Valid<…>` rejects a non-positive amount or a blank
    /// destination as 422). The debit and credit commit together or not at all;
    /// insufficient funds / an inactive party is a 422 with no partial debit.
    #[post("/wallets/:id/transfer", summary = "Transfer funds to another wallet", status = 200)]
    async fn transfer(
        State(api): State<WalletController>,
        Path(from): Path<Uuid>,
        Valid(body): Valid<TransferRequest>,
    ) -> WebResult<Json<WalletResponse>> {
        let to = Uuid::parse_str(&body.to).map_err(|_| {
            WebError::from(FireflyError::validation("`to` is not a valid wallet id"))
        })?;
        let view = api
            .service
            .transfer(from, to, body.amount)
            .await
            .map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `DELETE /api/v1/wallets/:id` — delete a wallet (idempotent; `204`).
    #[delete("/wallets/:id", summary = "Delete a wallet", status = 204)]
    async fn delete(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
    ) -> WebResult<StatusCode> {
        api.service.delete(id).await.map_err(service_to_web)?;
        Ok(StatusCode::NO_CONTENT)
    }
}

/// Maps a [`ServiceError`] onto the precise HTTP problem the domain implies.
///
/// A free function rather than `impl From<ServiceError> for WebError`: both
/// types are foreign to this crate, so Rust's orphan rule forbids the `From`
/// impl here — the same cross-crate constraint that shapes the `@Mapper`. The
/// mapping fn is the idiomatic alternative.
fn service_to_web(err: ServiceError) -> WebError {
    match err {
        ServiceError::NotFound => WebError::from(FireflyError::not_found("wallet not found")),
        ServiceError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        ServiceError::Conflict(detail) => WebError::from(FireflyError::conflict(detail)),
        ServiceError::Backend(detail) => WebError::from(FireflyError::internal(detail)),
    }
}

// `WalletStatus` must default for `#[serde(default)]` on the query — re-stated
// here as a compile assertion that the contract enum stays `Default`.
const _: fn() = || {
    let _ = <WalletStatus as Default>::default();
};
