//! In-process **end-to-end** test of the full ecosystem wired together.
//!
//! It boots the real router on an ephemeral `127.0.0.1:0` server and drives
//! it with the reactive [`BankClient`] (the `firefly-client` `WebClient`
//! SDK), exercising the headline flow:
//!
//! 1. open account → deposit → withdraw (reactive CQRS dispatch),
//! 2. a money transfer that **completes** (saga happy path),
//! 3. a money transfer that **fails and compensates** (saga rollback),
//! 4. `GET /accounts/:id` reflects the EDA-driven projection, and
//! 5. the `/events` streaming endpoint emits the account's events as NDJSON,
//!    consumed reactively as a `Flux`.
//!
//! JWT auth is enforced throughout (401 without a token, 200 with).
//!
//! No `sleep` exceeds 200 ms — the test polls the read model with a short
//! retry loop instead.

use std::net::SocketAddr;
use std::time::Duration;

use firefly_client::WebClientBuilder;
use firefly_reactive::{Flux, Mono};
use firefly_sample_reactive_banking::build_router;
use firefly_sample_reactive_banking::domain::{AccountEvent, AccountView};
use firefly_sample_reactive_banking::sdk::BankClient;
use firefly_sample_reactive_banking::security::mint_token;

/// Boots the in-memory app on an ephemeral port and returns its base URL +
/// the join handle (kept alive for the test's duration).
async fn spawn_server() -> (String, tokio::task::JoinHandle<()>) {
    let app = build_router().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// Builds an authenticated SDK client (CUSTOMER role) for `base`.
fn authed_client(base: &str) -> BankClient {
    let token = mint_token("u-alice", &["CUSTOMER"]);
    BankClient::new(base, &token)
}

/// Polls `get_account` until the projected balance equals `want` (or a short
/// budget elapses), returning the converged view. The in-memory broker
/// delivers synchronously, so this converges immediately; the loop exists so
/// the same test body works unchanged against the eventually-consistent
/// Kafka path. No individual wait exceeds 200 ms.
async fn await_balance(client: &BankClient, id: &str, want: i64) -> AccountView {
    for _ in 0..50 {
        if let Some(view) = client.get_account(id).into_future().await.unwrap() {
            if view.balance == want {
                return view;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("account {id} never reached balance {want}");
}

#[tokio::test]
async fn full_ecosystem_flow_through_the_reactive_sdk() {
    let (base, _server) = spawn_server().await;
    let client = authed_client(&base);

    // 1. Open two accounts, deposit, withdraw — reactive CQRS dispatch.
    let alice = client
        .open_account("alice", 1_000)
        .into_future()
        .await
        .unwrap()
        .expect("opened alice");
    assert_eq!(alice.balance, 1_000);
    assert_eq!(alice.version, 1);

    let bob = client
        .open_account("bob", 0)
        .into_future()
        .await
        .unwrap()
        .expect("opened bob");
    assert_eq!(bob.balance, 0);

    let after_deposit = client
        .deposit(&alice.id, 500)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_deposit.balance, 1_500);

    let after_withdraw = client
        .withdraw(&alice.id, 200)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_withdraw.balance, 1_300);

    // 2. Transfer that completes (saga happy path): alice → bob, 300.
    let ok = client
        .transfer(&alice.id, &bob.id, 300)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok.status, "completed");
    assert_eq!(ok.steps_executed, ["debit", "credit"]);
    assert!(ok.steps_rolled_back.is_empty());

    // The projection reflects the moved funds: alice 1300 − 300 = 1000,
    // bob 0 + 300 = 300.
    let alice_view = await_balance(&client, &alice.id, 1_000).await;
    let bob_view = await_balance(&client, &bob.id, 300).await;
    assert_eq!(alice_view.owner, "alice");
    assert_eq!(bob_view.owner, "bob");

    // 3. Transfer that fails and COMPENSATES: bob → alice, 10_000 (overdraft).
    //    The server returns a 422; the SDK Mono surfaces it as a terminal
    //    error carrying the compensation detail.
    let failed = client
        .transfer(&bob.id, &alice.id, 10_000)
        .into_future()
        .await;
    let err = failed.expect_err("overdrawn transfer must fail");
    assert_eq!(err.status, 422);
    assert!(
        err.detail.contains("insufficient funds"),
        "detail: {}",
        err.detail
    );

    // The compensation left both balances intact (no funds moved).
    let bob_after = await_balance(&client, &bob.id, 300).await;
    let alice_after = await_balance(&client, &alice.id, 1_000).await;
    assert_eq!(bob_after.balance, 300);
    assert_eq!(alice_after.balance, 1_000);

    // 4. The /events streaming endpoint emits alice's events as NDJSON,
    //    consumed reactively as a Flux. Alice's stream: open(1000),
    //    deposit(500), withdraw(200), transfer-debit(300).
    let events: Vec<AccountEvent> = client
        .stream_events(&alice.id)
        .collect_list()
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.len(), 4, "events: {events:?}");
    assert_eq!(events[0].event_type, "AccountOpened");
    assert_eq!(events[0].amount, 1_000);
    assert_eq!(events[1].event_type, "MoneyDeposited");
    assert_eq!(events[1].amount, 500);
    assert_eq!(events[2].event_type, "MoneyWithdrawn");
    assert_eq!(events[2].amount, -200);
    assert_eq!(events[3].event_type, "MoneyWithdrawn"); // the transfer debit
    assert_eq!(events[3].amount, -300);
    // Versions are monotonic 1..=4.
    let versions: Vec<i64> = events.iter().map(|e| e.version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn jwt_auth_is_enforced_on_mutating_routes() {
    let (base, _server) = spawn_server().await;

    // No token → 401 on a mutating route.
    let anon = WebClientBuilder::new(&base).build();
    let resp = anon
        .post()
        .uri("/api/v1/accounts")
        .body(&serde_json::json!({ "owner": "mallory", "openingBalance": 100 }))
        .retrieve()
        .exchange()
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resp.status(), 401, "missing token must be unauthorized");

    // A bad token → 401.
    let bad = BankClient::new(&base, "not-a-real-jwt");
    let bad_err = bad
        .open_account("mallory", 100)
        .into_future()
        .await
        .expect_err("bad token must be rejected");
    assert_eq!(bad_err.status, 401);

    // A valid CUSTOMER token → 200/201.
    let good = authed_client(&base);
    let opened = good
        .open_account("alice", 100)
        .into_future()
        .await
        .unwrap()
        .expect("valid token opens an account");
    assert_eq!(opened.balance, 100);
}

#[tokio::test]
async fn reads_and_streams_are_public_but_writes_are_not() {
    let (base, _server) = spawn_server().await;
    let authed = authed_client(&base);

    // Open an account with a valid token.
    let acc = authed
        .open_account("alice", 250)
        .into_future()
        .await
        .unwrap()
        .unwrap();

    // The GET read and the /events stream are public (no token needed).
    let anon = WebClientBuilder::new(&base).build();
    let view: Option<AccountView> = anon
        .get()
        .uri(format!("/api/v1/accounts/{}", acc.id))
        .retrieve()
        .body_to_mono::<AccountView>()
        .into_future()
        .await
        .unwrap();
    assert_eq!(view.unwrap().balance, 250);

    let event_flux: Flux<AccountEvent> = anon
        .get()
        .uri(format!("/api/v1/accounts/{}/events", acc.id))
        .header("Accept", "application/x-ndjson")
        .retrieve()
        .body_to_flux::<AccountEvent>();
    let events: Vec<AccountEvent> = event_flux
        .collect_list()
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "AccountOpened");
}

/// A separate, deliberately tiny check that the streaming endpoint also
/// honours the SSE wire format on request (`?format=sse`).
#[tokio::test]
async fn events_endpoint_supports_sse_format() {
    let (base, _server) = spawn_server().await;
    let authed = authed_client(&base);
    let acc = authed
        .open_account("alice", 100)
        .into_future()
        .await
        .unwrap()
        .unwrap();

    let anon = WebClientBuilder::new(&base).build();
    // The WebClient's body_to_flux auto-detects text/event-stream from the
    // response Content-Type and parses SSE frames.
    let events: Vec<AccountEvent> = anon
        .get()
        .uri(format!("/api/v1/accounts/{}/events?format=sse", acc.id))
        .retrieve()
        .body_to_flux::<AccountEvent>()
        .collect_list()
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "AccountOpened");

    // Keep the Mono import used (a body_to_mono call path).
    let _: Mono<AccountView> = authed.get_account(&acc.id);
}
