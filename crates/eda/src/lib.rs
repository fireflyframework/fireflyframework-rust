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

//! # firefly-eda
//!
//! The framework's **event-driven architecture port**. It defines the
//! [`Event`] envelope every Firefly event flows through, the
//! [`Publisher`] / [`Subscriber`] / [`Broker`] ports, and an in-process
//! fan-out [`InMemoryBroker`]. Production transports — Kafka and
//! RabbitMQ — share the same ports and slot in via
//! [`new_kafka_broker`] / [`new_rabbitmq_broker`] once the dedicated
//! transport crates ship.
//!
//! Until those land, [`new_kafka_broker`] and [`new_rabbitmq_broker`]
//! return the typed sentinels [`EdaError::KafkaUnavailable`] and
//! [`EdaError::RabbitMqUnavailable`] so a misconfigured deployment
//! fails loud at startup rather than silently falling back to
//! in-memory — exactly like the Go module's `ErrKafkaUnavailable` /
//! `ErrRabbitMQUnavailable` sentinels.
//!
//! ## Wire compatibility
//!
//! [`Event`] serializes to the same JSON shape as the Java
//! `firefly-common-eda`, the .NET `FireflyFramework.Eda`, the Go `eda`,
//! and the Python `pyfly` ports: `id` / `type` / `source` / `topic` /
//! `correlationId` (omitted when empty) / `time` (RFC 3339) / `headers`
//! (omitted when empty) / `payload` (standard base64, `null` when
//! absent — Go's `[]byte` encoding).
//!
//! ## Quick start
//!
//! ```
//! use firefly_eda::{handler, Event, InMemoryBroker};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let broker = InMemoryBroker::new();
//!
//! broker
//!     .subscribe(
//!         "orders.created",
//!         handler(|ev: Event| async move {
//!             println!("got order {}", ev.id);
//!             Ok(())
//!         }),
//!     )
//!     .unwrap();
//!
//! let ev = Event::new(
//!     "orders.created",
//!     "OrderCreated",
//!     "orders-svc",
//!     Some(br#"{"id":"o1"}"#.to_vec()),
//! );
//! broker.publish(ev).await.unwrap();
//! broker.close().unwrap();
//! # });
//! ```
//!
//! ## Correlation propagation
//!
//! [`Event::new`] stamps the envelope's `correlationId` from the
//! kernel's task-local correlation scope
//! ([`firefly_kernel::with_correlation_id`]) — the Rust analog of the
//! Go module extracting it from `context.Context` via
//! `kernel.CorrelationIDFrom`.
//!
//! ## Raw-bytes publish convenience
//!
//! [`PublisherExt::publish_bytes`] is a one-call helper over any
//! [`Publisher`] — it builds the [`Event`] (stamping the correlation id
//! from the ambient scope) and publishes it, so the common "send this
//! payload to this topic" call does not spell out [`Event::new`] each
//! time. It is blanket-implemented for every publisher.
//!
//! ## Pluggable event serializer
//!
//! [`EventSerializer`] is the codec port a transport uses to turn an
//! [`Event`] into wire bytes and back (pyfly's `EventSerializer`). The
//! default [`JsonEventSerializer`] encodes via the canonical [`Event`]
//! JSON codec, so it is wire-compatible with every sibling port and a
//! zero-change default; [`AvroEventSerializer`] / [`ProtobufEventSerializer`]
//! are the failing-loud, not-yet-implemented sentinels pyfly ships, and
//! [`serializer_for`] selects one by name (`"json"` / `"avro"` /
//! `"protobuf"`) for a `serialization-format` config key.
//!
//! ## Delivery gates, dead-letter store, and health
//!
//! Three pyfly-parity surfaces layer over the broker:
//!
//! - [`EventFilter`] / [`HeaderEventFilter`] / [`PredicateEventFilter`]
//!   are delivery gates that decide — per envelope — whether a reached
//!   subscription actually runs; attach a chain with [`with_filters`]
//!   (pyfly's `eda.filter`).
//! - [`EdaDeadLetterStore`] / [`EdaDeadLetterEntry`] /
//!   [`InMemoryEdaDeadLetterStore`] capture failed *events* in a
//!   queryable store (list / get / remove). Wire one into
//!   [`ListenerPolicy::dead_letter_store`] so exhausted events are
//!   inspectable, not just republished (pyfly's `eda.dlq`).
//! - [`EventPublisherHealthIndicator`] adapts any [`BrokerHealth`]
//!   broker to a [`firefly_observability::Indicator`], surfacing broker
//!   liveness on `/actuator/health` (pyfly's `eda.health`).
//!
//! ## Reactive surface
//!
//! An *additive* Reactor / WebFlux-style façade layers over
//! [`InMemoryBroker`] without touching any existing signature or wire
//! format:
//!
//! - [`InMemoryBroker::subscribe_reactive`] turns a topic subscription
//!   into a [`firefly_reactive::Flux<Event>`] (the EDA analog of Reactor
//!   Kafka's `KafkaReceiver.receive()`), backed by a bounded channel with
//!   on-backpressure-drop semantics.
//! - [`InMemoryBroker::publish_mono`] is the cold, reactive publish
//!   helper returning a [`firefly_reactive::Mono`] (a reactive
//!   `KafkaTemplate.send(..)` → `Mono<Void>`).

#![warn(missing_docs)]

mod discovery;
mod dlq;
mod error;
mod event;
mod filter;
mod health;
mod inmemory;
mod kafka;
mod listener;
mod ports;
mod rabbitmq;
mod reactive;
mod registry;
mod serializer;

// Re-export `inventory` so `#[event_listener]`-generated `ListenerRegistration`
// thunks submit through `firefly_eda::inventory`.
pub use inventory;

pub use discovery::{
    discovered_listener_count, subscribe_discovered_listeners, BoxSubscribeFuture,
    ListenerRegistration,
};
pub use dlq::{EdaDeadLetterEntry, EdaDeadLetterStore, InMemoryEdaDeadLetterStore};
pub use error::{EdaError, EdaResult};
pub use event::Event;
pub use filter::{
    with_filter_chain, with_filters, EventFilter, HeaderEventFilter, PredicateEventFilter,
};
pub use health::{BrokerHealth, EventPublisherHealthIndicator};
pub use inmemory::InMemoryBroker;
pub use kafka::{new_kafka_broker, KafkaConfig};
pub use listener::{wrap_listener, ListenerPolicy, HEADER_EXCEPTION, HEADER_ORIGINAL_TOPIC};
pub use ports::{handler, Broker, Handler, HandlerFuture, Publisher, PublisherExt, Subscriber};
pub use rabbitmq::{new_rabbitmq_broker, RabbitMqConfig};
pub use reactive::DEFAULT_REACTIVE_BUFFER;
pub use registry::{
    broker, externalize_after_commit, publish_to_broker, publish_to_broker_on, register_broker,
};
pub use serializer::{
    serializer_for, AvroEventSerializer, EventSerializer, JsonEventSerializer,
    ProtobufEventSerializer,
};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = "26.6.6";
