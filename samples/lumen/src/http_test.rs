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

//! End-to-end HTTP tests driven with `tower::ServiceExt::oneshot` against the
//! full [`build_router`] composition — no socket bound. They prove the
//! macro-generated `#[rest_controller]` routes, the CQRS handler bean, the
//! event-sourced ledger, the read-model projection bean, the transfer saga
//! (happy path **and** compensation), and the JWT/RBAC enforcement all work
//! together.
//!
//! Each test boots **one** app context with [`build_router`] and drives every
//! request against it — Spring Boot's `@SpringBootTest` model. Because the
//! handlers, ledger, projection, query cache, and security are all DI beans
//! resolved from one container, the container's singletons stay consistent
//! across a test's requests (the wallet a command opens is the wallet a later
//! query reads). The `axum::Router` is cheap to `clone` (it is `Arc`-backed), so
//! each `oneshot` clones the shared app.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::build_router;
use crate::domain::WalletView;
use crate::security::{mint_token, CUSTOMER_ROLE};
use crate::tcc_transfer::TccTransferResult;
use crate::transfer::TransferResult;

fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

fn post(path: &str, body: serde_json::Value, auth: bool) -> Request<Body> {
    let mut b = Request::post(path).header("content-type", "application/json");
    if auth {
        b = b.header("authorization", bearer());
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(res: Response) -> T {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "decode body failed: {e}; raw: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn content_type(res: &Response) -> String {
    res.headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Sends one request against the (cloned) shared app and returns the response.
async fn send(app: &Router, req: Request<Body>) -> Response {
    app.clone().oneshot(req).await.unwrap()
}

/// Opens a wallet through the API against the shared app and returns its view.
async fn open_wallet(app: &Router, owner: &str, opening: i64) -> WalletView {
    let res = send(
        app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": owner, "openingBalance": opening }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED, "open should 201");
    body_json(res).await
}

/// Fetches a wallet view through the API against the shared app.
async fn get_wallet(app: &Router, id: &str) -> WalletView {
    let res = send(app, get(&format!("/api/v1/wallets/{id}"))).await;
    assert_eq!(res.status(), StatusCode::OK, "get should 200");
    body_json(res).await
}

#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let app = build_router().await;
    let opened = open_wallet(&app, "alice", 1_000).await;
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);
    assert_eq!(opened.version, 1);

    // GET dispatches the #[query_handler] on the handler bean; it reads the
    // projection (or repairs from the event stream) — both resolved from the
    // same container as the command that opened the wallet.
    let fetched = get_wallet(&app, &opened.id).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}

#[tokio::test]
async fn deposit_and_withdraw_update_the_balance() {
    let app = build_router().await;
    let opened = open_wallet(&app, "bob", 100).await;

    let res = send(
        &app,
        post(
            &format!("/api/v1/wallets/{}/deposit", opened.id),
            serde_json::json!({ "amount": 250 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let after_deposit: WalletView = body_json(res).await;
    assert_eq!(after_deposit.balance, 350);

    let res = send(
        &app,
        post(
            &format!("/api/v1/wallets/{}/withdraw", opened.id),
            serde_json::json!({ "amount": 50 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let after_withdraw: WalletView = body_json(res).await;
    assert_eq!(after_withdraw.balance, 300);

    // Read-after-write: the cache was invalidated, so GET reflects the writes.
    let view = get_wallet(&app, &opened.id).await;
    assert_eq!(view.balance, 300);
    assert_eq!(view.version, 3);
}

#[tokio::test]
async fn transfer_saga_happy_path_moves_funds_between_wallets() {
    let app = build_router().await;
    let src = open_wallet(&app, "carol", 1_000).await;
    let dst = open_wallet(&app, "dave", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 300 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let result: TransferResult = body_json(res).await;
    assert_eq!(result.status, "completed");
    assert_eq!(result.steps_executed, ["debit", "credit"]);
    assert!(result.steps_rolled_back.is_empty());

    // Funds moved on both views.
    assert_eq!(get_wallet(&app, &src.id).await.balance, 700);
    assert_eq!(get_wallet(&app, &dst.id).await.balance, 300);
}

#[tokio::test]
async fn transfer_saga_overdraft_compensates_and_is_422() {
    let app = build_router().await;
    let src = open_wallet(&app, "erin", 100).await;
    let dst = open_wallet(&app, "frank", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ),
    )
    .await;
    // A compensated transfer is a business failure → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));

    // Funds untouched: the debit was rejected up front, so the source keeps
    // its balance.
    assert_eq!(get_wallet(&app, &src.id).await.balance, 100);
}

// ---------------------------------------------------------------------------
// Declarative orchestration endpoints: the compliance *workflow* (parallel
// balance + limit checks → approve) and the two-phase (TCC) transfer.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compliance_workflow_approves_a_funded_in_limit_transfer() {
    let app = build_router().await;
    let src = open_wallet(&app, "grace", 1_000).await;
    let dst = open_wallet(&app, "heidi", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 300 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let decision: serde_json::Value = body_json(res).await;
    assert_eq!(decision["decision"], "approved");
    assert_eq!(decision["amount"], 300);

    // A read-only pre-check moves no funds.
    assert_eq!(get_wallet(&app, &src.id).await.balance, 1_000);
}

#[tokio::test]
async fn compliance_workflow_rejects_overdraft_with_422() {
    let app = build_router().await;
    let src = open_wallet(&app, "ivan", 100).await;
    let dst = open_wallet(&app, "judy", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ),
    )
    .await;
    // Insufficient funds → the workflow's approve node rejects → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn compliance_workflow_unknown_source_is_404() {
    let app = build_router().await;
    let dst = open_wallet(&app, "ken", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": "wlt_does_not_exist", "to": dst.id, "amount": 10 }),
            true,
        ),
    )
    .await;
    // The balance-check node cannot load an unknown source → 404 problem.
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn tcc_transfer_confirms_and_moves_funds() {
    let app = build_router().await;
    let src = open_wallet(&app, "laura", 1_000).await;
    let dst = open_wallet(&app, "mike", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers/2pc",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 250 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let result: TccTransferResult = body_json(res).await;
    assert_eq!(result.status, "confirmed");
    assert_eq!(result.amount, 250);

    // Both sides captured: the source was debited on try, the destination
    // credited on confirm.
    assert_eq!(get_wallet(&app, &src.id).await.balance, 750);
    assert_eq!(get_wallet(&app, &dst.id).await.balance, 250);
}

#[tokio::test]
async fn tcc_transfer_overdraft_releases_the_hold_and_is_422() {
    let app = build_router().await;
    let src = open_wallet(&app, "nina", 100).await;
    let dst = open_wallet(&app, "oscar", 0).await;

    let res = send(
        &app,
        post(
            "/api/v1/transfers/2pc",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ),
    )
    .await;
    // The source try (withdraw) fails up front → the coordinator cancels the
    // tried participants → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));

    // No funds left the source: the failed hold moved nothing.
    assert_eq!(get_wallet(&app, &src.id).await.balance, 100);
}

#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let app = build_router().await;
    let res = send(
        &app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn invalid_open_is_422_problem() {
    let app = build_router().await;
    let res = send(
        &app,
        post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "", "openingBalance": 10 }),
            true,
        ),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn unknown_wallet_is_404_problem() {
    let app = build_router().await;
    let res = send(&app, get("/api/v1/wallets/wlt_does_not_exist")).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(content_type(&res).contains("application/problem+json"));
}
