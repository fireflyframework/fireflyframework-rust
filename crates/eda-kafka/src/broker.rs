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

//! The [`KafkaBroker`] adapter: a [`FutureProducer`] plus a
//! [`StreamConsumer`] per subscribed topic.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use firefly_eda::{Broker, EdaError, EdaResult, Event, Handler, Publisher, Subscriber};
use firefly_kernel::FireflyError;
use rdkafka::consumer::StreamConsumer;
use rdkafka::message::{Header, Message, OwnedHeaders};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use rdkafka::util::Timeout;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::KafkaConfig;

/// Queue timeout handed to [`FutureProducer::send`] — how long a full
/// local queue blocks before the publish errors. Generous so transient
/// backpressure does not surface as a publish failure.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// An Apache Kafka [`Broker`]: publishes [`Event`]s through a shared
/// [`FutureProducer`] and runs a dedicated [`StreamConsumer`] loop per
/// subscribed topic, all under the configured consumer group.
///
/// Construct one with [`new_kafka_broker`]. Cloning the producer is
/// cheap (it wraps an `Arc`), but the broker itself owns the consumer
/// tasks, so keep a single instance per service and share it behind an
/// `Arc` like the in-memory broker.
pub struct KafkaBroker {
    producer: FutureProducer,
    config: KafkaConfig,
    /// Broadcasts the shutdown signal to every consumer loop; the value
    /// flips to `true` exactly once, on [`KafkaBroker::close`].
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    /// Join handles for the per-topic consumer loops, drained on close.
    consumers: Mutex<Vec<JoinHandle<()>>>,
}

impl KafkaBroker {
    /// Builds a [`KafkaBroker`] from `config`, creating the producer
    /// immediately. Returns [`EdaError::KafkaUnavailable`] if the
    /// `librdkafka` producer cannot be constructed (e.g. an invalid
    /// `bootstrap.servers`); the error preserves the underlying cause in
    /// its source chain.
    pub fn new(config: KafkaConfig) -> EdaResult<Self> {
        let producer: FutureProducer = config
            .producer_config()
            .create()
            .map_err(|e| kafka_unavailable("producer create", &e))?;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Ok(Self {
            producer,
            config,
            shutdown_tx,
            shutdown_rx,
            consumers: Mutex::new(Vec::new()),
        })
    }

    /// Publishes `ev` to its [`Event::topic`]. The record value is the
    /// canonical [`Event`] JSON, the key is the correlation id (falling
    /// back to the event id), and every event header is copied onto the
    /// Kafka record. See [the module docs](crate) for the full mapping.
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        if *self.shutdown_rx.borrow() {
            return Err(EdaError::Closed);
        }
        let record = EventRecord::encode(&ev)?;
        let topic = ev.topic.clone();
        let mut future_record = FutureRecord::to(&topic)
            .payload(&record.value)
            .key(&record.key);
        if let Some(headers) = record.headers.clone() {
            future_record = future_record.headers(headers);
        }
        self.producer
            .send(future_record, Timeout::After(SEND_TIMEOUT))
            .await
            .map_err(|(e, _msg)| kafka_internal("publish", &e))?;
        Ok(())
    }

    /// Subscribes `handler` to every record on Kafka `topic`, spawning a
    /// dedicated consumer loop. Returns [`EdaError::KafkaUnavailable`]
    /// if the consumer for the topic cannot be created.
    async fn subscribe(&self, topic: &str, handler: Handler) -> EdaResult<()> {
        if *self.shutdown_rx.borrow() {
            return Err(EdaError::Closed);
        }
        let consumer: StreamConsumer = self
            .config
            .consumer_config()
            .create()
            .map_err(|e| kafka_unavailable("consumer create", &e))?;
        rdkafka::consumer::Consumer::subscribe(&consumer, &[topic])
            .map_err(|e| kafka_unavailable("consumer subscribe", &e))?;

        let topic = topic.to_string();
        let shutdown = self.shutdown_rx.clone();
        let join = tokio::spawn(consume_loop(consumer, topic, handler, shutdown));
        self.consumers
            .lock()
            .expect("firefly/eda-kafka: consumers lock poisoned")
            .push(join);
        Ok(())
    }

    /// Signals every consumer loop to stop and flushes the producer.
    /// Idempotent — calling it again is a no-op that still returns
    /// `Ok(())`. After close, publish/subscribe return
    /// [`EdaError::Closed`].
    async fn close(&self) -> EdaResult<()> {
        // Flip the shutdown flag; every consume_loop observes it and
        // returns. send() ignores the error when there are no receivers.
        let _ = self.shutdown_tx.send(true);

        let handles: Vec<JoinHandle<()>> = {
            let mut guard = self
                .consumers
                .lock()
                .expect("firefly/eda-kafka: consumers lock poisoned");
            std::mem::take(&mut *guard)
        };
        for handle in handles {
            // A consumer loop only returns after observing shutdown, so
            // joining cannot hang; ignore a JoinError from an aborted task.
            let _ = handle.await;
        }

        // Best-effort flush of any in-flight produces.
        let _ = self.producer.flush(Timeout::After(SEND_TIMEOUT));
        Ok(())
    }
}

#[async_trait]
impl Publisher for KafkaBroker {
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        KafkaBroker::publish(self, ev).await
    }

    async fn close(&self) -> EdaResult<()> {
        KafkaBroker::close(self).await
    }
}

#[async_trait]
impl Subscriber for KafkaBroker {
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()> {
        KafkaBroker::subscribe(self, topic, h).await
    }

    async fn close(&self) -> EdaResult<()> {
        KafkaBroker::close(self).await
    }
}

/// Constructs a Kafka-backed [`Broker`] (the concrete [`KafkaBroker`]),
/// the production replacement for [`firefly_eda::new_kafka_broker`]'s
/// [`EdaError::KafkaUnavailable`] sentinel.
///
/// ```no_run
/// use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};
///
/// let broker = new_kafka_broker(KafkaConfig {
///     brokers: vec!["localhost:9092".into()],
///     consumer_group: "svc".into(),
///     ..Default::default()
/// })
/// .expect("kafka producer");
/// ```
pub fn new_kafka_broker(config: KafkaConfig) -> EdaResult<Box<dyn Broker>> {
    Ok(Box::new(KafkaBroker::new(config)?))
}

/// The per-topic consumer loop, mirroring pyfly's `_consume_loop`:
/// `recv` a record, decode it into an [`Event`], dispatch to `handler`,
/// and isolate every per-message failure (log + continue) so one poison
/// record never stalls the stream. Returns when `shutdown` flips to
/// `true`.
async fn consume_loop(
    consumer: StreamConsumer,
    topic: String,
    handler: Handler,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            // Shutdown wins so close() returns promptly.
            res = shutdown.changed() => {
                // Sender dropped or value flipped to true -> stop.
                if res.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            recv = consumer.recv() => {
                match recv {
                    Ok(msg) => {
                        let offset = msg.offset();
                        match EventRecord::decode(&msg) {
                            Ok(ev) => {
                                if let Err(err) = handler(ev).await {
                                    tracing::error!(
                                        topic = %topic,
                                        offset,
                                        error = %err,
                                        "firefly/eda-kafka: handler returned an error; continuing",
                                    );
                                }
                            }
                            Err(err) => {
                                tracing::error!(
                                    topic = %topic,
                                    offset,
                                    error = %err,
                                    "firefly/eda-kafka: failed to decode record; skipping",
                                );
                            }
                        }
                    }
                    Err(err) => {
                        // Transient broker / network error: log and retry
                        // the recv rather than tear the loop down.
                        tracing::error!(
                            topic = %topic,
                            error = %err,
                            "firefly/eda-kafka: consumer recv error; continuing",
                        );
                    }
                }
            }
        }
    }
}

/// The wire image of an [`Event`] as a Kafka record: JSON value, routing
/// key, and optional headers. Extracted so the [`Event`]&harr;record
/// mapping is unit-testable without a live cluster.
struct EventRecord {
    /// The canonical [`Event`] JSON, used as the record value.
    value: Vec<u8>,
    /// The partition key — the correlation id, or the event id when
    /// there is no correlation id.
    key: String,
    /// Kafka record headers built from the event headers, or `None`
    /// when the event has none.
    headers: Option<OwnedHeaders>,
}

impl EventRecord {
    /// Encodes `ev` into its Kafka record image.
    fn encode(ev: &Event) -> EdaResult<Self> {
        let value = serde_json::to_vec(ev).map_err(|e| {
            EdaError::Handler(FireflyError::internal(format!(
                "firefly/eda-kafka: serialize event {}: {e}",
                ev.id
            )))
        })?;
        let key = if ev.correlation_id.is_empty() {
            ev.id.clone()
        } else {
            ev.correlation_id.clone()
        };
        let headers = if ev.headers.is_empty() {
            None
        } else {
            let mut owned = OwnedHeaders::new();
            for (k, v) in &ev.headers {
                owned = owned.insert(Header {
                    key: k,
                    value: Some(v),
                });
            }
            Some(owned)
        };
        Ok(Self {
            value,
            key,
            headers,
        })
    }

    /// Decodes a consumed Kafka message back into an [`Event`] by
    /// deserializing its value as the canonical [`Event`] JSON. Kafka
    /// record headers are not merged back in: they round-trip inside the
    /// JSON envelope already, matching pyfly which deserializes the
    /// envelope value and ignores transport headers on the read path.
    fn decode<M: Message>(msg: &M) -> EdaResult<Event> {
        let payload = msg.payload().ok_or_else(|| {
            EdaError::Handler(FireflyError::internal(
                "firefly/eda-kafka: record has no payload",
            ))
        })?;
        serde_json::from_slice(payload).map_err(|e| {
            EdaError::Handler(FireflyError::internal(format!(
                "firefly/eda-kafka: deserialize event: {e}"
            )))
        })
    }
}

/// Wraps a `librdkafka` error as [`EdaError::KafkaUnavailable`] after
/// logging the underlying cause — used on construction failures where
/// the missing/unreachable transport is the right signal.
fn kafka_unavailable(stage: &str, err: &rdkafka::error::KafkaError) -> EdaError {
    tracing::error!(stage, error = %err, "firefly/eda-kafka: {stage} failed");
    EdaError::KafkaUnavailable
}

/// Wraps a `librdkafka` error as an internal [`FireflyError`] carried in
/// [`EdaError::Handler`] — used for operational failures (e.g. a failed
/// publish) where the transport exists but the operation did not
/// succeed.
fn kafka_internal(stage: &str, err: &rdkafka::error::KafkaError) -> EdaError {
    EdaError::Handler(FireflyError::internal(format!(
        "firefly/eda-kafka: {stage}: {err}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdkafka::message::{Headers, OwnedMessage};
    use rdkafka::Timestamp;

    fn sample_event() -> Event {
        let mut ev = Event::new(
            "orders.created",
            "OrderCreated",
            "orders-svc",
            Some(br#"{"id":"o1"}"#.to_vec()),
        );
        ev.id = "evt-1".into();
        ev.correlation_id = "corr-1".into();
        ev = ev.with_header("tenant", "t1");
        ev
    }

    #[test]
    fn encode_value_is_canonical_event_json() {
        let ev = sample_event();
        let record = EventRecord::encode(&ev).unwrap();
        let back: Event = serde_json::from_slice(&record.value).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn encode_key_is_correlation_id_when_present() {
        let ev = sample_event();
        let record = EventRecord::encode(&ev).unwrap();
        assert_eq!(record.key, "corr-1");
    }

    #[test]
    fn encode_key_falls_back_to_event_id() {
        let mut ev = sample_event();
        ev.correlation_id = String::new();
        let record = EventRecord::encode(&ev).unwrap();
        assert_eq!(record.key, "evt-1");
    }

    #[test]
    fn encode_headers_carry_every_event_header() {
        let ev = sample_event().with_header("region", "eu");
        let record = EventRecord::encode(&ev).unwrap();
        let headers = record.headers.expect("headers present");
        assert_eq!(headers.count(), 2);
        // Collect into a map to assert independent of header order.
        let mut found = std::collections::BTreeMap::new();
        for i in 0..headers.count() {
            let h = headers.get(i);
            let value = h.value.map(|b| String::from_utf8_lossy(b).into_owned());
            found.insert(h.key.to_string(), value.unwrap_or_default());
        }
        assert_eq!(found.get("tenant").map(String::as_str), Some("t1"));
        assert_eq!(found.get("region").map(String::as_str), Some("eu"));
    }

    #[test]
    fn encode_headers_none_when_event_has_no_headers() {
        let mut ev = sample_event();
        ev.headers.clear();
        let record = EventRecord::encode(&ev).unwrap();
        assert!(record.headers.is_none());
    }

    #[test]
    fn decode_round_trips_an_encoded_event() {
        let ev = sample_event();
        let record = EventRecord::encode(&ev).unwrap();
        let msg = OwnedMessage::new(
            Some(record.value),
            Some(record.key.into_bytes()),
            ev.topic.clone(),
            Timestamp::NotAvailable,
            0,
            0,
            None,
        );
        let back = EventRecord::decode(&msg).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn decode_errors_when_payload_missing() {
        let msg = OwnedMessage::new(
            None,
            None,
            "orders.created".into(),
            Timestamp::NotAvailable,
            0,
            0,
            None,
        );
        let err = EventRecord::decode(&msg).unwrap_err();
        assert!(matches!(err, EdaError::Handler(_)));
    }

    #[test]
    fn decode_errors_on_non_event_json() {
        let msg = OwnedMessage::new(
            Some(b"not json".to_vec()),
            None,
            "orders.created".into(),
            Timestamp::NotAvailable,
            0,
            0,
            None,
        );
        assert!(EventRecord::decode(&msg).is_err());
    }

    #[tokio::test]
    async fn new_broker_succeeds_without_a_cluster() {
        // Producer creation is local — it does not dial brokers — so a
        // valid config constructs even with nothing listening.
        let broker = KafkaBroker::new(KafkaConfig {
            brokers: vec!["localhost:9092".into()],
            consumer_group: "test".into(),
            ..Default::default()
        });
        assert!(broker.is_ok());
    }

    #[tokio::test]
    async fn close_is_idempotent_and_then_rejects() {
        let broker = KafkaBroker::new(KafkaConfig {
            brokers: vec!["localhost:9092".into()],
            ..Default::default()
        })
        .unwrap();
        Publisher::close(&broker).await.unwrap();
        // Second close still succeeds.
        Publisher::close(&broker).await.unwrap();
        // Publish after close is rejected with Closed (no network touch).
        let ev = Event::new("t", "T", "s", None);
        assert!(matches!(
            Publisher::publish(&broker, ev).await,
            Err(EdaError::Closed)
        ));
    }

    #[tokio::test]
    async fn subscribe_after_close_rejects() {
        let broker = KafkaBroker::new(KafkaConfig::default()).unwrap();
        Publisher::close(&broker).await.unwrap();
        let h = firefly_eda::handler(|_ev| async { Ok(()) });
        assert!(matches!(
            Subscriber::subscribe(&broker, "t", h).await,
            Err(EdaError::Closed)
        ));
    }

    #[test]
    fn new_kafka_broker_returns_boxed_broker() {
        let broker = new_kafka_broker(KafkaConfig {
            brokers: vec!["localhost:9092".into()],
            ..Default::default()
        });
        assert!(broker.is_ok());
    }
}
