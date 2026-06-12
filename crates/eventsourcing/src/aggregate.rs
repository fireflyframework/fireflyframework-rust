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

//! Aggregate primitives: [`DomainEvent`], [`AggregateRoot`], the
//! [`EventStore`] port and its in-memory default.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::EventSourcingError;
use crate::upcaster::{apply_upcasters, EventUpcaster};

/// Serializes binary payloads as standard (padded) base64 strings — the JSON
/// encoding Go gives `[]byte` — so events written by any port can be read by
/// every other.
pub(crate) mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Encodes `bytes` as a standard base64 JSON string.
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    /// Decodes a base64 JSON string; Go encodes a nil `[]byte` as JSON
    /// `null`, which is accepted and treated as an empty payload.
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(Vec::new()),
            Some(s) => STANDARD.decode(s).map_err(serde::de::Error::custom),
        }
    }
}

/// DomainEvent is the wire-shape of every persisted event. Wire-compatible
/// with the Java `EventEnvelope`, the .NET `DomainEvent` and the Go
/// `DomainEvent`: field names and order, the base64 payload encoding and the
/// `metadata` omission rule all match exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomainEvent {
    /// Identifier of the aggregate the event belongs to.
    #[serde(rename = "aggregateId")]
    pub aggregate_id: String,
    /// Aggregate type discriminator (e.g. `"User"`).
    #[serde(rename = "aggregateType")]
    pub aggregate_type: String,
    /// 1-based, monotonically increasing position within the stream.
    pub version: i64,
    /// Event type discriminator (e.g. `"UserCreated"`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// UTC instant the event was raised.
    pub time: DateTime<Utc>,
    /// Opaque event body, serialized as standard base64 like Go's `[]byte`.
    #[serde(with = "base64_bytes")]
    pub payload: Vec<u8>,
    /// Contextual metadata; omitted from JSON when empty (matching Go's
    /// `omitempty`). A `BTreeMap` keeps key order deterministic, matching
    /// Go's sorted map-key encoding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Optional tenant identifier for multi-tenant event stores — mirrors
    /// pyfly's `StoredEventEnvelope.tenant_id`. Omitted from JSON when `None`
    /// (the default), so an event raised without a tenant serialises
    /// byte-for-byte identically to the Go/Java/.NET wire format. When set, it
    /// is persisted as a filterable column by [`SqlEventStore`] and threaded
    /// through [`EventStore::append`] / [`EventStore::load`] /
    /// [`EventStore::stream_all`] for tenant scoping.
    ///
    /// [`SqlEventStore`]: crate::SqlEventStore
    #[serde(rename = "tenantId", default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// AggregateRoot is held (composed) by domain aggregates — the Rust analog
/// of Go's struct embedding. It tracks the in-memory uncommitted events and
/// the loaded version. Aggregates apply state transitions inside their
/// command methods and append the event via [`AggregateRoot::raise`].
#[derive(Debug, Clone, Default)]
pub struct AggregateRoot {
    /// Aggregate identifier.
    pub id: String,
    /// Aggregate type discriminator stamped onto every raised event.
    pub aggregate_type: String,
    /// Version of the last event raised or loaded; starts at 0.
    pub version: i64,
    /// Optional tenant identifier stamped onto every raised event when set —
    /// the per-aggregate counterpart of [`DomainEvent::tenant_id`]. Use
    /// [`AggregateRoot::with_tenant`] to set it at construction.
    pub tenant_id: Option<String>,
    uncommitted: Vec<DomainEvent>,
}

impl AggregateRoot {
    /// Returns a fresh root at version 0 with no uncommitted events.
    pub fn new(id: impl Into<String>, aggregate_type: impl Into<String>) -> Self {
        AggregateRoot {
            id: id.into(),
            aggregate_type: aggregate_type.into(),
            version: 0,
            tenant_id: None,
            uncommitted: Vec::new(),
        }
    }

    /// Sets the tenant id stamped onto every subsequently raised event — the
    /// builder form of [`AggregateRoot::tenant_id`]. Mirrors pyfly threading a
    /// `tenant_id` through the repository onto each `StoredEventEnvelope`.
    #[must_use]
    pub fn with_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Records a state-changing event. The event is buffered until
    /// [`EventStore::append`] persists it. The aggregate's
    /// [`tenant_id`](AggregateRoot::tenant_id) (if any) is stamped onto the
    /// event.
    pub fn raise(&mut self, event_type: impl Into<String>, payload: impl Into<Vec<u8>>) {
        self.version += 1;
        self.uncommitted.push(DomainEvent {
            aggregate_id: self.id.clone(),
            aggregate_type: self.aggregate_type.clone(),
            version: self.version,
            event_type: event_type.into(),
            time: Utc::now(),
            payload: payload.into(),
            metadata: BTreeMap::new(),
            tenant_id: self.tenant_id.clone(),
        });
    }

    /// Returns the buffered events. Callers should pass these to
    /// [`EventStore::append`] then [`clear`](AggregateRoot::clear) them —
    /// or use [`take_uncommitted`](AggregateRoot::take_uncommitted) to do
    /// both in one move.
    pub fn uncommitted(&self) -> &[DomainEvent] {
        &self.uncommitted
    }

    /// Drains and returns the buffered events, leaving the buffer empty —
    /// a Rust-idiomatic fusion of `Uncommitted()` + `Clear()` that hands
    /// ownership straight to [`EventStore::append`].
    pub fn take_uncommitted(&mut self) -> Vec<DomainEvent> {
        std::mem::take(&mut self.uncommitted)
    }

    /// Empties the uncommitted buffer.
    pub fn clear(&mut self) {
        self.uncommitted.clear();
    }
}

/// One event read off the global, cross-aggregate ordered stream.
///
/// Ports pyfly's `StoredEventEnvelope` view as seen by `stream_all`: it pairs
/// the [`DomainEvent`] with a stable, store-assigned `event_id` cursor key so
/// a [`ProjectionRunner`](crate::ProjectionRunner) can resume from where it
/// left off ([`EventStore::stream_all`]). The [`DomainEvent`] itself keeps its
/// pinned Go-parity wire format; the cursor key lives only on this wrapper.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamedEvent {
    /// Stable, monotonic-within-store cursor key — pass as `after_event_id`
    /// to [`EventStore::stream_all`] to resume after this event.
    pub event_id: String,
    /// The stored domain event (Go-parity wire shape preserved).
    pub event: DomainEvent,
}

impl StreamedEvent {
    /// The event's tenant id, if any (sugar over `self.event.tenant_id`).
    pub fn tenant_id(&self) -> Option<&str> {
        self.event.tenant_id.as_deref()
    }
}

/// EventStore is the persistence boundary for event-sourced aggregates.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Persists events for `aggregate_id` after the expected version.
    /// Returns [`EventSourcingError::Concurrency`] if `expected_version` no
    /// longer matches the head.
    async fn append(
        &self,
        aggregate_id: &str,
        expected_version: i64,
        events: Vec<DomainEvent>,
    ) -> Result<(), EventSourcingError>;

    /// Returns every event for `aggregate_id`, ordered by version ascending.
    /// Returns [`EventSourcingError::AggregateNotFound`] when the stream is
    /// empty or absent.
    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError>;

    /// Returns events whose version is greater than `since_version`.
    async fn load_after(
        &self,
        aggregate_id: &str,
        since_version: i64,
    ) -> Result<Vec<DomainEvent>, EventSourcingError>;

    /// Returns a page of the global, cross-aggregate ordered event stream —
    /// the Rust analog of pyfly's `EventStore.stream_all`.
    ///
    /// Events are returned in global append order. `after_event_id` is a
    /// cursor: pass `None` to start from the very beginning, or the
    /// [`StreamedEvent::event_id`] of the last consumed event to resume after
    /// it (cursor-style, at-least-once, in-order). At most `limit` events are
    /// returned. When `tenant` is `Some`, only events with a matching
    /// [`DomainEvent::tenant_id`] are returned.
    ///
    /// The default implementation returns an empty page — a store that has no
    /// global log opts out without breaking the contract. [`MemoryEventStore`]
    /// and [`SqlEventStore`](crate::SqlEventStore) override it.
    async fn stream_all(
        &self,
        after_event_id: Option<&str>,
        limit: usize,
        tenant: Option<&str>,
    ) -> Result<Vec<StreamedEvent>, EventSourcingError> {
        let _ = (after_event_id, limit, tenant);
        Ok(Vec::new())
    }
}

/// MemoryEventStore is the in-process [`EventStore`] — suitable for tests
/// and for the default starter-domain wiring before a real DB is added.
///
/// An optional [`EventUpcaster`] chain (see
/// [`MemoryEventStore::with_upcasters`]) is applied on the read paths
/// ([`load`](EventStore::load) / [`load_after`](EventStore::load_after)) so
/// consumers always observe current-schema events — matching pyfly's
/// `InMemoryEventStore`. The write path ([`append`](EventStore::append)) is
/// never touched.
#[derive(Default)]
pub struct MemoryEventStore {
    inner: RwLock<MemoryState>,
    upcasters: Vec<Arc<dyn EventUpcaster>>,
}

/// The mutable interior of [`MemoryEventStore`]: the per-aggregate streams
/// plus a single global, append-ordered log that backs
/// [`stream_all`](EventStore::stream_all). Each global entry carries a stable,
/// monotonic `event_id` used as the cursor key, mirroring pyfly's
/// `InMemoryEventStore._all` list keyed by `StoredEventEnvelope.event_id`.
#[derive(Default)]
struct MemoryState {
    streams: HashMap<String, Vec<DomainEvent>>,
    all: Vec<(String, DomainEvent)>,
    next_seq: u64,
}

impl std::fmt::Debug for MemoryEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryEventStore")
            .field("upcasters", &self.upcasters.len())
            .finish_non_exhaustive()
    }
}

impl MemoryEventStore {
    /// Returns an empty store with no upcasters configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns an empty store whose read paths apply `upcasters` (in order)
    /// to every event before returning it — the Rust analog of pyfly's
    /// `InMemoryEventStore(upcasters=[...])`.
    pub fn with_upcasters(upcasters: Vec<Arc<dyn EventUpcaster>>) -> Self {
        MemoryEventStore {
            inner: RwLock::new(MemoryState::default()),
            upcasters,
        }
    }
}

#[async_trait]
impl EventStore for MemoryEventStore {
    async fn append(
        &self,
        aggregate_id: &str,
        expected_version: i64,
        events: Vec<DomainEvent>,
    ) -> Result<(), EventSourcingError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.write().expect("event store lock poisoned");
        let current = inner
            .streams
            .get(aggregate_id)
            .map_or(0, |s| s.len() as i64);
        if expected_version != current {
            return Err(EventSourcingError::Concurrency);
        }
        for event in events {
            // Stamp a stable, monotonic global cursor key and mirror the event
            // into the global log so stream_all observes it in append order.
            let event_id = format!("{:020}", inner.next_seq);
            inner.next_seq += 1;
            inner
                .streams
                .entry(aggregate_id.to_string())
                .or_default()
                .push(event.clone());
            inner.all.push((event_id, event));
        }
        Ok(())
    }

    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let inner = self.inner.read().expect("event store lock poisoned");
        match inner.streams.get(aggregate_id) {
            Some(events) if !events.is_empty() => Ok(events
                .iter()
                .cloned()
                .map(|e| apply_upcasters(e, &self.upcasters))
                .collect()),
            _ => Err(EventSourcingError::AggregateNotFound),
        }
    }

    async fn load_after(
        &self,
        aggregate_id: &str,
        since_version: i64,
    ) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let inner = self.inner.read().expect("event store lock poisoned");
        Ok(inner
            .streams
            .get(aggregate_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.version > since_version)
                    .cloned()
                    .map(|e| apply_upcasters(e, &self.upcasters))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn stream_all(
        &self,
        after_event_id: Option<&str>,
        limit: usize,
        tenant: Option<&str>,
    ) -> Result<Vec<StreamedEvent>, EventSourcingError> {
        let inner = self.inner.read().expect("event store lock poisoned");
        // Find the start index: just past `after_event_id`, or 0 from the
        // beginning. An unknown cursor yields an empty page (the event it
        // pointed at is gone), matching pyfly's "scan until matched, else
        // empty" behaviour.
        let start = match after_event_id {
            None => 0,
            Some(cursor) => match inner.all.iter().position(|(id, _)| id == cursor) {
                Some(idx) => idx + 1,
                None => return Ok(Vec::new()),
            },
        };
        let mut out = Vec::new();
        for (event_id, event) in inner.all[start..].iter() {
            if let Some(t) = tenant {
                if event.tenant_id.as_deref() != Some(t) {
                    continue;
                }
            }
            out.push(StreamedEvent {
                event_id: event_id.clone(),
                event: apply_upcasters(event.clone(), &self.upcasters),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}
