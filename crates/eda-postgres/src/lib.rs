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

//! # firefly-eda-postgres
//!
//! A Postgres-backed [`Broker`](firefly_eda::Broker) — a durable
//! **transactional outbox** plus `LISTEN`/`NOTIFY` wake-ups — ported
//! from pyfly's `pyfly.eda.adapters.postgres.PostgresEventBus`.
//!
//! Every published event is appended to a `firefly_eda_outbox` table
//! (monotonic `BIGSERIAL` id) and a `pg_notify` fires on a shared
//! channel. Each consumer **group** keeps a cursor row in
//! `firefly_eda_offsets` so subscribers survive restarts and catch up
//! on events they missed.
//!
//! ## Delivery semantics
//!
//! * **At-least-once.** The cursor (`firefly_eda_offsets.last_event_id`)
//!   is advanced only **after** a handler returns successfully. A crash
//!   mid-dispatch re-delivers from the last committed id.
//! * **Single drainer per group.** The drain loop is gated by
//!   `pg_try_advisory_lock` on a SHA-256-folded `i64` key derived from
//!   the consumer-group name, so two replicas sharing a `group` never
//!   double-advance the cursor. The session-level lock auto-releases on
//!   connection close, so a crashed worker never zombies the lock.
//! * **Poll fallback.** The drain loop also wakes on a fixed
//!   `poll_interval` so events that arrived while a listener was
//!   reconnecting are never stuck.
//!
//! ## Wire / DDL compatibility
//!
//! The DDL is ported from pyfly verbatim except for the table-name
//! prefix (`firefly_eda_*` instead of pyfly's `pyfly_eda_*`), matching
//! this crate's framework naming. The outbox columns
//! (`destination`, `event_type`, `payload`, `headers`, `created_at`)
//! are identical, so a pyfly producer and a Rust consumer (or vice
//! versa) interoperate as long as both point at the same table name.
//!
//! ## Pattern subscription
//!
//! pyfly's `subscribe(event_type_pattern, handler)` dispatches on an
//! `fnmatch` glob over the event's *type*. The Rust [`Subscriber`]
//! trait keys on `topic`; this adapter treats the `topic` argument as
//! that glob pattern, matched against each outbox row's `event_type`.
//! Use [`PostgresBroker::subscribe_pattern`] for the explicit spelling.
//!
//! ## Quick start
//!
//! ```no_run
//! use firefly_eda::{handler, Event, Publisher, Subscriber};
//! use firefly_eda_postgres::{PostgresBroker, PostgresConfig};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let broker = PostgresBroker::new(
//!     PostgresConfig::new("host=db user=app dbname=app")
//!         .destinations(["orders.created"])
//!         .group("orders-workers"),
//! );
//! broker.start().await?;
//! broker
//!     .subscribe(
//!         "OrderCreated", // event-type glob, e.g. "Order*"
//!         handler(|ev: Event| async move {
//!             println!("got {}", ev.event_type);
//!             Ok(())
//!         }),
//!     )
//!     .await?;
//! let ev = Event::new("orders.created", "OrderCreated", "orders-svc", None);
//! broker.publish(ev).await?;
//! # Ok(()) }
//! ```

#![warn(missing_docs)]

mod broker;
mod sql;

pub use broker::{PostgresBroker, PostgresConfig};
pub use sql::{group_lock_key, normalise_dsn, quote_ident, IdentError};

/// The released framework version, shared across all Firefly crates.
pub const VERSION: &str = "26.6.3";
