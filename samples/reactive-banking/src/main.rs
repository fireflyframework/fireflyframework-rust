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

//! The reactive-banking process entry point.
//!
//! Boots the whole application — banner, public API on
//! [`DEFAULT_ADDR`](firefly_sample_reactive_banking::web::DEFAULT_ADDR)
//! (`127.0.0.1:8080`), the actuator admin surface on
//! [`DEFAULT_ADMIN_ADDR`](firefly_sample_reactive_banking::web::DEFAULT_ADMIN_ADDR)
//! (`127.0.0.1:8081`), and graceful SIGINT/SIGTERM shutdown through the
//! lifecycle application.
//!
//! Infrastructure is **in-memory by default**. Set
//! `FIREFLY_TEST_POSTGRES_URL` to back the read model with a real Postgres
//! reactive repository, and `FIREFLY_TEST_KAFKA_BROKERS`
//! (comma-separated `host:port`) to publish/consume domain events over a
//! real Kafka cluster. Override the bind addresses with `BANKING_ADDR` /
//! `BANKING_ADMIN_ADDR`.
//!
//! This binary is **not** exercised by tests (they drive
//! [`build_app`](firefly_sample_reactive_banking::build_app) /
//! [`build_router`](firefly_sample_reactive_banking::build_router)
//! in-process against an ephemeral `127.0.0.1:0` server instead).

use std::sync::Arc;

use firefly_eda::Broker;
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};
use firefly_eventsourcing::{EventStore, MemoryEventStore};
use firefly_sample_reactive_banking::repository::{
    new_in_memory, new_postgres, AccountRepository, DDL,
};
use firefly_sample_reactive_banking::web::{
    build_app_with, APP_NAME, DEFAULT_ADDR, DEFAULT_ADMIN_ADDR,
};
use firefly_sample_reactive_banking::VERSION;
use firefly_starter_core::InfoContributor;
use firefly_starter_web::{CoreConfig, WebStack};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- Read-model repository: Postgres when configured, else in-memory. ---
    let (repo, repo_kind): (AccountRepository, &str) =
        match std::env::var("FIREFLY_TEST_POSTGRES_URL") {
            Ok(url) if !url.is_empty() => {
                let (client, connection) =
                    tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
                tokio::spawn(async move {
                    if let Err(e) = connection.await {
                        eprintln!("postgres connection error: {e}");
                    }
                });
                let client = Arc::new(client);
                client.batch_execute(DDL).await?;
                (new_postgres(client), "postgres")
            }
            _ => (new_in_memory(), "in-memory"),
        };

    // --- EDA broker: Kafka when configured (carried on the CoreConfig). ----
    let kafka_broker: Option<Arc<dyn Broker>> = match std::env::var("FIREFLY_TEST_KAFKA_BROKERS") {
        Ok(brokers) if !brokers.is_empty() => {
            let broker = new_kafka_broker(KafkaConfig {
                brokers: brokers.split(',').map(|s| s.trim().to_owned()).collect(),
                client_id: APP_NAME.into(),
                consumer_group: APP_NAME.into(),
                ..Default::default()
            })?;
            Some(Arc::from(broker))
        }
        _ => None,
    };
    let broker_kind = if kafka_broker.is_some() {
        "kafka"
    } else {
        "in-memory"
    };

    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());

    // Assemble the application, threading the (optional) Kafka broker onto
    // the web stack's CoreConfig so every layer shares the one broker.
    let app = build_app_with(
        Arc::clone(&store),
        move |mut cfg: CoreConfig| {
            if let Some(broker) = kafka_broker {
                cfg.broker = Some(broker);
            }
            WebStack::new(cfg)
        },
        repo,
    )
    .await;

    // Best-effort: a test harness may already own the global subscriber.
    let _ = app.web.init_logging();

    let api = app.router();
    let contributor: InfoContributor = Box::new(move || {
        let mut info = serde_json::Map::new();
        info.insert(
            "sample".into(),
            serde_json::json!({
                "name": "reactive-banking",
                "readModel": repo_kind,
                "eventBus": broker_kind,
            }),
        );
        info
    });
    let admin = app.web.actuator_router(vec![contributor]);

    app.web.print_banner();
    println!(":: reactive-banking :: read-model={repo_kind} event-bus={broker_kind} (v{VERSION})");

    let api_addr = std::env::var("BANKING_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let admin_addr =
        std::env::var("BANKING_ADMIN_ADDR").unwrap_or_else(|_| DEFAULT_ADMIN_ADDR.to_owned());

    let application = app
        .web
        .new_application()
        .on_server("api", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind(&api_addr).await?;
            axum::serve(listener, api)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        })
        .on_server("admin", move |shutdown| async move {
            let listener = tokio::net::TcpListener::bind(&admin_addr).await?;
            axum::serve(listener, admin)
                .with_graceful_shutdown(shutdown.wait())
                .await?;
            Ok(())
        });

    if let Err(err) = application.run().await {
        if !err.is_cancelled() {
            eprintln!("application failed: {err}");
            std::process::exit(1);
        }
    }
    Ok(())
}
