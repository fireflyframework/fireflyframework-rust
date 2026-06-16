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

//! # firefly-eda-kafka
//!
//! The framework's **Apache Kafka transport** for the
//! [`firefly_eda`] event-driven-architecture port. [`KafkaBroker`]
//! implements the same [`Publisher`](firefly_eda::Publisher) /
//! [`Subscriber`](firefly_eda::Subscriber) /
//! [`Broker`](firefly_eda::Broker) surfaces as the in-memory broker, so
//! services written against [`firefly_eda`] switch to Kafka by swapping
//! the constructor — no handler changes.
//!
//! It is built on [`rdkafka`] (a binding over the `librdkafka` C
//! library): publishes go through a [`FutureProducer`] and each
//! subscribed topic gets a dedicated [`StreamConsumer`] reading under a
//! shared consumer group.
//!
//! ## Wire format
//!
//! An [`Event`](firefly_eda::Event) is mapped onto a Kafka record by:
//!
//! - **value** — the canonical [`Event`](firefly_eda::Event) JSON
//!   (`id` / `type` / `source` / `topic` / `correlationId` / `time` /
//!   `headers` / `payload`), byte-for-byte the shape every Firefly port
//!   emits, produced with [`serde_json`];
//! - **key** — the event's `correlation_id` when present, else its
//!   `id`, so events sharing a correlation land on the same partition
//!   and preserve per-correlation ordering;
//! - **topic** — the event's `topic` field (the pyfly `destination`);
//! - **headers** — every [`Event`](firefly_eda::Event) header copied
//!   onto the Kafka record as a UTF-8 header.
//!
//! The consumer reverses the mapping: it deserializes the record value
//! back into an [`Event`](firefly_eda::Event) and dispatches to every
//! handler subscribed to that topic.
//!
//! ## Consumer loop & error isolation
//!
//! Each subscribed topic runs a `recv` loop on its own Tokio task,
//! mirroring pyfly's `_consume_loop`: a record that fails to
//! deserialize is **logged and skipped**, and a handler that returns an
//! error is **logged and the loop continues** — one poison message
//! never stalls the stream. The loops shut down cleanly on
//! [`KafkaBroker::close`] via a shared [`CancellationToken`].
//!
//! ## Quick start
//!
//! ```no_run
//! use firefly_eda::{handler, Event, Publisher, Subscriber};
//! use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let broker = new_kafka_broker(KafkaConfig {
//!     brokers: vec!["localhost:9092".into()],
//!     consumer_group: "orders-svc".into(),
//!     ..Default::default()
//! })?;
//!
//! broker
//!     .subscribe(
//!         "orders.created",
//!         handler(|ev: Event| async move {
//!             println!("got order {}", ev.id);
//!             Ok(())
//!         }),
//!     )
//!     .await?;
//!
//! let ev = Event::new("orders.created", "OrderCreated", "orders-svc", None);
//! broker.publish(ev).await?;
//! // `close` is inherited from both ports, so disambiguate; both
//! // release the whole broker.
//! Publisher::close(&*broker).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## pyfly parity
//!
//! This is the Rust analog of pyfly's `KafkaEventBus` (aiokafka): a
//! producer plus a consumer-group loop with per-message error
//! isolation. The differences are idiomatic, not behavioral:
//!
//! - pyfly subscribes by `fnmatch` `event_type` pattern over a fixed
//!   topic list; the Rust [`Subscriber`](firefly_eda::Subscriber) port
//!   is topic-based, so [`KafkaBroker`] subscribes by Kafka topic (the
//!   pyfly `destination`). The wire format and per-message isolation
//!   are identical.
//! - pyfly carries an injected `EventSerializer`; Rust uses the
//!   canonical [`Event`](firefly_eda::Event) JSON codec directly (Avro /
//!   Protobuf are `NotImplementedError` stubs in pyfly too).

#![warn(missing_docs)]

mod broker;
mod config;

pub use broker::{new_kafka_broker, KafkaBroker};
pub use config::KafkaConfig;

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = "26.6.24";
