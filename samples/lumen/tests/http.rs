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
//! macro-generated `#[rest_controller]` routes, the CQRS handlers, the
//! event-sourced ledger, the read-model projection, the transfer saga (happy
//! path **and** compensation), and the JWT/RBAC enforcement all work together.
//!
//! Lumen's free-fn command handlers and projection publish their collaborators
//! through process-global `OnceLock`s (the declarative-macro pattern), so the
//! first `build_router()` in this binary wires the shared ledger every test
//! then drives. Each test opens its own wallets (random ids), so there is no
//! cross-test interference.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use firefly_sample_lumen::build_router;
use firefly_sample_lumen::domain::WalletView;
use firefly_sample_lumen::security::{mint_token, CUSTOMER_ROLE};
use firefly_sample_lumen::tcc_transfer::TccTransferResult;
use firefly_sample_lumen::transfer::TransferResult;
use http_body_util::BodyExt;
use tower::ServiceExt;

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

/// Opens a wallet through the API and returns its created view.
async fn open_wallet(owner: &str, opening: i64) -> WalletView {
    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": owner, "openingBalance": opening }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "open should 201");
    body_json(res).await
}

#[tokio::test]
async fn open_then_get_round_trips_through_cqrs() {
    let opened = open_wallet("alice", 1_000).await;
    assert_eq!(opened.owner, "alice");
    assert_eq!(opened.balance, 1_000);
    assert_eq!(opened.version, 1);

    // GET dispatches the #[query_handler]; the projection has already folded
    // the WalletOpened event into the read model.
    let res = build_router()
        .await
        .oneshot(get(&format!("/api/v1/wallets/{}", opened.id)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let fetched: WalletView = body_json(res).await;
    assert_eq!(fetched.id, opened.id);
    assert_eq!(fetched.balance, 1_000);
}

#[tokio::test]
async fn deposit_and_withdraw_update_the_balance() {
    let opened = open_wallet("bob", 100).await;

    let res = build_router()
        .await
        .oneshot(post(
            &format!("/api/v1/wallets/{}/deposit", opened.id),
            serde_json::json!({ "amount": 250 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let after_deposit: WalletView = body_json(res).await;
    assert_eq!(after_deposit.balance, 350);

    let res = build_router()
        .await
        .oneshot(post(
            &format!("/api/v1/wallets/{}/withdraw", opened.id),
            serde_json::json!({ "amount": 50 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let after_withdraw: WalletView = body_json(res).await;
    assert_eq!(after_withdraw.balance, 300);

    // Read-after-write: the cache was invalidated, so GET reflects the writes.
    let res = build_router()
        .await
        .oneshot(get(&format!("/api/v1/wallets/{}", opened.id)))
        .await
        .unwrap();
    let view: WalletView = body_json(res).await;
    assert_eq!(view.balance, 300);
    assert_eq!(view.version, 3);
}

#[tokio::test]
async fn transfer_saga_happy_path_moves_funds_between_wallets() {
    let src = open_wallet("carol", 1_000).await;
    let dst = open_wallet("dave", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 300 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let result: TransferResult = body_json(res).await;
    assert_eq!(result.status, "completed");
    assert_eq!(result.steps_executed, ["debit", "credit"]);
    assert!(result.steps_rolled_back.is_empty());

    // Funds moved on both projected views.
    let src_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", src.id)))
            .await
            .unwrap(),
    )
    .await;
    let dst_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", dst.id)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(src_view.balance, 700);
    assert_eq!(dst_view.balance, 300);
}

#[tokio::test]
async fn transfer_saga_overdraft_compensates_and_is_422() {
    let src = open_wallet("erin", 100).await;
    let dst = open_wallet("frank", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ))
        .await
        .unwrap();
    // A compensated transfer is a business failure → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));

    // Funds untouched: the debit was rejected up front, so the source keeps
    // its balance and the destination is unchanged.
    let src_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", src.id)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(src_view.balance, 100);
}

// ---------------------------------------------------------------------------
// Declarative orchestration endpoints: the compliance *workflow* (parallel
// balance + limit checks → approve) and the two-phase (TCC) transfer.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compliance_workflow_approves_a_funded_in_limit_transfer() {
    let src = open_wallet("grace", 1_000).await;
    let dst = open_wallet("heidi", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 300 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let decision: serde_json::Value = body_json(res).await;
    assert_eq!(decision["decision"], "approved");
    assert_eq!(decision["amount"], 300);

    // A read-only pre-check moves no funds.
    let src_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", src.id)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(src_view.balance, 1_000);
}

#[tokio::test]
async fn compliance_workflow_rejects_overdraft_with_422() {
    let src = open_wallet("ivan", 100).await;
    let dst = open_wallet("judy", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ))
        .await
        .unwrap();
    // Insufficient funds → the workflow's approve node rejects → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn compliance_workflow_unknown_source_is_404() {
    let dst = open_wallet("ken", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers/compliance",
            serde_json::json!({ "from": "wlt_does_not_exist", "to": dst.id, "amount": 10 }),
            true,
        ))
        .await
        .unwrap();
    // The balance-check node cannot load an unknown source → 404 problem.
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn tcc_transfer_confirms_and_moves_funds() {
    let src = open_wallet("laura", 1_000).await;
    let dst = open_wallet("mike", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers/2pc",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 250 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let result: TccTransferResult = body_json(res).await;
    assert_eq!(result.status, "confirmed");
    assert_eq!(result.amount, 250);

    // Both sides captured: the source was debited on try, the destination
    // credited on confirm.
    let src_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", src.id)))
            .await
            .unwrap(),
    )
    .await;
    let dst_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", dst.id)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(src_view.balance, 750);
    assert_eq!(dst_view.balance, 250);
}

#[tokio::test]
async fn tcc_transfer_overdraft_releases_the_hold_and_is_422() {
    let src = open_wallet("nina", 100).await;
    let dst = open_wallet("oscar", 0).await;

    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/transfers/2pc",
            serde_json::json!({ "from": src.id, "to": dst.id, "amount": 500 }),
            true,
        ))
        .await
        .unwrap();
    // The source try (withdraw) fails up front → the coordinator cancels the
    // tried participants → 422 problem.
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));

    // No funds left the source: the failed hold moved nothing (and any partial
    // hold is released), so the balance is unchanged.
    let src_view: WalletView = body_json(
        build_router()
            .await
            .oneshot(get(&format!("/api/v1/wallets/{}", src.id)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(src_view.balance, 100);
}

#[tokio::test]
async fn missing_token_is_401_problem_on_mutations() {
    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "mallory", "openingBalance": 10 }),
            false, // no Authorization header
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn invalid_open_is_422_problem() {
    let res = build_router()
        .await
        .oneshot(post(
            "/api/v1/wallets",
            serde_json::json!({ "owner": "", "openingBalance": 10 }),
            true,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(content_type(&res).contains("application/problem+json"));
}

#[tokio::test]
async fn unknown_wallet_is_404_problem() {
    let res = build_router()
        .await
        .oneshot(get("/api/v1/wallets/wlt_does_not_exist"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(content_type(&res).contains("application/problem+json"));
}
