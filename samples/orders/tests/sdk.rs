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

//! Rust-specific SDK coverage (the Go sample ships its `sdk` package
//! untested): the typed client drives the full router over a real
//! socket on an ephemeral 127.0.0.1 port — never the bin's default
//! 8080 — exercising `firefly-client`'s JSON, correlation, and RFC 7807
//! problem decoding end to end.

use firefly_kernel::TYPE_NOT_FOUND;
use firefly_sample_orders::build_router;
use firefly_sample_orders::interfaces::PlaceOrderRequest;
use firefly_sample_orders::sdk::Client;

/// Serves `build_router()` on an ephemeral port, returning the base URL
/// and the serve-task handle (aborted by the caller).
async fn serve_app() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, build_router()).await.expect("serve");
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn sdk_place_and_get_roundtrip() {
    let (base_url, server) = serve_app().await;
    let client = Client::new(&base_url);

    let placed = client
        .place(&PlaceOrderRequest {
            customer: "alice".into(),
            sku: "SKU-1".into(),
            quantity: 2,
            total: 19.99,
        })
        .await
        .expect("place order");
    assert!(placed.id.starts_with("ord_"), "id: {}", placed.id);
    assert_eq!(placed.status, "placed");
    assert_eq!(placed.customer, "alice");

    let got = client.get(&placed.id).await.expect("get order");
    assert_eq!(got, placed);

    server.abort();
}

#[tokio::test]
async fn sdk_get_missing_decodes_not_found_problem() {
    let (base_url, server) = serve_app().await;
    let client = Client::new(&base_url);

    let err = client.get("missing").await.expect_err("expected 404");
    let fe = err.as_firefly().expect("problem-decoded error");
    assert_eq!(fe.status, 404);
    assert_eq!(fe.code, TYPE_NOT_FOUND);
    assert_eq!(fe.detail, "order missing not found");

    server.abort();
}

#[tokio::test]
async fn sdk_place_invalid_decodes_validation_problem() {
    let (base_url, server) = serve_app().await;
    let client = Client::new(&base_url);

    let err = client
        .place(&PlaceOrderRequest::default())
        .await
        .expect_err("expected 422");
    let fe = err.as_firefly().expect("problem-decoded error");
    assert_eq!(fe.status, 422);
    assert_eq!(fe.detail, "customer is required");

    server.abort();
}
