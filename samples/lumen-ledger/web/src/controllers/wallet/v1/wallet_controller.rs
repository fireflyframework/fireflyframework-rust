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
//! port from the `-core` crate and is auto-mounted by `#[rest_controller]`. Each
//! handler dispatches to the service and turns a [`ServiceError`] into the
//! precise RFC 9457 problem (`404` / `422` / `500`). The request/response
//! schemas are **inferred** from the handler signatures — `Valid<T>` / `Json<T>`
//! arguments and `Json<T>` results — so the OpenAPI document needs no manual
//! `request`/`response` annotations.

// `firefly::web::WebError` is a large enum by design; returning it from these
// handlers is what makes `?`-into-`WebResult<T>` ergonomic.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use firefly::prelude::*;
use firefly::web::{Valid, WebError, WebResult};
use lumen_ledger_core::{ServiceError, WalletService};
use lumen_ledger_interfaces::{AmountRequest, CreateWalletRequest, WalletResponse};
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

/// `?owner=…` query for the list endpoint.
#[derive(Debug, Deserialize)]
struct OwnerQuery {
    /// The owner whose wallets to list.
    owner: String,
}

/// `#[rest_controller]` generates `WalletController::routes(state)`; each method
/// carries one verb mapping and returns `WebResult<T>` so an error renders as
/// RFC 9457 `application/problem+json`.
#[rest_controller(path = "/api/v1", tag = "Wallets")]
impl WalletController {
    /// `POST /api/v1/wallets` — open a wallet (`Valid<…>` enforces the DTO's
    /// bean-validation constraints, rendering `422` on a blank owner/currency).
    #[post(
        "/wallets",
        summary = "Open a wallet",
        description = "Opens a new wallet for an owner with an optional opening balance.",
        status = 201
    )]
    async fn open(
        State(api): State<WalletController>,
        Valid(body): Valid<CreateWalletRequest>,
    ) -> WebResult<(StatusCode, Json<WalletResponse>)> {
        let view = api.service.create(body).await.map_err(service_to_web)?;
        Ok((StatusCode::CREATED, Json(view)))
    }

    /// `GET /api/v1/wallets?owner=…` — list one owner's wallets.
    #[get("/wallets", summary = "List an owner's wallets")]
    async fn list(
        State(api): State<WalletController>,
        Query(query): Query<OwnerQuery>,
    ) -> WebResult<Json<Vec<WalletResponse>>> {
        let views = api
            .service
            .list_by_owner(&query.owner)
            .await
            .map_err(service_to_web)?;
        Ok(Json(views))
    }

    /// `GET /api/v1/wallets/:id` — fetch one wallet (404 when unknown).
    #[get("/wallets/:id", summary = "Fetch a wallet")]
    async fn get(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api.service.get(id).await.map_err(service_to_web)?;
        Ok(Json(view))
    }

    /// `POST /api/v1/wallets/:id/deposit` — credit a wallet.
    #[post("/wallets/:id/deposit", summary = "Deposit funds", status = 200)]
    async fn deposit(
        State(api): State<WalletController>,
        Path(id): Path<Uuid>,
        Json(body): Json<AmountRequest>,
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
        Json(body): Json<AmountRequest>,
    ) -> WebResult<Json<WalletResponse>> {
        let view = api
            .service
            .withdraw(id, body.amount)
            .await
            .map_err(service_to_web)?;
        Ok(Json(view))
    }
}

/// Maps a [`ServiceError`] onto the precise HTTP problem the domain implies.
fn service_to_web(err: ServiceError) -> WebError {
    match err {
        ServiceError::NotFound => WebError::from(FireflyError::not_found("wallet not found")),
        ServiceError::Validation(detail) => WebError::from(FireflyError::validation(detail)),
        ServiceError::Backend(detail) => WebError::from(FireflyError::internal(detail)),
    }
}
