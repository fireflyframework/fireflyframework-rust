# `firefly-starter-domain`

> **Tier:** Starter · **Status:** Stable

## Overview

`firefly-starter-domain` composes
[`firefly-starter-core`](../starter-core/) with the in-memory
event-sourcing stores from [`firefly-eventsourcing`](../eventsourcing/):

* `events` — `Arc<dyn EventStore>` (default `MemoryEventStore`).
* `snapshots` — `Arc<dyn SnapshotStore>` (default
  `MemorySnapshotStore`).
* `projections` — `Arc<ProjectionRunner>`.

The canonical wiring for domain-tier services that source aggregates
from events. A Postgres-backed `EventStore` is on the roadmap; until
then, services that need persistent event storage register their own
implementation by overriding `domain.events` after `Domain::new(...)` —
the fields are public trait objects for exactly that reason.

`Domain` dereferences to `Core`, so every core field and convenience
method — `apply_middleware`, `actuator_router`, `new_application`,
`print_banner`, … — is available directly on the domain value.
`starter_name` defaults to `"starter-domain"`.

## Public surface

```rust,ignore
pub struct Domain {
    pub core: Core,                        // Deref/DerefMut target
    pub events: Arc<dyn EventStore>,       // default MemoryEventStore
    pub snapshots: Arc<dyn SnapshotStore>, // default MemorySnapshotStore
    pub projections: Arc<ProjectionRunner>,
}

impl Domain {
    pub fn new(cfg: CoreConfig) -> Self;
}
```

`Core`, `CoreConfig` and the event-sourcing types (`AggregateRoot`,
`DomainEvent`, `EventStore`, `MemoryEventStore`, `SnapshotStore`,
`MemorySnapshotStore`, `Snapshot`, `Projection`, `ProjectionRunner`,
`EventSourcingError`) are re-exported flat from this crate, so a
domain-tier service can depend on `firefly-starter-domain` alone.

## Quick start

```rust,ignore
use firefly_starter_domain::{AggregateRoot, CoreConfig, Domain};

#[tokio::main]
async fn main() {
    let domain = Domain::new(CoreConfig {
        app_name: "billing".into(),
        ..CoreConfig::default()
    });

    domain.projections.register(billing_projection);

    let mut invoice = AggregateRoot::new("i1", "Invoice");
    invoice.raise("InvoiceCreated", br#"{"amount":100}"#);
    let batch = invoice.take_uncommitted();
    domain.events.append(&invoice.id, 0, batch).await.unwrap();
    domain.projections.replay(&*domain.events, "i1").await.unwrap();
}
```

## Testing

```bash
cargo test -p firefly-starter-domain
```

The suite verifies that the event-sourcing dependencies are wired, the
starter name is `"starter-domain"`, and an event round-trips through
the wired store, with coverage for: custom starter names
pass through untouched while an explicit `"starter-core"` is renamed,
core defaults flow through, the wired stores keep their
optimistic-concurrency / `AggregateNotFound` / snapshot soft-miss
semantics, the README projection-replay flow, swapping `domain.events`
for a custom store after construction, two writers racing on a fresh
stream, `Deref`/`DerefMut` promotion of the core surface, and
`Send + Sync` bounds.
