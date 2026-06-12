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

//! Generic event-sourced repository: load (snapshot + replay) and save
//! (append uncommitted + snapshot policy) tied together.
//!
//! Ports pyfly's `eventsourcing.repository.EventSourcedRepository`. The Rust
//! [`AggregateRoot`] is *composed* into a domain aggregate (the analog of Go
//! struct embedding) rather than subclassed, so the repository is generic over
//! an [`EventSourcedAggregate`] trait that exposes the embedded root and the
//! fold/snapshot hooks. The repository orchestrates the glue every event-
//! sourced service otherwise hand-writes:
//!
//! * [`load`](EventSourcedRepository::load) — restore the latest snapshot
//!   (if a [`SnapshotStore`] is configured), then replay only the events
//!   *after* the snapshot version onto the aggregate.
//! * [`save`](EventSourcedRepository::save) — append the aggregate's
//!   uncommitted events with optimistic concurrency, then write a fresh
//!   snapshot when the batch **crossed** a multiple of `snapshot_interval`
//!   (exact-divisibility checks miss a batch that straddles the threshold —
//!   pyfly audit #151).

use std::marker::PhantomData;
use std::sync::Arc;

use crate::aggregate::{AggregateRoot, DomainEvent, EventStore};
use crate::error::EventSourcingError;
use crate::snapshot::{Snapshot, SnapshotStore};

/// A domain aggregate that can be reconstructed from, and persisted to, an
/// [`EventStore`] by an [`EventSourcedRepository`].
///
/// Implementors compose an [`AggregateRoot`] (tracking id / version /
/// uncommitted events) and supply the read-side fold ([`apply_event`]) plus
/// optional snapshot (de)hydration. This mirrors pyfly's `AggregateRoot`
/// subclass contract (`when`/`apply`/`replay` + snapshot payload), adapted to
/// Rust composition.
///
/// [`apply_event`]: EventSourcedAggregate::apply_event
///
/// # Example
///
/// ```
/// use firefly_eventsourcing::{AggregateRoot, EventSourcedAggregate, DomainEvent, EventSourcingError};
///
/// #[derive(Default)]
/// struct Order {
///     root: AggregateRoot,
///     amount: i64,
///     shipped: bool,
/// }
///
/// impl EventSourcedAggregate for Order {
///     const AGGREGATE_TYPE: &'static str = "Order";
///     fn root(&self) -> &AggregateRoot { &self.root }
///     fn root_mut(&mut self) -> &mut AggregateRoot { &mut self.root }
///     fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError> {
///         match event.event_type.as_str() {
///             "OrderPlaced" => {
///                 let v: serde_json::Value = serde_json::from_slice(&event.payload)
///                     .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
///                 self.amount = v["amount"].as_i64().unwrap_or(0);
///             }
///             "OrderShipped" => self.shipped = true,
///             other => return Err(EventSourcingError::Projection(format!("no handler for {other}"))),
///         }
///         Ok(())
///     }
/// }
/// ```
pub trait EventSourcedAggregate: Default + Send + Sync {
    /// The aggregate type discriminator stamped onto stored events and
    /// snapshots, and checked on replay. The Rust analog of pyfly's
    /// `type(aggregate).__name__`.
    const AGGREGATE_TYPE: &'static str;

    /// Borrows the embedded [`AggregateRoot`].
    fn root(&self) -> &AggregateRoot;

    /// Mutably borrows the embedded [`AggregateRoot`].
    fn root_mut(&mut self) -> &mut AggregateRoot;

    /// Folds one (already-persisted) event into the aggregate's state. Called
    /// for every event replayed on [`load`](EventSourcedRepository::load). An
    /// event with no handler should return [`EventSourcingError::Projection`]
    /// so reconstruction fails loudly rather than silently corrupting state
    /// (pyfly audit #146).
    fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError>;

    /// Serialises the aggregate's state for a snapshot. Default: empty — a
    /// repository with no [`SnapshotStore`] never calls this, and one with a
    /// store but no override snapshots an empty payload (still bounding
    /// replay by the snapshot's version). Override to persist real state.
    fn snapshot_payload(&self) -> Result<Vec<u8>, EventSourcingError> {
        Ok(Vec::new())
    }

    /// Restores aggregate state from a snapshot payload produced by
    /// [`snapshot_payload`](EventSourcedAggregate::snapshot_payload). Default:
    /// no-op. Override alongside `snapshot_payload`.
    fn restore_snapshot(&mut self, payload: &[u8]) -> Result<(), EventSourcingError> {
        let _ = payload;
        Ok(())
    }
}

/// Reconstructs aggregates from an [`EventStore`] and persists their pending
/// events, applying a snapshot policy. Ports pyfly's `EventSourcedRepository`.
///
/// Construct with [`EventSourcedRepository::new`] (no snapshots) or
/// [`with_snapshots`](EventSourcedRepository::with_snapshots).
pub struct EventSourcedRepository<A: EventSourcedAggregate> {
    store: Arc<dyn EventStore>,
    snapshots: Option<Arc<dyn SnapshotStore>>,
    snapshot_interval: i64,
    _aggregate: PhantomData<fn() -> A>,
}

impl<A: EventSourcedAggregate> std::fmt::Debug for EventSourcedRepository<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSourcedRepository")
            .field("aggregate_type", &A::AGGREGATE_TYPE)
            .field("snapshots", &self.snapshots.is_some())
            .field("snapshot_interval", &self.snapshot_interval)
            .finish()
    }
}

impl<A: EventSourcedAggregate> EventSourcedRepository<A> {
    /// Builds a repository over `store` with no snapshotting.
    pub fn new(store: Arc<dyn EventStore>) -> Self {
        EventSourcedRepository {
            store,
            snapshots: None,
            snapshot_interval: 100,
            _aggregate: PhantomData,
        }
    }

    /// Builds a repository that captures a snapshot every `snapshot_interval`
    /// events (pyfly default: 100). An interval `<= 0` is clamped to 1.
    pub fn with_snapshots(
        store: Arc<dyn EventStore>,
        snapshots: Arc<dyn SnapshotStore>,
        snapshot_interval: i64,
    ) -> Self {
        EventSourcedRepository {
            store,
            snapshots: Some(snapshots),
            snapshot_interval: snapshot_interval.max(1),
            _aggregate: PhantomData,
        }
    }

    /// Reconstructs the aggregate `aggregate_id`, or `Ok(None)` if it has no
    /// snapshot and no events (it was never persisted).
    ///
    /// When a [`SnapshotStore`] is configured, the latest snapshot is restored
    /// first and only events *after* its version are replayed — bounding
    /// rehydration cost. Replayed events are checked to belong to this
    /// aggregate id / type; a mismatch is a store bug and surfaces as
    /// [`EventSourcingError::Projection`] (pyfly audit #150).
    pub async fn load(&self, aggregate_id: &str) -> Result<Option<A>, EventSourcingError> {
        let mut aggregate = A::default();
        {
            let root = aggregate.root_mut();
            root.id = aggregate_id.to_string();
            root.aggregate_type = A::AGGREGATE_TYPE.to_string();
        }

        let mut starting_version = 0;
        if let Some(snapshots) = &self.snapshots {
            if let Some(snap) = snapshots.latest(aggregate_id).await? {
                aggregate.restore_snapshot(&snap.payload)?;
                aggregate.root_mut().version = snap.version;
                starting_version = snap.version;
            }
        }

        let events = self
            .store
            .load_after(aggregate_id, starting_version)
            .await?;
        if events.is_empty() && starting_version == 0 {
            return Ok(None);
        }
        for event in &events {
            if event.aggregate_id != aggregate_id {
                return Err(EventSourcingError::Projection(format!(
                    "firefly/eventsourcing: replayed event aggregate_id {:?} != loaded aggregate {aggregate_id:?}",
                    event.aggregate_id
                )));
            }
            if !event.aggregate_type.is_empty() && event.aggregate_type != A::AGGREGATE_TYPE {
                return Err(EventSourcingError::Projection(format!(
                    "firefly/eventsourcing: replayed event aggregate_type {:?} != {:?}",
                    event.aggregate_type,
                    A::AGGREGATE_TYPE
                )));
            }
            aggregate.apply_event(event)?;
            aggregate.root_mut().version = event.version;
        }
        Ok(Some(aggregate))
    }

    /// Persists the aggregate's uncommitted events with optimistic
    /// concurrency, clears them, and writes a snapshot when the batch crossed
    /// a `snapshot_interval` boundary.
    ///
    /// A no-op when the aggregate has no uncommitted events.
    pub async fn save(&self, aggregate: &mut A) -> Result<(), EventSourcingError> {
        let pending = aggregate.root().uncommitted().len() as i64;
        if pending == 0 {
            return Ok(());
        }
        let aggregate_id = aggregate.root().id.clone();
        let new_version = aggregate.root().version;
        let expected = new_version - pending;
        let events = aggregate.root_mut().take_uncommitted();
        self.store.append(&aggregate_id, expected, events).await?;

        // Snapshot when the batch CROSSED a multiple of the interval — exact
        // divisibility ('version % interval == 0') is missed when a batch
        // straddles the threshold (pyfly audit #151).
        if let Some(snapshots) = &self.snapshots {
            let crossed =
                (new_version / self.snapshot_interval) > (expected / self.snapshot_interval);
            if crossed {
                let payload = aggregate.snapshot_payload()?;
                snapshots
                    .save(Snapshot {
                        aggregate_id,
                        aggregate_type: A::AGGREGATE_TYPE.to_string(),
                        version: new_version,
                        payload,
                    })
                    .await?;
            }
        }
        Ok(())
    }
}
