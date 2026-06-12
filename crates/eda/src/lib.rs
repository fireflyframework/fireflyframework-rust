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

#![warn(missing_docs)]

mod error;
mod event;
mod inmemory;
mod kafka;
mod ports;
mod rabbitmq;

pub use error::{EdaError, EdaResult};
pub use event::Event;
pub use inmemory::InMemoryBroker;
pub use kafka::{new_kafka_broker, KafkaConfig};
pub use ports::{handler, Broker, Handler, HandlerFuture, Publisher, Subscriber};
pub use rabbitmq::{new_rabbitmq_broker, RabbitMqConfig};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = "26.6.1";
