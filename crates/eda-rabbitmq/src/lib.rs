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

//! # firefly-eda-rabbitmq
//!
//! A RabbitMQ-backed implementation of the [`firefly_eda`] transport
//! ports ([`Publisher`](firefly_eda::Publisher) /
//! [`Subscriber`](firefly_eda::Subscriber) /
//! [`Broker`](firefly_eda::Broker)) over [`lapin`]. It is the Rust port
//! of pyfly's `RabbitMqEventBus` and slots into the EDA starter via
//! [`new_rabbitmq_broker`] in place of the
//! [`firefly_eda::new_rabbitmq_broker`] sentinel.
//!
//! ## Topology
//!
//! On [`start`](RabbitMqBroker::start) the broker declares (see
//! [`RabbitMqBrokerConfig::declaration_plan`]):
//!
//! * one **durable `direct` exchange** (default `pyfly`), and
//! * one **durable queue `<group>.<destination>`** per configured
//!   destination, bound to the exchange with `<destination>` as the
//!   routing key and consumed with **manual ack**.
//!
//! The publishing channel enables **publisher confirms**, so
//! [`publish`](firefly_eda::Publisher::publish) only resolves once the
//! broker has acknowledged the message.
//!
//! ## Delivery semantics (at-least-once)
//!
//! Mirrors pyfly's `on_message` exactly:
//!
//! * handler success, or no matching subscription → `basic_ack`;
//! * a matching handler returns `Err` → `basic_nack(requeue = true)` for
//!   redelivery;
//! * an undeserializable body → `basic_reject(requeue = false)` so a
//!   poison message is dropped instead of looping.
//!
//! ## Pattern subscription
//!
//! [`subscribe`](firefly_eda::Subscriber::subscribe) registers an
//! `fnmatch`-style pattern (`*`, `?`, `[...]`) tested against the
//! event's `type` — pyfly's `subscribe(event_type_pattern, handler)`.
//! Use [`pattern_matches`] directly to test the matcher.
//!
//! ## Quick start
//!
//! ```no_run
//! use firefly_eda::{handler, Event, Publisher, Subscriber};
//! use firefly_eda_rabbitmq::{RabbitMqBroker, RabbitMqBrokerConfig};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let broker = RabbitMqBroker::new(
//!     RabbitMqBrokerConfig::default()
//!         .with_url("amqp://guest:guest@localhost:5672/")
//!         .with_destinations(["orders"])
//!         .with_group("svc"),
//! );
//!
//! broker
//!     .subscribe(
//!         "order.*",
//!         handler(|ev: Event| async move {
//!             println!("got {}", ev.event_type);
//!             Ok(())
//!         }),
//!     )
//!     .await?;
//! broker.start().await?;
//!
//! let ev = Event::new("orders", "order.created", "orders-svc", Some(b"{}".to_vec()));
//! broker.publish(ev).await?;
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod broker;
mod config;
mod dispatch;

pub use broker::{new_rabbitmq_broker, RabbitMqBroker};
pub use config::{DeclarationPlan, ExchangeDeclaration, QueueDeclaration, RabbitMqBrokerConfig};
pub use dispatch::{dispatch, pattern_matches, Ack, Subscription};

/// Framework version stamp, shared across all Firefly crates.
pub const VERSION: &str = "26.6.8";
