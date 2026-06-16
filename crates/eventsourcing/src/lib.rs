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

//! firefly-eventsourcing — event-sourced aggregate primitives for the
//! Firefly Framework.
//!
//! This crate ports the Go `eventsourcing` module (Java original:
//! `firefly-event-sourcing-spring-boot-starter`) and provides:
//!
//! * [`AggregateRoot`] — composed into domain aggregates; tracks
//!   uncommitted events and the loaded version.
//! * [`EventStore`] port — [`EventStore::append`] (with optimistic
//!   concurrency), [`EventStore::load`], [`EventStore::load_after`].
//!   Default [`MemoryEventStore`].
//! * [`SnapshotStore`] port — periodic state captures to bound rehydration
//!   cost. Default [`MemorySnapshotStore`].
//! * [`Projection`] + [`ProjectionRunner`] — read-side handlers with replay.
//!
//! ## pyfly parity additions
//!
//! * [`EventUpcaster`] — schema migration applied on the read paths
//!   ([`MemoryEventStore::with_upcasters`] / [`SqlEventStore::with_upcasters`]).
//! * [`TransactionalOutbox`] + [`OutboxRecord`] — at-least-once delivery of
//!   stored events to a broker via an [`OutboxSink`] (default [`EdaSink`]
//!   over `firefly-eda`).
//! * [`SqlEventStore`] — a SQL-backed [`EventStore`] over the
//!   `firefly-transactional` `Database` port.
//! * [`EventStore::stream_all`] — the global, cross-aggregate ordered event
//!   stream with a resumable cursor, driving read-model projections that span
//!   many aggregates (pyfly `EventStore.stream_all`). [`ProjectionRunner`]
//!   consumes it via [`ProjectionRunner::drive_once`] /
//!   [`ProjectionRunner::replay_all`], plus [`FunctionProjection`].
//! * Event-store multi-tenancy — an optional [`DomainEvent::tenant_id`]
//!   (persisted/filterable; omitted from JSON when `None` so the wire format
//!   is unchanged) threaded through append / load / `stream_all`
//!   (pyfly `StoredEventEnvelope.tenant_id`).
//! * [`EventSourcedRepository`] + [`EventSourcedAggregate`] — ties load
//!   (snapshot + replay) and save (append uncommitted + snapshot policy)
//!   together (pyfly `eventsourcing.repository.EventSourcedRepository`).
//!
//! The [`DomainEvent`] JSON wire format (camelCase field names, base64
//! payload, `metadata` omitted when empty) is byte-compatible with the
//! Java, .NET, Go and Python ports.
//!
//! # Quick start
//!
//! ```
//! use firefly_eventsourcing::{AggregateRoot, EventStore, MemoryEventStore};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let store = MemoryEventStore::new();
//!
//! let mut user = AggregateRoot::new("u1", "User");
//! user.raise("UserCreated", br#"{"name":"alice"}"#);
//! user.raise("UserRenamed", br#"{"name":"bob"}"#);
//!
//! // Err(EventSourcingError::Concurrency) means another writer raced.
//! let events = user.take_uncommitted();
//! store.append(&user.id, 0, events).await.unwrap();
//!
//! assert_eq!(store.load("u1").await.unwrap().len(), 2);
//! # });
//! ```

mod aggregate;
mod error;
mod outbox;
mod projection;
mod repository;
mod snapshot;
mod sql_store;
mod upcaster;

pub use aggregate::{AggregateRoot, DomainEvent, EventStore, MemoryEventStore, StreamedEvent};
pub use error::EventSourcingError;
pub use outbox::{EdaSink, OutboxRecord, OutboxSink, TransactionalOutbox};
pub use projection::{FunctionProjection, Projection, ProjectionRunner};
pub use repository::{EventSourcedAggregate, EventSourcedRepository};
pub use snapshot::{MemorySnapshotStore, Snapshot, SnapshotStore};
pub use sql_store::{parse_occurred_at, SqlEventStore, DDL};
pub use upcaster::{EventUpcaster, NoOpUpcaster};

/// Framework version stamp.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
