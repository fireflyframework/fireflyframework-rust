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

//! **Real-infrastructure** end-to-end test — the genuine cross-infra
//! integration the orchestrator runs against the live stack.
//!
//! It is gated on `FIREFLY_TEST_POSTGRES_URL` **and**
//! `FIREFLY_TEST_KAFKA_BROKERS`: when either is unset the test **skips
//! cleanly** (prints a notice and returns), so `cargo test` on a bare
//! machine stays green. When both are set it runs the identical
//! open → deposit → withdraw → transfer(complete) → transfer(compensate) →
//! projection → streaming-events flow as the in-process e2e, but against:
//!
//! - a real **Postgres reactive repository** (the read model), and
//! - a real **Kafka event bus** (domain-event publish + projection
//!   consume),
//!
//! driving it with the reactive `WebClient` SDK.
//!
//! Because Kafka delivery is asynchronous (consumer-group lag), the read
//! model converges eventually; the test polls `GET /accounts/:id` with a
//! bounded budget rather than sleeping a fixed amount.

use std::sync::Arc;
use std::time::Duration;

use firefly_eda::Broker;
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};
use firefly_eventsourcing::{EventStore, MemoryEventStore};
use firefly_reactive::Mono;
use firefly_sample_reactive_banking::domain::{AccountEvent, AccountView};
use firefly_sample_reactive_banking::repository::{new_postgres, DDL};
use firefly_sample_reactive_banking::sdk::BankClient;
use firefly_sample_reactive_banking::security::mint_token;
use firefly_sample_reactive_banking::web::{build_app_with, APP_NAME};
use firefly_starter_web::{CoreConfig, WebStack};

/// Reads both gating env vars, returning `None` (and printing a skip notice)
/// when either is missing.
fn infra() -> Option<(String, String)> {
    let pg = std::env::var("FIREFLY_TEST_POSTGRES_URL")
        .ok()
        .filter(|s| !s.is_empty());
    let kafka = std::env::var("FIREFLY_TEST_KAFKA_BROKERS")
        .ok()
        .filter(|s| !s.is_empty());
    match (pg, kafka) {
        (Some(pg), Some(kafka)) => Some((pg, kafka)),
        _ => {
            eprintln!(
                "SKIP real_infra: set FIREFLY_TEST_POSTGRES_URL and \
                 FIREFLY_TEST_KAFKA_BROKERS to run the real cross-infra e2e"
            );
            None
        }
    }
}

/// Polls `get_account` until the projected balance equals `want`, with a
/// generous budget for Kafka consumer lag. Each wait is short (50 ms); the
/// total budget is ~10 s.
async fn await_balance(client: &BankClient, id: &str, want: i64) -> AccountView {
    for _ in 0..200 {
        if let Some(view) = client.get_account(id).into_future().await.unwrap() {
            if view.balance == want {
                return view;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("account {id} never reached balance {want} against real infra");
}

/// Polls the `account_view` table directly until it holds at least `want`
/// rows, with the same ~10 s budget [`await_balance`] uses. The Kafka→
/// projection→Postgres path is eventually consistent, so a synchronous count
/// races the projection; this waits for convergence instead of sleeping a
/// fixed amount. Returns the last observed count.
async fn await_row_count(pg: &tokio_postgres::Client, want: i64) -> i64 {
    let mut count = 0;
    for _ in 0..50 {
        let row = pg
            .query_one("SELECT COUNT(*) FROM \"account_view\"", &[])
            .await
            .unwrap();
        count = row.get(0);
        if count >= want {
            return count;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    count
}

#[tokio::test]
async fn real_postgres_and_kafka_full_flow() {
    let Some((pg_url, kafka_brokers)) = infra() else {
        return; // clean skip
    };

    // --- Real Postgres reactive read-model repository. ---
    let (client, connection) = tokio_postgres::connect(&pg_url, tokio_postgres::NoTls)
        .await
        .expect("connect postgres");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let pg = Arc::new(client);
    // Fresh table per run.
    pg.batch_execute(&format!("DROP TABLE IF EXISTS \"account_view\"; {DDL}"))
        .await
        .expect("provision account_view");
    let repo = new_postgres(Arc::clone(&pg));

    // --- Real Kafka event bus, with a per-run consumer group + topic so
    // concurrent runs don't cross-talk. ---
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let kafka: Arc<dyn Broker> = Arc::from(
        new_kafka_broker(KafkaConfig {
            brokers: kafka_brokers
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect(),
            client_id: format!("{APP_NAME}-{suffix}"),
            consumer_group: format!("{APP_NAME}-{suffix}"),
            ..Default::default()
        })
        .expect("kafka broker"),
    );

    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());

    // Assemble the app over the real infra (Kafka broker on the CoreConfig).
    let app = build_app_with(
        Arc::clone(&store),
        move |mut cfg: CoreConfig| {
            cfg.broker = Some(Arc::clone(&kafka));
            WebStack::new(cfg)
        },
        repo,
    )
    .await;

    // Boot on an ephemeral port and drive with the reactive SDK.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router();
    let _server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let base = format!("http://{addr}");
    let token = mint_token("u-alice", &["CUSTOMER"]);
    let sdk = BankClient::new(&base, &token);

    // open → deposit → withdraw.
    let alice = sdk
        .open_account("alice", 1_000)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    let bob = sdk
        .open_account("bob", 0)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    sdk.deposit(&alice.id, 500)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    sdk.withdraw(&alice.id, 200)
        .into_future()
        .await
        .unwrap()
        .unwrap();

    // transfer that completes (saga happy path).
    let ok = sdk
        .transfer(&alice.id, &bob.id, 300)
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok.status, "completed");

    // transfer that fails + compensates (saga rollback).
    let err = sdk
        .transfer(&bob.id, &alice.id, 99_999)
        .into_future()
        .await
        .expect_err("overdraft must fail");
    assert_eq!(err.status, 422);

    // The Postgres-backed projection (fed off real Kafka) converges:
    // alice 1000 + 500 − 200 − 300 = 1000; bob 0 + 300 = 300.
    let alice_view = await_balance(&sdk, &alice.id, 1_000).await;
    let bob_view = await_balance(&sdk, &bob.id, 300).await;
    assert_eq!(alice_view.owner, "alice");
    assert_eq!(bob_view.owner, "bob");

    // The streaming events endpoint emits alice's events as NDJSON, consumed
    // as a Flux: open, deposit, withdraw, transfer-debit.
    let events: Vec<AccountEvent> = sdk
        .stream_events(&alice.id)
        .collect_list()
        .into_future()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.len(), 4, "events: {events:?}");
    assert_eq!(events[0].event_type, "AccountOpened");

    // Verify the read model is genuinely in Postgres (not just the
    // write-side fallback) by counting rows directly. The Kafka→projection→
    // Postgres path is asynchronous/eventually-consistent, so the synchronous
    // `SELECT COUNT(*)` races the projection — poll for convergence with the
    // same ~10 s budget the balance checks use, rather than a fixed sleep.
    let count = await_row_count(&pg, 2).await;
    assert_eq!(count, 2, "both account views projected into Postgres");

    // Keep the Mono import meaningful.
    let _: Mono<AccountView> = sdk.get_account(&alice.id);
}
