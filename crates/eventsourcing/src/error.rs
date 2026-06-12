//! Error taxonomy for the event-sourcing module.

use thiserror::Error;

/// Errors returned by the event-sourcing ports.
///
/// The `Display` strings of [`EventSourcingError::Concurrency`] and
/// [`EventSourcingError::AggregateNotFound`] are byte-for-byte identical to
/// the Go port's sentinel errors (`ErrConcurrency`, `ErrAggregateNotFound`)
/// so operators and log scrapers see the same text on every platform.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventSourcingError {
    /// Returned by [`EventStore::append`](crate::EventStore::append) when the
    /// expected version doesn't match the current head — the
    /// optimistic-concurrency signal that another writer raced.
    #[error("firefly/eventsourcing: concurrency conflict")]
    Concurrency,

    /// Returned by [`EventStore::load`](crate::EventStore::load) when no
    /// events exist for the requested aggregate id.
    #[error("firefly/eventsourcing: aggregate not found")]
    AggregateNotFound,

    /// A read-side [`Projection`](crate::Projection) failed to apply an
    /// event. Go projections return arbitrary `error` values; the Rust port
    /// carries the failure message in this variant.
    #[error("firefly/eventsourcing: projection error: {0}")]
    Projection(String),
}
