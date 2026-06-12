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
