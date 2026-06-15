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

//! The optional reactive streaming endpoint, exercised only when the
//! `streaming` feature is on:
//!
//! ```sh
//! cargo test -p firefly-sample-lumen --features streaming --test streaming
//! ```
//!
//! Without the feature this file compiles to nothing, so the default test run
//! stays lean.

#![cfg(feature = "streaming")]

use crate::build_router;
use crate::domain::WalletView;
use crate::security::{mint_token, CUSTOMER_ROLE};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn bearer() -> String {
    format!("Bearer {}", mint_token("u-alice", &[CUSTOMER_ROLE]))
}

async fn open_with_deposit() -> String {
    // Open a wallet and make a deposit so the stream has two events.
    let res = build_router()
        .await
        .oneshot(
            Request::post("/api/v1/wallets")
                .header("content-type", "application/json")
                .header("authorization", bearer())
                .body(Body::from(
                    serde_json::to_vec(
                        &serde_json::json!({ "owner": "grace", "openingBalance": 100 }),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let view: WalletView = serde_json::from_slice(&bytes).unwrap();

    build_router()
        .await
        .oneshot(
            Request::post(format!("/api/v1/wallets/{}/deposit", view.id))
                .header("content-type", "application/json")
                .header("authorization", bearer())
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "amount": 50 })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    view.id
}

#[tokio::test]
async fn events_stream_as_ndjson_by_default() {
    let id = open_with_deposit().await;
    let res = build_router()
        .await
        .oneshot(
            Request::get(format!("/api/v1/wallets/{id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        ct.contains("ndjson"),
        "default stream should be NDJSON, got {ct:?}"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(body.to_vec()).unwrap();
    // Two events: the WalletOpened and the MoneyDeposited, one JSON per line.
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 NDJSON lines, got: {text:?}");
    assert!(text.contains("WalletOpened"));
    assert!(text.contains("MoneyDeposited"));
}

#[tokio::test]
async fn events_stream_as_sse_when_requested() {
    let id = open_with_deposit().await;
    let res = build_router()
        .await
        .oneshot(
            Request::get(format!("/api/v1/wallets/{id}/events?format=sse"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(ct.contains("text/event-stream"), "got {ct:?}");
}

#[tokio::test]
async fn events_for_unknown_wallet_is_404_problem() {
    let res = build_router()
        .await
        .oneshot(
            Request::get("/api/v1/wallets/wlt_missing/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
