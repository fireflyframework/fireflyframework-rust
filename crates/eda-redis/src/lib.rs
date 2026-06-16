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

//! # firefly-eda-redis
//!
//! A Redis Streams transport for the Firefly [`firefly_eda`] event-driven
//! architecture port — the Rust port of pyfly's `RedisStreamsEventBus`.
//!
//! [`RedisStreamsBroker`] implements [`firefly_eda::Publisher`] and
//! [`firefly_eda::Subscriber`] (and therefore [`firefly_eda::Broker`])
//! over the [`redis`] crate's async multiplexed connection:
//!
//! - **subscribe** registers a glob topic pattern + handler;
//! - **publish** `XADD`s `{envelope: <json>}` to the stream named by the
//!   event's `topic`;
//! - **start** issues `XGROUP CREATE … MKSTREAM` per configured stream
//!   (tolerating `BUSYGROUP`) and spawns an `XREADGROUP … BLOCK` consume
//!   loop;
//! - the loop dispatches each entry to matching handlers, `XACK`s on
//!   success, and **leaves the entry pending on handler error** so Redis
//!   redelivers it — at-least-once, exactly as pyfly does by skipping the
//!   `XACK`.
//!
//! The on-stream record uses the field name `envelope` carrying the
//! [`firefly_eda::Event`] JSON, byte-for-byte compatible with the pyfly
//! Redis adapter's `{b"envelope": …}` records.
//!
//! ## Quick start
//!
//! ```no_run
//! use firefly_eda::{handler, Event};
//! use firefly_eda_redis::{new_redis_broker, RedisConfig, RedisStreamsBroker};
//!
//! # async fn run() -> firefly_eda::EdaResult<()> {
//! let broker = RedisStreamsBroker::connect(
//!     RedisConfig::new("redis://localhost:6379/0").with_group("orders-svc"),
//! )?;
//!
//! broker
//!     .subscribe(
//!         "orders.*",
//!         handler(|ev: Event| async move {
//!             println!("got {}", ev.event_type);
//!             Ok(())
//!         }),
//!     )
//!     .await?;
//! broker.start().await?;
//!
//! broker
//!     .publish(Event::new("orders.created", "OrderCreated", "orders-svc", None))
//!     .await?;
//! # firefly_eda::Publisher::close(&broker).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Topic dispatch
//!
//! pyfly matches handler patterns against the envelope's `event_type`;
//! this port matches against the envelope's `topic`, consistent with the
//! [`firefly_eda::Subscriber`] contract shared by every Firefly transport
//! (including [`firefly_eda::InMemoryBroker`]). Set the event `topic` to
//! the value you would have matched on in pyfly.

#![warn(missing_docs)]

mod broker;
mod config;

pub use broker::{new_redis_broker, RedisStreamsBroker};
pub use config::RedisConfig;

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
