<!--
Copyright 2026 Firefly Software Foundation.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0
-->

# Event Sourcing

The [last chapter](./10-eda-messaging.md) left one question politely unasked.
Lumen's `Ledger` persists wallet events and the projection rebuilds the read
model by re-folding the stream — but *what stream?* So far the wallet's
canonical state has been implied. By the end of this chapter it is explicit and
load-bearing: the `Wallet` aggregate holds **no stored balance at all**. Its
balance is a pure function of an append-only stream of `WalletOpened`,
`MoneyDeposited`, and `MoneyWithdrawn` events, recomputed every time the
aggregate is loaded.

That is **event sourcing**: instead of storing current state and discarding each
change, you store the *sequence of changes* and derive state by replaying them. A
financial ledger is the ideal domain for it — accountants have known for
centuries that a ledger's authority comes from its entries, not from the running
total at the foot of the column. The total is a *derived fact*; the entries are
the *source of truth*. By the end of this chapter, an auditor asking "what was
wallet `wlt_…`'s balance after the third movement?" gets an answer Lumen can
*prove* from the stream, not merely report from a column.

`firefly-eventsourcing` provides the framework's **event-sourced aggregate**
primitives: an `AggregateRoot` that tracks uncommitted events, an `EventStore`
with optimistic concurrency, snapshots, projections, a global cross-aggregate
stream, a transactional outbox, and multi-tenancy. Where the
[EDA chapter](./10-eda-messaging.md)'s `Event` envelope was the *transport* for a
fact, the `DomainEvent` here is the *record* of it — the durable truth from which
state is rebuilt.

> **Design note.** `firefly-eventsourcing` provides the event-sourced aggregate
> primitives used here: `AggregateRoot::raise` to record an event, an `EventStore`
> with optimistic concurrency, and projections that build read models. A command
> `raise`s an event; the same `apply` fold runs on both the write path and replay —
> that symmetry is the correctness guarantee of event sourcing. The `DomainEvent`
> serializes to a stable, versioned, language-neutral JSON contract, so any service
> that honors it interoperates regardless of the language it is written in.

## State storage vs event storage

The clearest way to feel the shift is to compare what Lumen's storage *holds* in
each model.

In the **state-storage model**, the store keeps only the wallet's current state:

| id | owner | balance | version |
|----|-------|---------|---------|
| wlt_a1 | alice | 120 | 3 |

Every deposit and withdrawal overwrites `balance`. The history is gone — you know
the wallet holds 120 cents now; you cannot know how it got there.

In the **event-storage model**, the store keeps the stream:

| aggregate_id | version | event_type | payload |
|--------------|---------|------------|---------|
| wlt_a1 | 1 | WalletOpened | `{"wallet_id":"wlt_a1","owner":"alice","opening_balance":100}` |
| wlt_a1 | 2 | MoneyDeposited | `{"wallet_id":"wlt_a1","amount":50}` |
| wlt_a1 | 3 | MoneyWithdrawn | `{"wallet_id":"wlt_a1","amount":30}` |

The current balance is still 120 cents — but now you can read every decision that
led to it, replay to any version, and audit the lot. The trade-off is real: reads
cost a replay (mitigated by **snapshots**) and events are immutable (schema
evolution handled by **upcasters**). Both have first-class support below.

> **Note** — Event sourcing is not the same as the
> [previous chapter](./10-eda-messaging.md)'s EDA. There, the aggregate stored
> its state and *published* events as a side effect. Here the events *are* the
> state: there is no `balance` column to keep in sync — the balance is computed
> by folding the stream every time the aggregate loads.

## The mental model

<figure class="fig">
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 420 300" role="img"
     aria-label="The event-sourcing cycle: AggregateRoot::raise records an event into the uncommitted list, EventStore::append persists it with optimistic concurrency, and rehydrate/fold replays the stream into the current state"
     font-family="Avenir Next,Avenir,Helvetica Neue,Helvetica,Arial,sans-serif">
  <rect x="100" y="14" width="220" height="48" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="36" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">AggregateRoot::raise</text>
  <text x="210" y="53" text-anchor="middle" font-size="10.5" fill="#7a6450">record an event</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="62" x2="210" y2="84"/><polygon points="210,92 206,84 214,84"/>
  </g>
  <text x="210" y="80" text-anchor="middle" font-size="10.5" fill="#7a6450" font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">uncommitted []</text>
  <rect x="100" y="94" width="220" height="48" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="116" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">EventStore::append</text>
  <text x="210" y="133" text-anchor="middle" font-size="10.5" fill="#7a6450">optimistic concurrency</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="142" x2="210" y2="172"/><polygon points="210,180 206,172 214,172"/>
  </g>
  <rect x="100" y="182" width="220" height="48" rx="10" fill="#fdf6ea" stroke="#e0cda8" stroke-width="1.5"/>
  <text x="210" y="204" text-anchor="middle" font-size="13" font-weight="700" fill="#2a1d10"
        font-family="SF Mono,JetBrains Mono,Menlo,Consolas,monospace">rehydrate / fold</text>
  <text x="210" y="221" text-anchor="middle" font-size="10.5" fill="#7a6450">replay the stream</text>
  <g stroke="#d4793a" stroke-width="3" fill="#d4793a">
    <line x1="210" y1="230" x2="210" y2="252"/><polygon points="210,260 206,252 214,252"/>
  </g>
  <text x="210" y="282" text-anchor="middle" font-size="12.5" font-weight="600" fill="#3a2a1c">current state</text>
</svg>
<figcaption>The event-sourcing cycle. <code>raise</code> stages an event on the aggregate, <code>EventStore::append</code> persists the uncommitted events under optimistic concurrency, and a later load rehydrates the aggregate by folding its stream back into the current state.</figcaption>
</figure>

## The Wallet's domain events

In Lumen the three events are plain payload structs carrying
`#[derive(DomainEvent)]`. The derive stamps each with a stable `EVENT_TYPE`
discriminator (its struct name) and a `to_domain_event(...)` conversion onto the
framework wire event — so the event type is never spelled as a bare string
literal at the call sites:

```rust
use firefly::eventsourcing::DomainEvent;
use serde::{Deserialize, Serialize};

/// Payload of the event raised when a wallet is opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct WalletOpened {
    pub wallet_id: String,
    pub owner: String,
    /// The opening balance, in minor units (cents).
    pub opening_balance: i64,
}

/// Payload of the event raised when money is credited to a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyDeposited {
    pub wallet_id: String,
    pub amount: i64,
}

/// Payload of the event raised when money is debited from a wallet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DomainEvent)]
pub struct MoneyWithdrawn {
    pub wallet_id: String,
    pub amount: i64,
}
```

`#[derive(DomainEvent)]` generates, for each struct, a `pub const EVENT_TYPE:
&'static str` equal to the struct name (`"WalletOpened"`, …), an `event_type()`
accessor, and a `to_domain_event(aggregate_id, aggregate_type, version)` that
JSON-encodes the payload into a framework `DomainEvent`. That generated
`EVENT_TYPE` is the only thing the aggregate and its `apply` fold reference, so a
rename of the struct flows through automatically.

> **Note.** `#[derive(DomainEvent)]` generates, per struct, a `pub const
> EVENT_TYPE` equal to the struct name (the routing discriminator), an
> `event_type()` accessor, and a `to_domain_event(...)` that JSON-encodes the
> payload into a framework `DomainEvent` — so the event type is never a bare string
> literal at the call sites, and a struct rename flows through automatically. The
> generated JSON is a stable, versioned, language-neutral contract that any service
> honoring it can consume.

## The Wallet aggregate — raise, then apply

The `Wallet` carries `#[derive(AggregateRoot)]`, which finds the embedded
framework `AggregateRoot` field (`root`) and generates `Wallet::AGGREGATE_TYPE`
plus `aggregate()` / `aggregate_mut()` accessors. The projected state (`owner`,
`balance`, `opened`) is *not* stored — it is folded from the stream:

```rust
use firefly::eventsourcing::{AggregateRoot, DomainEvent};

use crate::money::Money;

#[derive(Debug, Clone, AggregateRoot)]
#[firefly(aggregate_type = "Wallet")]
pub struct Wallet {
    /// The framework aggregate root — uncommitted-event buffer + version.
    pub root: AggregateRoot,
    pub owner: String,
    /// Folded from the stream; never stored.
    pub balance: Money,
    /// Whether the wallet has been opened (an empty stream is "absent").
    pub opened: bool,
}
```

Every command follows the canonical event-sourcing shape: validate the
invariant, `raise` the matching event onto the embedded root, then apply it to
in-memory state. The write path and the replay path run the *same* `apply` code —
that symmetry is the correctness guarantee of event sourcing.

```rust,ignore
/// Credits `amount` to the wallet, raising a `MoneyDeposited` event.
pub fn deposit(&mut self, amount: Money) -> Result<(), DomainError> {
    self.require_opened()?;
    let amount = amount.require_positive()?;
    self.raise(
        MoneyDeposited::EVENT_TYPE,
        &MoneyDeposited {
            wallet_id: self.root.id.clone(),
            amount: amount.cents_value(),
        },
    );
    self.balance = self.balance.add(amount);
    Ok(())
}

/// Serialises a `#[derive(DomainEvent)]` payload and raises it onto the embedded
/// root under `event_type` — the discriminator from the generated `EVENT_TYPE`.
fn raise<P: Serialize>(&mut self, event_type: &str, payload: &P) {
    let bytes = serde_json::to_vec(payload).expect("domain event payload serialises");
    self.root.raise(event_type, bytes);
}
```

`AggregateRoot::raise` buffers the event (so the ledger can persist it) and bumps
the version. `withdraw` is the same shape, with one extra guard: it computes the
remaining balance *first* and lets `Money::subtract` reject an overdraw — so a
failed withdrawal raises **no** event at all, leaving the stream clean. That
overdraft guard is the trigger the transfer saga relies on in
[Sagas, Workflows & TCC](./12-sagas.md).

### Rehydration — folding the stream

Rehydration is the load path: rebuild a wallet by folding its full ordered stream
through the same `apply` the commands use. An empty stream yields an unopened
wallet — which is how the ledger distinguishes "absent" from "exists":

```rust,ignore
/// Rebuilds a wallet by folding `events` (its full ordered stream).
pub fn rehydrate(id: &str, events: &[DomainEvent]) -> Self {
    let mut wallet = Wallet {
        root: AggregateRoot::new(id, AGGREGATE_TYPE),
        owner: String::new(),
        balance: Money::ZERO,
        opened: false,
    };
    for event in events {
        wallet.apply(event);
        // Keep the root version in lock-step with the stream head so a
        // subsequent command appends at the right expected version.
        wallet.root.version = event.version;
    }
    wallet
}

/// Folds one persisted event into the projected state.
fn apply(&mut self, event: &DomainEvent) {
    match event.event_type.as_str() {
        WalletOpened::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<WalletOpened>(&event.payload) {
                self.owner = p.owner;
                self.balance = Money::cents(p.opening_balance);
                self.opened = true;
            }
        }
        MoneyDeposited::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyDeposited>(&event.payload) {
                self.balance = self.balance.add(Money::cents(p.amount));
            }
        }
        MoneyWithdrawn::EVENT_TYPE => {
            if let Ok(p) = serde_json::from_slice::<MoneyWithdrawn>(&event.payload) {
                self.balance = Money::cents(self.balance.cents_value() - p.amount);
            }
        }
        _ => {}
    }
}
```

Note that `apply` folds `MoneyWithdrawn` with a raw subtraction
(`Money::cents(self.balance.cents_value() - p.amount)`) rather than the
overdraft-guarded `Money::subtract` the `withdraw` command uses. That asymmetry is
deliberate: replay never re-validates. The guard already ran at *write* time and a
failed withdrawal raised no event, so every event in the stream is a fact that
already passed its invariant — replay simply applies it.

The folding logic in `apply` is matched on the *same* `EVENT_TYPE` constant the
commands raise under, so the two halves can never disagree about an event's name.
Lumen's unit tests prove the replay law directly: open + deposit + withdraw on a
*writer* wallet, take its uncommitted stream, then `Wallet::rehydrate` a fresh
wallet from that stream and assert the rebuilt balance, owner, and version match —
state recomputed from events, never stored.

> **Design note.** The discipline: a command `raise`s the event, `apply` mutates
> the projected fields, and load replays the same `apply` to rebuild state. Lumen
> registers no handler table — it `match`es on the generated `EVENT_TYPE` const, the
> Rust-idiomatic way to keep the write fold and the replay fold from ever
> disagreeing about an event's name.

## Raising and appending events

The framework `AggregateRoot` accumulates `DomainEvent`s as you `raise` them; you
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

The default is `MemoryEventStore` — the in-process store Lumen runs on, ideal for
development and tests. `SqlEventStore` backs it with a SQL store over the
`firefly-transactional` `Database` port for production; swapping it is a one-line
change to the `event_store` `#[bean]` in `LumenBeans`, exactly like swapping the
broker in the last chapter.

## The Ledger ties it together

Lumen's `Ledger` is the application service that owns the store (and the broker
from the [last chapter](./10-eda-messaging.md)). Every command rehydrates, runs
the domain method, and commits with optimistic concurrency. Here is `deposit` and
the load path — the version the wallet rehydrated to *is* the `expected_version`
the append must match:

```rust,ignore
/// Credits `amount` to `wallet_id`, persisting + publishing `MoneyDeposited`.
pub async fn deposit(&self, wallet_id: &str, amount: Money) -> Result<WalletView, DomainError> {
    let mut wallet = self.load(wallet_id).await?;
    let expected = wallet.root.version;
    wallet.deposit(amount)?;
    self.commit(&mut wallet, expected).await?;
    Ok(wallet.view())
}

/// Rehydrates the aggregate from its persisted stream.
async fn load(&self, wallet_id: &str) -> Result<Wallet, DomainError> {
    let events = self.load_events(wallet_id).await?;
    Ok(Wallet::rehydrate(wallet_id, &events))
}

/// Loads the full event stream, mapping an absent aggregate to a domain 404.
pub async fn load_events(&self, wallet_id: &str) -> Result<Vec<DomainEvent>, DomainError> {
    match self.store.load(wallet_id).await {
        Ok(events) => Ok(events),
        Err(EventSourcingError::AggregateNotFound) => {
            Err(DomainError::NotFound(wallet_id.to_string()))
        }
        Err(e) => Err(DomainError::NotFound(format!("{wallet_id}: {e}"))),
    }
}
```

The `commit` method (shown in full in the [last chapter](./10-eda-messaging.md))
appends at `expected` then publishes each event. The two chapters meet here: this
one supplies the durable, replayable store; that one carries each appended event
onto the wire so the projection can react.

### Optimistic concurrency in practice

Two concurrent requests — say a deposit from the app and a fee withdrawal from a
job — can both load wallet `wlt_a1` at version 3, each apply a change, and each
try to append at `expected_version = 3`. The first append wins and the stream
advances to 4; the second now mismatches and the store returns
`EventSourcingError::Concurrency`. Lumen maps that to a `DomainError::NotFound`
detail ("concurrent modification") so the caller retries from a fresh load. You
never manage version numbers by hand — the version the wallet rehydrated to is
the token, and the store enforces it.

> **Note.** `append(id, expected_version, events)` enforces optimistic
> concurrency: the version the wallet rehydrated to is the token, and a stale
> append fails with `EventSourcingError::Concurrency`. Catch it and retry the
> load-mutate-save cycle (or surface a 409) — never swallow it.

## Typed aggregates and the repository

Lumen folds the stream by hand in `Wallet::apply` because it teaches the
mechanic clearly. For larger aggregates the framework offers a thinner path:
implement `EventSourcedAggregate` — a typed `apply_event` plus optional snapshot
serialization — and let `EventSourcedRepository` tie `load` (snapshot + replay)
and `save` (append + snapshot policy) together:

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

## Snapshots — when streams get long

Event sourcing trades write simplicity for read cost: a wallet with 10,000
movements replays 10,000 events every load. **Snapshots** cut that down. A
snapshot is a serialized checkpoint of the aggregate's state at a version; on
load, the repository deserializes the latest snapshot and replays only the events
after it. A snapshot at version 9,000 turns a 10,000-event replay into 1,000.

Lumen's wallets are short-lived enough that the in-memory store's full replay is
fine, so the sample does not wire snapshots — but the seam is there:
`with_snapshots(store, MemorySnapshotStore::new(), 100)` would checkpoint every
time a wallet's stream crosses a 100-event boundary. Snapshots are an
optimization, never a correctness requirement: remove them and the system is
slower but still correct.

> **Note.** `with_snapshots(store, MemorySnapshotStore::new(), interval)`
> checkpoints aggregate state every time a stream crosses an interval boundary; on
> load, the repository deserializes the latest snapshot and replays only the events
> after it. The interval-crossing trigger handles a batch that straddles the
> threshold (version 95 → 105 still snapshots). Snapshots are an optimization, never
> a correctness requirement.

## Projections — building read models

A `Projection` is a read-side handler. Register projections on a
`ProjectionRunner` and replay an aggregate's events through them. This is the
event-*store* sibling of the [last chapter](./10-eda-messaging.md)'s event-*bus*
listener: Lumen's `project_wallet_event` reacts to events as they are published,
whereas a `ProjectionRunner` can replay history from the beginning to rebuild a
read model from scratch:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{FunctionProjection, ProjectionRunner};

let runner = ProjectionRunner::new();
runner.register(Arc::new(FunctionProjection::new("balances", |event| async move {
    // update a read-model row from the event ...
    Ok(())
})));

runner.replay(&store, "wlt_a1").await?;  // replay one aggregate's stream
```

This rebuildability is unique to event sourcing. If Lumen's read model is ever
lost or its schema changes, you stop the projector, clear the read model, and
replay every stream — the history is right there in the store. A state-storage
model cannot do this; it discarded the history at write time.

## The global stream

`EventStore::stream_all` exposes the global, cross-aggregate, ordered event
stream with a resumable cursor — the engine for read models that span many
aggregates (think: "all movements across all wallets, in order"). The runner
consumes it in batches, at-least-once and in-order:

```rust,ignore
// Drive one batch; returns the next cursor + any per-event error.
let (next_cursor, err) = runner
    .drive_once(&store, None, 100, None)
    .await?;

// Or replay the whole global stream from a start cursor.
let cursor = runner.replay_all(&store, None, 100, None).await?;
```

## The transactional outbox

The [last chapter](./10-eda-messaging.md) noted a gap in `Ledger::commit`: it
appends, then publishes, and a crash *between* the two persists the fact but drops
the broadcast. `TransactionalOutbox` closes that gap. Instead of publishing
directly, a writer `enqueue`s the `DomainEvent`; a background relay polls and
forwards each pending record to an `OutboxSink`, retrying up to `max_attempts`.
The default `EdaSink` bridges each `DomainEvent` to a `firefly_eda::Event` and
publishes it — the same `to_envelope`-shaped bridge, but durable:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EdaSink, TransactionalOutbox};

let outbox = TransactionalOutbox::new(Arc::new(EdaSink::new(
    broker,           // the Arc<dyn firefly_eda::Publisher>
    "wallet.events",  // destination topic
    "lumen",          // logical source stamped onto every Event::source
)))
.with_max_attempts(5);

outbox.enqueue(some_event).await;       // a writer enqueues
outbox.start().await;                   // background relay forwards + retries
// ... later
let dead = outbox.dead_letters().await; // exhausted records, for inspection
outbox.stop().await;
```

Exhausted records become dead letters — excluded from the publish loop and
surfaced for inspection or manual retry. This is the upgrade path for a
production Lumen: enqueue into the outbox inside the same store transaction as the
append, and let the relay guarantee at-least-once delivery to the broker even
across crashes — which is exactly why the projection was built to be
**idempotent** in the last chapter.

> **Design note.** `TransactionalOutbox` closes the append-then-publish gap:
> instead of publishing directly, a writer `enqueue`s the `DomainEvent` in the same
> store transaction as the append, and a background relay forwards each pending
> record to an `OutboxSink`, retrying up to `max_attempts` and dead-lettering on
> exhaustion. Recording the event durably *before* dispatching it is what gives
> at-least-once delivery across crashes — and why the projection was built to be
> idempotent.

## Schema evolution — upcasters

`EventUpcaster` migrates events on the **read** paths only, so consumers always
observe current-schema events while the stored history stays untouched. Suppose
Lumen later needs a `reference` field on every deposit for reconciliation: new
events carry it, old `MoneyDeposited` events do not, and an upcaster fills the gap
on load:

```rust,ignore
use std::sync::Arc;
use firefly_eventsourcing::{EventUpcaster, MemoryEventStore};

let store = MemoryEventStore::with_upcasters(vec![Arc::new(MyUpcaster)]);
// every event returned by load / load_after passes through applicable upcasters
```

Old data becomes readable without a migration; new data is written in the current
schema. The events themselves stay immutable — you never rewrite history.

## Multi-tenancy

An optional `DomainEvent::tenant_id` (stamped from
`AggregateRoot::with_tenant`, persisted and filterable, omitted from JSON when
`None`) is threaded through `append` / `load` / `stream_all`, so one store serves
many tenants with per-tenant isolation on the global stream — the route a
multi-bank Lumen deployment would take to keep each tenant's wallet streams
separate.

## Recap — what changed in Lumen

The wallet's balance is no longer a stored value — it is a *computation* over an
immutable stream, and the stream is the system of record.

| Piece | Role |
|-------|------|
| `#[derive(DomainEvent)]` | Generates `EVENT_TYPE` + `to_domain_event(...)` for each payload struct |
| `#[derive(AggregateRoot)]` | Generates `AGGREGATE_TYPE` + `aggregate()`/`aggregate_mut()` over the embedded `root` |
| `Wallet::raise` / `apply` | Command applies the event; the same fold runs on write and on replay |
| `Wallet::rehydrate` | Rebuilds a wallet by folding its full stream — empty stream = unopened |
| `EventStore` / `MemoryEventStore` | The append-only log; `SqlEventStore` for production |
| `append(id, expected_version, …)` | Optimistic concurrency — the rehydrated version is the token |
| `ProjectionRunner` | Rebuilds read models from history (the store-side sibling of the EDA listener) |
| `TransactionalOutbox` | Closes the append-then-publish gap with at-least-once relay |

Three ideas carry forward. **The events are the truth** — there is no balance
column to drift. **Write and replay share one fold** — `apply` runs the same way
whether a command just raised the event or a load is rebuilding from history,
which is the correctness guarantee. **Depend on the `EventStore` port** — the
in-memory store becomes SQL with a one-line swap, just as the broker became Kafka.

When a business process spans multiple aggregates and needs compensation — moving
money from one wallet to another, atomically — folding a single stream is no
longer enough. Continue to [Sagas, Workflows & TCC](./12-sagas.md), where the
transfer saga drives two wallets and rolls the debit back when the credit fails.

## Exercises

1. **Replay to a point in time.** Open a wallet and make three deposits. Load the
   raw stream with `ledger.load_events(&id)`, take only the events with
   `version <= 2`, and `Wallet::rehydrate` a fresh wallet from that slice. Assert
   the balance equals opening + first deposit only — the "time-travel query" a
   state-storage model cannot answer.

2. **Prove the overdraft guard raises no event.** Open a wallet with 100 cents,
   attempt to `withdraw` 101, and assert it errors with
   `DomainError::InsufficientFunds`. Then call `root.uncommitted()` and assert the
   buffer still holds exactly one event (the `WalletOpened`) — the failed command
   left the stream clean.

3. **Force an optimistic-concurrency conflict.** Append the open event for a
   wallet at `expected_version = 0`. Then, without reloading, raise a second
   event and append it *also* at `expected_version = 0`. Assert the second append
   returns `EventSourcingError::Concurrency`, and explain why a fresh load (which
   advances `expected` to 1) would have succeeded.

4. **Add a `ProjectionRunner` rebuild.** Register a `FunctionProjection` that
   tallies the count of `MoneyDeposited` events per wallet into an in-memory map,
   `replay` one wallet's stream through it, and assert the count. Then clear the
   map and replay again — confirming the read model is rebuildable from the store
   alone, with no live event traffic.
