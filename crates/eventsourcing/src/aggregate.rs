//! Aggregate primitives: [`DomainEvent`], [`AggregateRoot`], the
//! [`EventStore`] port and its in-memory default.

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::EventSourcingError;

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
    uncommitted: Vec<DomainEvent>,
}

impl AggregateRoot {
    /// Returns a fresh root at version 0 with no uncommitted events.
    pub fn new(id: impl Into<String>, aggregate_type: impl Into<String>) -> Self {
        AggregateRoot {
            id: id.into(),
            aggregate_type: aggregate_type.into(),
            version: 0,
            uncommitted: Vec::new(),
        }
    }

    /// Records a state-changing event. The event is buffered until
    /// [`EventStore::append`] persists it.
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
}

/// MemoryEventStore is the in-process [`EventStore`] — suitable for tests
/// and for the default starter-domain wiring before a real DB is added.
#[derive(Debug, Default)]
pub struct MemoryEventStore {
    streams: RwLock<HashMap<String, Vec<DomainEvent>>>,
}

impl MemoryEventStore {
    /// Returns an empty store.
    pub fn new() -> Self {
        Self::default()
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
        let mut streams = self.streams.write().expect("event store lock poisoned");
        let current = streams.get(aggregate_id).map_or(0, |s| s.len() as i64);
        if expected_version != current {
            return Err(EventSourcingError::Concurrency);
        }
        streams
            .entry(aggregate_id.to_string())
            .or_default()
            .extend(events);
        Ok(())
    }

    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let streams = self.streams.read().expect("event store lock poisoned");
        match streams.get(aggregate_id) {
            Some(events) if !events.is_empty() => Ok(events.clone()),
            _ => Err(EventSourcingError::AggregateNotFound),
        }
    }

    async fn load_after(
        &self,
        aggregate_id: &str,
        since_version: i64,
    ) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let streams = self.streams.read().expect("event store lock poisoned");
        Ok(streams
            .get(aggregate_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.version > since_version)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }
}
