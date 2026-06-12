# `firefly-eventsourcing`

> **Tier:** Platform · **Status:** Full · **Java original:** `firefly-event-sourcing-spring-boot-starter` · **Go module:** `eventsourcing`

## Overview

`firefly-eventsourcing` provides the framework's **event-sourced aggregate**
primitives:

* `AggregateRoot` — composed into domain aggregates (the Rust analog of
  Go's struct embedding); tracks uncommitted events and the loaded version.
* `EventStore` port — `append` (with optimistic concurrency), `load`,
  `load_after`. Default `MemoryEventStore`.
* `SnapshotStore` port — periodic state captures to bound rehydration
  cost. Default `MemorySnapshotStore`.
* `Projection` + `ProjectionRunner` — read-side handlers with replay.

The `DomainEvent` JSON wire format — camelCase field names, base64-encoded
`payload` (matching Go's `[]byte` encoding), `metadata` omitted when empty —
is byte-compatible with the Java, .NET, Go and Python ports. A
Postgres-backed `EventStore` with the canonical `firefly_events` table
schema is on the roadmap.

## Mental model

```
            ┌────────────────────────┐
            │  AggregateRoot::raise  │
            └────────────────────────┘
                       │
                  uncommitted []
                       │
            ┌──────────┴──────────┐
            │  EventStore::append │  optimistic concurrency
            └──────────┬──────────┘
                       │
              ┌────────┴───────────┐
              │  ProjectionRunner   │ → read models
              └─────────────────────┘
```

## Public surface

```rust,ignore
pub struct DomainEvent {
    pub aggregate_id: String,        // JSON: "aggregateId"
    pub aggregate_type: String,      // JSON: "aggregateType"
    pub version: i64,
    pub event_type: String,          // JSON: "type"
    pub time: DateTime<Utc>,
    pub payload: Vec<u8>,            // JSON: base64 string
    pub metadata: BTreeMap<String, serde_json::Value>, // omitted when empty
}

pub struct AggregateRoot {
    pub id: String,
    pub aggregate_type: String,
    pub version: i64,
    // private uncommitted: Vec<DomainEvent>
}
impl AggregateRoot {
    pub fn new(id, aggregate_type) -> Self;
    pub fn raise(&mut self, event_type, payload);
    pub fn uncommitted(&self) -> &[DomainEvent];
    pub fn take_uncommitted(&mut self) -> Vec<DomainEvent>; // drain + clear
    pub fn clear(&mut self);
}

#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, aggregate_id, expected_version, events) -> Result<(), EventSourcingError>; // Concurrency on mismatch
    async fn load(&self, aggregate_id) -> Result<Vec<DomainEvent>, EventSourcingError>;               // AggregateNotFound on empty
    async fn load_after(&self, aggregate_id, since_version) -> Result<Vec<DomainEvent>, EventSourcingError>;
}

#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn latest(&self, aggregate_id) -> Result<Option<Snapshot>, EventSourcingError>; // Ok(None) is a soft miss
    async fn save(&self, snapshot: Snapshot) -> Result<(), EventSourcingError>;
}

#[async_trait]
pub trait Projection: Send + Sync {
    fn name(&self) -> &str;
    async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError>;
}
pub struct ProjectionRunner { /* ... */ }
impl ProjectionRunner {
    pub fn new() -> Self;
    pub fn register(&self, projection: Arc<dyn Projection>);
    pub async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError>;
    pub async fn replay(&self, store: &dyn EventStore, aggregate_id: &str) -> Result<(), EventSourcingError>;
}

pub enum EventSourcingError {
    Concurrency,        // "firefly/eventsourcing: concurrency conflict"
    AggregateNotFound,  // "firefly/eventsourcing: aggregate not found"
    Projection(String),
}
```

## Quick start

```rust
use firefly_eventsourcing::{AggregateRoot, EventStore, MemoryEventStore};

#[tokio::main]
async fn main() {
    let store = MemoryEventStore::new();

    let mut user = AggregateRoot::new("u1", "User");
    user.raise("UserCreated", br#"{"name":"alice"}"#);
    user.raise("UserRenamed", br#"{"name":"bob"}"#);

    let events = user.take_uncommitted();
    if let Err(err) = store.append(&user.id, 0, events).await {
        // EventSourcingError::Concurrency means another writer raced.
        eprintln!("append failed: {err}");
    }

    // Rebuild a read model.
    // let runner = firefly_eventsourcing::ProjectionRunner::new();
    // runner.register(my_projection);
    // runner.replay(&store, "u1").await.unwrap();
    assert_eq!(store.load("u1").await.unwrap().len(), 2);
}
```

## Testing

```bash
cargo test -p firefly-eventsourcing
```

Covers the raise → uncommitted → clear lifecycle, optimistic-concurrency
rejection (stale `expected_version`), `load` returning
`EventSourcingError::AggregateNotFound`, projection replay and
short-circuiting, snapshot soft-miss semantics, concurrent-append races,
and Go-compatible JSON wire formats (base64 payloads, sorted metadata
keys, RFC 3339 timestamps).
