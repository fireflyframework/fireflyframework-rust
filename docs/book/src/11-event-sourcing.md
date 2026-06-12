# Event Sourcing

`firefly-eventsourcing` provides the framework's **event-sourced aggregate**
primitives: an `AggregateRoot` that tracks uncommitted events, an `EventStore`
with optimistic concurrency, snapshots, projections, a global cross-aggregate
stream, a transactional outbox, and multi-tenancy. Where the
[DDD chapter](./08-domain-driven-design.md)'s events were transient (collected
for post-commit publication), event-sourced events are the *source of truth* —
state is rebuilt by replaying them.

> **Spring parity** — This is the `firefly-event-sourcing` starter / Axon-style
> model: `AggregateRoot.raise`, an `EventStore` with optimistic concurrency, and
> projections that build read models. The `DomainEvent` JSON is wire-compatible
> across every port.

## The mental model

```text
            ┌────────────────────────┐
            │  AggregateRoot::raise  │  record an event
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

## Raising and appending events

An `AggregateRoot` accumulates `DomainEvent`s as you `raise` them, then you
`take_uncommitted` and `append` them to the store. `append` enforces optimistic
concurrency: pass the version you loaded, and a concurrent writer's append fails
with `EventSourcingError::Concurrency`:

```rust
use firefly_eventsourcing::{AggregateRoot, EventStore, MemoryEventStore};

#[tokio::main]
async fn main() {
    let store = MemoryEventStore::new();

    let mut user = AggregateRoot::new("u1", "User");
    user.raise("UserCreated", br#"{"name":"alice"}"#);
    user.raise("UserRenamed", br#"{"name":"bob"}"#);

    let events = user.take_uncommitted();
    // expected_version 0 -> this is a brand-new aggregate.
    if let Err(err) = store.append(&user.id, 0, events).await {
        eprintln!("append failed (raced): {err}");
    }

    assert_eq!(store.load("u1").await.unwrap().len(), 2);
}
```

The `EventStore` port:

```rust,ignore
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn append(&self, aggregate_id: &str, expected_version: i64,
                    events: Vec<DomainEvent>) -> Result<(), EventSourcingError>;
    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn load_after(&self, aggregate_id: &str, since_version: i64)
        -> Result<Vec<DomainEvent>, EventSourcingError>;
    async fn stream_all(&self, after_event_id: Option<&str>, limit: usize, tenant: Option<&str>)
        -> Result<Vec<StreamedEvent>, EventSourcingError>;
}
```

The default is `MemoryEventStore`; `SqlEventStore` backs it with a SQL store over
the `firefly-transactional` `Database` port for production.

## Typed aggregates and the repository

For real aggregates, implement `EventSourcedAggregate` — a typed `apply_event`
that mutates state per event, plus optional snapshot serialization — and let
`EventSourcedRepository` tie `load` (snapshot + replay) and `save` (append +
snapshot policy) together:

```rust,ignore
use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcedAggregate, EventSourcedRepository,
    EventSourcingError, MemoryEventStore,
};
use std::sync::Arc;

#[derive(Default)]
struct Wallet { root: AggregateRoot, balance: i64 }

impl EventSourcedAggregate for Wallet {
    const AGGREGATE_TYPE: &'static str = "Wallet";
    fn root(&self) -> &AggregateRoot { &self.root }
    fn root_mut(&mut self) -> &mut AggregateRoot { &mut self.root }
    fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        if event.event_type == "Credited" {
            let amount: i64 = serde_json::from_slice(&event.payload)
                .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
            self.balance += amount;
        }
        Ok(())
    }
}

# async fn ex() -> Result<(), EventSourcingError> {
let repo = EventSourcedRepository::<Wallet>::new(Arc::new(MemoryEventStore::new()));

let mut w = Wallet::default();
w.root_mut().raise("Credited", b"500");
repo.save(&mut w).await?;                     // append uncommitted

let reloaded = repo.load(&w.root.id).await?;  // snapshot + replay
assert!(reloaded.is_some());
# Ok(())
# }
```

`EventSourcedRepository::with_snapshots(store, snapshots, interval)` enables
periodic state captures so rehydration does not replay the entire history.

## Projections — building read models

A `Projection` is a read-side handler. Register projections on a
`ProjectionRunner` and replay an aggregate's events through them:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{FunctionProjection, ProjectionRunner};

let runner = ProjectionRunner::new();
runner.register(Arc::new(FunctionProjection::new("balances", |event| async move {
    // update a read-model row from the event ...
    Ok(())
})));

runner.replay(&store, "u1").await?;  // replay one aggregate's stream
```

## The global stream

`EventStore::stream_all` exposes the global, cross-aggregate, ordered event
stream with a resumable cursor — the engine for read models that span many
aggregates. The runner consumes it in batches, at-least-once and in-order:

```rust,ignore
// Drive one batch; returns the next cursor + any per-event error.
let (next_cursor, err) = runner
    .drive_once(&store, None, 100, None)
    .await?;

// Or replay the whole global stream from a start cursor.
let cursor = runner.replay_all(&store, None, 100, None).await?;
```

## The transactional outbox

`TransactionalOutbox` gives at-least-once delivery of stored events to a broker.
A writer `enqueue`s a `DomainEvent`; a background relay polls and forwards each
pending record to an `OutboxSink`, retrying up to `max_attempts`. The default
`EdaSink` bridges each `DomainEvent` to a `firefly_eda::Event` and publishes it:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EdaSink, TransactionalOutbox};

let outbox = TransactionalOutbox::new(Arc::new(EdaSink::new(broker)))
    .with_max_attempts(5);

outbox.enqueue(some_event).await;       // a writer enqueues
outbox.start().await;                   // background relay forwards + retries
// ... later
let dead = outbox.dead_letters().await; // exhausted records, for inspection
outbox.stop().await;
```

Exhausted records become dead letters — excluded from the publish loop and
surfaced for inspection or manual retry.

## Schema evolution — upcasters

`EventUpcaster` migrates events on the **read** paths only, so consumers always
observe current-schema events while the stored history stays untouched:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EventUpcaster, MemoryEventStore};

let store = MemoryEventStore::with_upcasters(vec![Arc::new(MyUpcaster)]);
// every event returned by load / load_after passes through applicable upcasters
```

## Multi-tenancy

An optional `DomainEvent::tenant_id` (stamped from
`AggregateRoot::with_tenant`, persisted and filterable, omitted from JSON when
`None`) is threaded through `append` / `load` / `stream_all`, so one store
serves many tenants with per-tenant isolation on the global stream.

When a business process spans multiple aggregates or services and needs
compensation, reach for the orchestration engines. Continue to
[Sagas, Workflows & TCC](./12-sagas.md).
