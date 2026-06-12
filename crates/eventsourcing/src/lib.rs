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
mod projection;
mod snapshot;

pub use aggregate::{AggregateRoot, DomainEvent, EventStore, MemoryEventStore};
pub use error::EventSourcingError;
pub use projection::{Projection, ProjectionRunner};
pub use snapshot::{MemorySnapshotStore, Snapshot, SnapshotStore};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
