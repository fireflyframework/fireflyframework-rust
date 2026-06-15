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

//! The bridge between the in-process event system and the EDA message broker —
//! a process-wide [`Broker`] registry plus transaction-aware externalization.
//!
//! Spring (and Spring Modulith) keep two layers distinct: in-process
//! `@EventListener` / `@TransactionalEventListener` events
//! (`firefly_transactional`'s [`publish_event`](firefly_transactional::publish_event)),
//! and the message broker. The canonical way they cooperate is to publish a
//! domain event in-process and forward it to the broker **after the transaction
//! commits**, so a message is never sent for work that rolled back. This module
//! provides both halves of that bridge:
//!
//! * [`register_broker`] / [`broker`] — a process-wide [`Broker`] handle (first
//!   registration wins, like the transaction manager), so a free-function event
//!   listener or any service can reach the broker without threading it through.
//! * [`publish_to_broker`] — serialize a value to JSON and publish it on a
//!   topic through the registered broker.
//! * [`externalize_after_commit`] — register an after-commit transactional
//!   listener that forwards every in-process event of type `E` to the broker.
//!   The Rust analog of Spring Modulith's externalized events: call it once at
//!   startup, then `publish_event(e)` and the commit forwards `e` to the broker.

use std::any::Any;
use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::ports::Broker;
use crate::{EdaError, EdaResult, Event};

/// The source stamped on events forwarded by [`externalize_after_commit`].
const EXTERNALIZED_SOURCE: &str = "firefly";

/// The process-wide EDA broker.
static BROKER: OnceLock<Arc<dyn Broker>> = OnceLock::new();

/// Registers the process-wide EDA broker. Returns `false` if one was already
/// registered (first registration wins, mirroring the transaction manager and
/// cache adapter). Typically called once by a starter / auto-configuration.
pub fn register_broker(broker: Arc<dyn Broker>) -> bool {
    BROKER.set(broker).is_ok()
}

/// The registered process-wide EDA broker, if any.
#[must_use]
pub fn broker() -> Option<Arc<dyn Broker>> {
    BROKER.get().cloned()
}

/// Serializes `payload` to JSON and publishes it on `topic` through the given
/// broker. The explicit-broker form, useful in tests and multi-broker setups.
pub async fn publish_to_broker_on<E: Serialize + ?Sized>(
    broker: &Arc<dyn Broker>,
    topic: impl Into<String>,
    event_type: impl Into<String>,
    source: impl Into<String>,
    payload: &E,
) -> EdaResult<()> {
    let bytes = serde_json::to_vec(payload).map_err(|e| EdaError::Serialization {
        serializer: "json".to_string(),
        message: e.to_string(),
    })?;
    broker
        .publish(Event::new(topic, event_type, source, Some(bytes)))
        .await
}

/// Serializes `payload` to JSON and publishes it on `topic` through the
/// process-wide [`broker`]. Errors with [`EdaError::BrokerUnavailable`] if no
/// broker is registered. Call this from an event listener (or anywhere) to push
/// a domain payload onto the broker.
pub async fn publish_to_broker<E: Serialize + ?Sized>(
    topic: impl Into<String>,
    event_type: impl Into<String>,
    source: impl Into<String>,
    payload: &E,
) -> EdaResult<()> {
    let broker = broker().ok_or(EdaError::BrokerUnavailable)?;
    publish_to_broker_on(&broker, topic, event_type, source, payload).await
}

/// Bridges in-process events of type `E` to the EDA broker: registers an
/// after-commit transactional listener that serializes each `E` published with
/// [`publish_event`](firefly_transactional::publish_event) and forwards it to
/// `topic`. Call once at startup, per externalized event type.
///
/// Because it rides the [`AfterCommit`](firefly_transactional::TransactionPhase::AfterCommit)
/// phase, the event reaches the broker only once the surrounding transaction
/// commits — never for a rolled-back one (Spring's after-commit publication).
/// With no active transaction the event is forwarded immediately (the
/// no-transaction fallback). Forwarding is best-effort: a missing broker or a
/// publish failure after commit is swallowed (it does not unwind the committed
/// transaction); use a real outbox if you need at-least-once delivery.
pub fn externalize_after_commit<E>(topic: &'static str, event_type: &'static str)
where
    E: Serialize + Any + Send + Sync + 'static,
{
    firefly_transactional::register_event_listener::<E>(
        Some(firefly_transactional::TransactionPhase::AfterCommit),
        Arc::new(move |event: Arc<dyn Any + Send + Sync>| {
            Box::pin(async move {
                if let Some(typed) = event.downcast_ref::<E>() {
                    let _ = publish_to_broker(topic, event_type, EXTERNALIZED_SOURCE, typed).await;
                }
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{Publisher, Subscriber};
    use crate::Handler;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A broker that records every published event synchronously, so tests can
    /// assert delivery without timing concerns.
    struct RecordingBroker {
        sent: Arc<Mutex<Vec<Event>>>,
    }

    #[async_trait]
    impl Publisher for RecordingBroker {
        async fn publish(&self, ev: Event) -> EdaResult<()> {
            self.sent.lock().unwrap().push(ev);
            Ok(())
        }
        async fn close(&self) -> EdaResult<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Subscriber for RecordingBroker {
        async fn subscribe(&self, _topic: &str, _h: Handler) -> EdaResult<()> {
            Ok(())
        }
        async fn close(&self) -> EdaResult<()> {
            Ok(())
        }
    }

    // `Broker` is blanket-implemented for any `Publisher + Subscriber`.

    #[derive(Serialize)]
    struct OrderPlaced {
        id: u32,
        total: u64,
    }

    #[tokio::test]
    async fn publish_to_broker_on_serializes_and_publishes() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let broker: Arc<dyn Broker> = Arc::new(RecordingBroker { sent: sent.clone() });
        publish_to_broker_on(
            &broker,
            "orders",
            "order.placed",
            "orders-svc",
            &OrderPlaced { id: 7, total: 350 },
        )
        .await
        .unwrap();

        let events = sent.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].topic, "orders");
        assert_eq!(events[0].event_type, "order.placed");
        let payload = events[0].payload.as_ref().unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(payload).unwrap();
        assert_eq!(decoded["id"], 7);
        assert_eq!(decoded["total"], 350);
    }

    #[tokio::test]
    async fn publish_to_broker_errors_without_a_registered_broker() {
        // This test deliberately does not register the global broker; in the
        // eda test binary no other test registers it either, so `broker()` is
        // empty here.
        let err = publish_to_broker("t", "e", "s", &OrderPlaced { id: 1, total: 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, EdaError::BrokerUnavailable));
    }
}
