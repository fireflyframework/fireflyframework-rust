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

//! End-to-end tests for the declarative event listeners (`#[event_listener]`,
//! `#[transactional_event_listener]`) and the EDA bridge
//! (`externalize_after_commit`), driven entirely through the one-dependency
//! `firefly` facade. They prove the macros register through `inventory`, that
//! transaction-bound listeners respect commit vs. rollback, and that an
//! externalized in-process event reaches the message broker only after the
//! surrounding transaction commits — Spring's after-commit publication.

use std::sync::{Arc, Mutex};

use firefly::eda::{handler, InMemoryBroker};
use firefly::prelude::*;
use firefly::transactional::{register_transaction_manager, transactional, TxError, TxOptions};
use serde::Serialize;

// --- immediate listener (#[event_listener]) --------------------------------

static IMMEDIATE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

struct Pinged {
    n: u32,
}

#[firefly::application_event_listener]
async fn on_pinged(event: &Pinged) {
    IMMEDIATE.lock().unwrap().push(event.n);
}

#[tokio::test]
async fn immediate_event_listener_fires_at_publish() {
    publish_event(Pinged { n: 7 }).await;
    assert!(IMMEDIATE.lock().unwrap().contains(&7));
}

// --- transaction-bound listener (#[transactional_event_listener]) ----------

static TX_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Settled {
    id: u32,
}

#[firefly::transactional_event_listener] // defaults to after_commit
async fn on_settled(event: &Settled) {
    TX_LOG.lock().unwrap().push(format!("commit:{}", event.id));
}

#[tokio::test]
async fn transactional_event_listener_fires_only_after_commit() {
    register_transaction_manager(Arc::new(LocalTransactionManager));

    // A committing transaction: the listener fires.
    let committed: Result<(), TxError> = transactional(TxOptions::required(), || async {
        publish_event(Settled { id: 1 }).await;
        Ok(())
    })
    .await;
    assert!(committed.is_ok());

    // A rolling-back transaction: the listener must NOT fire.
    let rolled_back: Result<(), TxError> = transactional(TxOptions::required(), || async {
        publish_event(Settled { id: 2 }).await;
        Err(TxError::application("rollback"))
    })
    .await;
    assert!(rolled_back.is_err());

    let log = TX_LOG.lock().unwrap();
    assert!(log.contains(&"commit:1".to_string()), "log was {log:?}");
    assert!(!log.contains(&"commit:2".to_string()), "log was {log:?}");
}

// --- EDA externalization (works with the broker, Spring-style) -------------

static BROKER_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Serialize)]
struct WalletOpened {
    wallet: String,
}

#[tokio::test]
async fn externalized_event_reaches_the_broker_after_commit() {
    // Register the in-memory broker and record what arrives on the topic.
    let broker = Arc::new(InMemoryBroker::new());
    register_broker(broker.clone());
    broker
        .subscribe(
            "wallet.events",
            handler(|ev| async move {
                let payload = ev.payload.unwrap_or_default();
                let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                BROKER_LOG
                    .lock()
                    .unwrap()
                    .push(value["wallet"].as_str().unwrap().to_string());
                Ok::<(), FireflyError>(())
            }),
        )
        .unwrap();

    register_transaction_manager(Arc::new(LocalTransactionManager));
    externalize_after_commit::<WalletOpened>("wallet.events", "wallet.opened");

    // A committing transaction: the event is forwarded to the broker.
    let committed: Result<(), TxError> = transactional(TxOptions::required(), || async {
        publish_event(WalletOpened {
            wallet: "w-keep".into(),
        })
        .await;
        Ok(())
    })
    .await;
    assert!(committed.is_ok());

    // A rolling-back transaction: the event must NOT reach the broker.
    let rolled_back: Result<(), TxError> = transactional(TxOptions::required(), || async {
        publish_event(WalletOpened {
            wallet: "w-drop".into(),
        })
        .await;
        Err(TxError::application("rollback"))
    })
    .await;
    assert!(rolled_back.is_err());

    let log = BROKER_LOG.lock().unwrap();
    assert!(
        log.contains(&"w-keep".to_string()),
        "committed event should reach the broker: {log:?}"
    );
    assert!(
        !log.contains(&"w-drop".to_string()),
        "rolled-back event must not reach the broker: {log:?}"
    );
}
