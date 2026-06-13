<!--
Copyright 2026 Firefly Software Foundation.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0
-->

# Event-Driven Architecture & Messaging

By the end of the [CQRS chapter](./09-cqrs.md), Lumen could open a wallet,
deposit, withdraw, and read a balance — but the command side and the query side
were quietly cheating. The `Wallet` aggregate raised crisp domain events
(`WalletOpened`, `MoneyDeposited`, `MoneyWithdrawn`), the `Ledger` persisted
them, and then nothing carried them anywhere. The read model the `GetWallet`
query serves had to be repaired on the fly by re-folding the event stream.

By the end of *this* chapter, Lumen closes the loop. Every event the ledger
persists is also **published** to a `Broker`, and a read-model **projection** —
declared with one `#[event_listener]` attribute — consumes those events and
keeps the query side current without the write side knowing it exists. That is
event-driven architecture: a fact is published once, and any number of
independent reactions subscribe to it. The audit trail, the welcome
notification, the balance read model — each becomes a subscriber you can add
months later without touching a single command handler.

`firefly-eda` is the framework's **event-driven architecture port**. It defines
the `Event` envelope every Firefly event flows through, the
`Publisher`/`Subscriber`/`Broker` ports, an in-process `InMemoryBroker`, and the
messaging machinery — glob topics, consumer groups, retry/DLQ, event filters,
and a reactive `Flux` subscription surface. The production transports (Kafka,
RabbitMQ, Postgres outbox, Redis Streams) implement the same ports and slot in
at wiring time, so Lumen's projection never changes when the broker does.

> **Spring parity** — The `Broker` port is the Spring Cloud Stream binder
> abstraction; publishing to it is `ApplicationEventPublisher.publishEvent` /
> `StreamBridge.send`. `#[event_listener(topic = …)]` is `@KafkaListener` /
> `@EventListener`. `wrap_listener`'s retry/DLQ is `@RetryableTopic`; the glob
> subscription is `bus.subscribe("user.*", …)`.

## Two kinds of "event" in one wallet

Before wiring anything, it is worth being precise about the word *event*,
because Lumen ends up using it for two different things and confusing them leads
to the wrong port.

A **domain event** in the event-sourcing sense — `firefly::eventsourcing`'s
`DomainEvent` — is the durable, versioned fact the `Wallet` aggregate raises and
the [next chapter](./11-event-sourcing.md) makes the source of truth. It lives
in the event *store*.

A **messaging event** — `firefly::eda`'s `Event` — is the wire envelope that
carries a fact *to subscribers*. It lives on the *broker*. Lumen bridges the two
with one function (`to_envelope`, below): the ledger persists a `DomainEvent`,
then maps it onto an `Event` and publishes it. This chapter is about the second
kind — getting the fact onto the wire and reacting to it. The first kind is the
next chapter's subject.

## The `Event` envelope

`Event` is wire-compatible across the Java/.NET/Go/Python/Rust ports — the same
JSON field names and omission rules. Construct one with `Event::new`, which also
stamps `correlation_id` from the kernel's task-local correlation scope:

```rust
use firefly_eda::Event;

let ev = Event::new(
    "orders.created",   // topic
    "OrderCreated",     // event type
    "orders-svc",       // source
    Some(br#"{"id":"o1"}"#.to_vec()), // payload (base64 on the wire)
)
.with_header("x-tenant", "acme")
.with_key(b"customer-42".to_vec()); // partition / routing key
```

The `key` is what Kafka uses for partitioning and RabbitMQ for routing; it is
omitted from the wire when absent, so older events stay byte-for-byte identical.

### Lumen's domain-event-to-envelope bridge

Lumen never builds an `Event` by hand in a handler. The ledger owns one mapping
function that turns a persisted `DomainEvent` into the canonical envelope,
carrying the JSON-encoded domain event as the payload and the wallet id as the
partition key so a real broker keeps per-wallet events ordered:

```rust
use firefly::eda::Event;
use firefly::eventsourcing::DomainEvent;

/// The EDA topic every wallet domain event is published to.
pub const EVENTS_TOPIC: &str = "wallets.events";
/// The logical EDA source stamped on published events.
pub const EVENT_SOURCE: &str = "lumen";

/// Maps a persisted `DomainEvent` onto the canonical EDA `Event` envelope.
pub fn to_envelope(event: &DomainEvent) -> Event {
    let payload = serde_json::to_vec(event).expect("domain event serialises");
    Event::new(
        EVENTS_TOPIC,
        event.event_type.clone(),
        EVENT_SOURCE,
        Some(payload),
    )
    .with_key(event.aggregate_id.clone().into_bytes())
    .with_header("aggregateType", "Wallet")
    .with_header("aggregateId", event.aggregate_id.clone())
    .with_header("version", event.version.to_string())
}
```

Three design choices repay attention. The **topic** (`wallets.events`) is a
shared constant — the publisher and the projection key off the same value, so
the channel name can never drift. The **key** is the wallet id, so on a
partitioned broker every event for one wallet lands on the same partition and
stays in order. The **headers** (`aggregateId`, `version`) carry just enough
routing metadata for a subscriber to find and re-fold the affected aggregate
without decoding the payload — which is exactly what Lumen's projection does
below.

> **Spring parity** — `to_envelope` is the hand-written equivalent of a Spring
> Cloud Stream message converter: the domain object becomes the payload, and the
> `KafkaHeaders.MESSAGE_KEY` / custom headers carry the routing metadata.

## Publishing from the ledger

The `Ledger` is the single write path every command and the transfer saga call.
After it appends an aggregate's uncommitted events to the store with optimistic
concurrency, it publishes each one — `to_envelope` then `broker.publish` — so
the projection downstream can react:

```rust
use std::sync::Arc;
use firefly::eda::Broker;
use firefly::eventsourcing::{EventSourcingError, EventStore};

use crate::domain::{DomainError, Wallet};

/// Appends the aggregate's uncommitted events at `expected_version`
/// (optimistic concurrency) then publishes each to the EDA broker.
async fn commit(&self, wallet: &mut Wallet, expected: i64) -> Result<(), DomainError> {
    let events = wallet.take_uncommitted();
    if events.is_empty() {
        return Ok(());
    }
    self.store
        .append(&wallet.root.id, expected, events.clone())
        .await
        .map_err(|e| match e {
            EventSourcingError::Concurrency => {
                DomainError::NotFound(format!("{}: concurrent modification", wallet.root.id))
            }
            other => DomainError::NotFound(format!("{}: {other}", wallet.root.id)),
        })?;
    for event in &events {
        self.broker
            .publish(to_envelope(event))
            .await
            .map_err(|e| DomainError::NotFound(format!("publish failed: {e}")))?;
    }
    Ok(())
}
```

Notice the ordering: **append before publish.** A subscriber must never see a
fact that did not persist — the same "save before you publish" discipline a
Spring service follows with `@TransactionalEventListener(phase = AFTER_COMMIT)`.
If the append fails (including the optimistic-concurrency race) the loop is never
reached, so no event is broadcast. The store backing this `Ledger` is the
in-memory `MemoryEventStore`; the [next chapter](./11-event-sourcing.md) is where
that store earns the name *event-sourced*.

> **Spring parity** — Publishing after the unit of work commits mirrors Spring's
> `@TransactionalEventListener(phase = AFTER_COMMIT)`. The gap between append and
> publish — where a crash could persist a fact but drop the broadcast — is what
> the transactional outbox in the next chapter eliminates.

## The in-process broker

`InMemoryBroker` is the default — fan-out delivery, glob topic matching, and
per-`(topic, group)` round-robin, with no external dependency. It is the broker
Lumen's `WebStack` exposes as `web.broker`, and it is everything the teaching
build (and the test suite) needs. Subscribe a handler, publish an event:

```rust
use firefly_eda::{handler, Event, InMemoryBroker};

#[tokio::main]
async fn main() {
    let broker = InMemoryBroker::new();

    broker
        .subscribe(
            "wallets.events",
            handler(|ev: Event| async move {
                println!("observed {} for {}", ev.event_type,
                    ev.headers.get("aggregateId").map(String::as_str).unwrap_or("?"));
                Ok(())
            }),
        )
        .unwrap();

    let ev = Event::new("wallets.events", "WalletOpened", "lumen",
        Some(br#"{"wallet_id":"wlt_1"}"#.to_vec()));
    broker.publish(ev).await.unwrap();
    broker.close().unwrap();
}
```

`InMemoryBroker::publish` awaits each subscribed handler sequentially on the
publisher's task; the first handler error short-circuits and is returned to the
publisher. After `close()`, publish and subscribe fail with `EdaError::Closed`.

## The read-model projection — `#[event_listener]`

Here is where Lumen closes the CQRS loop. The **projection** is a free `async fn`
carrying one attribute. The `#[event_listener(topic = "wallets.events")]` macro
generates a `subscribe_project_wallet_event(broker)` helper that subscribes the
function to the topic; for each delivered event it reloads the affected wallet's
stream, folds it into a `WalletView`, and upserts it into the read model:

```rust
use firefly::eda::Event;
use firefly::prelude::*;

use crate::domain::Wallet;

/// The read-model projection. `#[event_listener]` generates
/// `subscribe_project_wallet_event(broker)`, which subscribes this fn to
/// `EVENTS_TOPIC`. For each event it reloads the wallet's stream, folds it into
/// a `WalletView`, and upserts it — the idempotent rebuild-from-stream pattern.
#[event_listener(topic = "wallets.events")]
pub async fn project_wallet_event(ev: Event) -> FireflyResult<()> {
    let Some(state) = projection_state() else {
        return Ok(());
    };
    let Some(wallet_id) = ev.headers.get("aggregateId") else {
        return Ok(());
    };
    // A transient store miss is swallowed so one poison message never stalls
    // the projection — the EDA at-least-once contract.
    if let Ok(events) = state.store.load(wallet_id).await {
        let view = Wallet::rehydrate(wallet_id, &events).view();
        state.read_model.upsert(view);
    }
    Ok(())
}
```

Two properties make this a *good* projection rather than just a working one.

It is **idempotent.** Rather than mutating the read-model row from the single
delivered event (`balance += amount`), it reloads the wallet's full stream and
rebuilds the view from scratch. Under EDA's at-least-once delivery a redelivered
`MoneyDeposited` would double-count if you applied the delta — but re-folding the
same stream converges on the same `WalletView` no matter how many times the
event arrives. The header carries the `aggregateId`; that is all the projection
needs to find the stream.

It is **decoupled.** `project_wallet_event` imports no command, calls no handler,
and has no idea a deposit was processed. It reacts purely to the published fact.
You can add a `FraudDetector` or a `WelcomeNotifier` subscriber next to it
without touching a line of the command path.

> **Spring parity** — `#[event_listener(topic = "wallets.events")]` →
> `subscribe_project_wallet_event(broker)` is the macro analog of Spring's
> `@KafkaListener(topics = "wallets.events")` and pyfly's
> `@event_listener(event_types=[...])`: the framework wires the subscription for
> you; you write only the reaction.

### Wiring state into a free-fn listener

Spring would inject the store and read model into a `@Component` listener bean. A
Rust free fn cannot capture wiring state, so Lumen uses the same
publish-collaborators-once pattern its CQRS handlers use: the resolved
collaborators are placed in a process-global `OnceLock` at startup, and the
listener reads them back through a small accessor:

```rust
use std::sync::{Arc, OnceLock};
use firefly::eventsourcing::EventStore;

use crate::ledger::ReadModel;

/// The collaborators the free-fn projection needs.
struct ProjectionState {
    store: Arc<dyn EventStore>,
    read_model: Arc<ReadModel>,
}

static PROJECTION: OnceLock<ProjectionState> = OnceLock::new();

/// Publishes the projection's collaborators and returns the *effective* state
/// (the first call wins, so repeated builds in one test binary share one state).
pub fn bind_projection(
    store: Arc<dyn EventStore>,
    read_model: Arc<ReadModel>,
) -> (Arc<dyn EventStore>, Arc<ReadModel>) {
    let effective = PROJECTION.get_or_init(|| ProjectionState { store, read_model });
    (
        Arc::clone(&effective.store),
        Arc::clone(&effective.read_model),
    )
}

fn projection_state() -> Option<&'static ProjectionState> {
    PROJECTION.get()
}
```

The composition root ties it all together: it binds the projection's
collaborators to the *same* store and read model the command handlers use, then
awaits the generated subscription so the events the handlers publish are exactly
the events the projection consumes:

```rust,ignore
// in build_app() — crate::web
ledger::bind_projection(Arc::clone(ledger.store()), Arc::clone(&read_model));
crate::commands::register(&bus);
// Subscribe the projection to the effective ledger's broker.
ledger::subscribe_project_wallet_event(ledger.broker().as_ref())
    .await
    .expect("projection subscription");
```

With that one `await`, every `POST /api/v1/wallets/:id/deposit` flows
command → ledger → store → broker → projection → read model, and the next
`GET /api/v1/wallets/:id` is served from the projected view. The HTTP test suite
proves the loop converges: it deposits, withdraws, then reads back the balance
and `version` the projection folded — no manual repair needed.

## Glob topics and consumer groups

A subscription topic is a glob pattern (`*`, `?`, `[..]`, `{a,b}`); a published
event is delivered to every subscription whose pattern matches its topic. Lumen
subscribes to the exact `wallets.events`, but a multi-event service could fan a
single listener across a family:

```rust,ignore
broker.subscribe("wallets.*", handler(|ev| async move { Ok(()) })).unwrap();
// matches wallets.events, wallets.audit, ...
```

Consumer groups give competing-consumer delivery: within a group each matching
event goes to exactly **one** member (round-robin); distinct groups each get
their own copy:

```rust,ignore
broker.subscribe_group("wallets.events", "projections", handler1).unwrap();
broker.subscribe_group("wallets.events", "projections", handler2).unwrap();
// each event reaches exactly one of handler1/handler2
```

This is how you would scale Lumen's projection horizontally: run several
projector instances in one group and the broker shares the partitions among
them, each instance owning a slice of the wallet space.

## Retry and dead-letter

`wrap_listener(handler, publisher, policy)` is the adapter-agnostic retry/DLQ
wrapper. A failing delivery is retried up to `retries` times with linear backoff
(`retry_delay * attempt`); on exhaustion the event is republished to the
dead-letter topic (when set), carrying the original payload/key/headers plus
`x-original-topic` and `x-exception` diagnostic headers:

```rust
use std::sync::Arc;
use std::time::Duration;
use firefly_eda::{handler, wrap_listener, InMemoryBroker, ListenerPolicy};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let inner = handler(|_ev| async { Err(firefly_kernel::FireflyError::internal("boom")) });
let wrapped = wrap_listener(
    inner,
    broker.clone(),
    ListenerPolicy::with_retries(3)
        .retry_delay(Duration::from_millis(50))
        .dead_letter_topic("wallets.events.DLT"),
);
broker.subscribe("wallets.events", wrapped).unwrap();
# });
```

Lumen's projection takes a gentler path — it *swallows* a transient store miss
and returns `Ok(())` rather than failing the delivery, so a single poison message
never stalls the stream. That is the right call for a rebuild-from-stream
projection (the next redelivery, or the next event for the wallet, converges
anyway). A side-effecting listener — one that sends an email or calls an external
API — is where `wrap_listener` and a dead-letter topic earn their keep.

For an inspectable record of failures (rather than a routing topic), wire an
`EdaDeadLetterStore` via `ListenerPolicy::dead_letter_store`: an exhausted event
is captured into the store (queryable with `list` / `get` / `remove`).

## Event filters

`EventFilter` is a per-envelope delivery gate layered over topic matching. Where
the broker decides *which* subscriptions a topic reaches, a filter decides
whether a reached subscription actually *runs*. Two ship — a header regex filter
and an arbitrary predicate filter. Lumen's envelopes carry an `aggregateType`
header, so a header filter could restrict a subscriber to `Wallet` events:

```rust
use firefly_eda::{handler, with_filters, Event, HeaderEventFilter, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = InMemoryBroker::new();
let inner = handler(|_ev: Event| async { Ok(()) });
let gated = with_filters(inner, [HeaderEventFilter::new("aggregateType", r"^Wallet$").unwrap()]);
broker.subscribe("wallets.events", gated).unwrap();
# });
```

An event must pass *every* filter to be delivered; a non-matching event is
dropped before the handler body runs.

## The reactive subscription surface

`InMemoryBroker::subscribe_reactive(topic)` is the reactive twin of
`subscribe` — a `Flux<Event>` that emits every event delivered to the topic,
composing with the whole Reactor operator set. `publish_mono(event)` is the cold
reactive publish (nothing happens until the `Mono` is subscribed):

```rust
use std::sync::Arc;
use firefly_eda::{Event, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let flux = broker.subscribe_reactive("wallets.*").unwrap();

broker
    .publish_mono(Event::new("wallets.events", "WalletOpened", "lumen", None))
    .block()
    .await
    .unwrap();
broker.close().unwrap(); // terminates the Flux

let events = flux.take(1).collect_list().block().await.unwrap().unwrap();
assert_eq!(events[0].topic, "wallets.events");
# });
```

Deliveries are buffered through a bounded channel; when the downstream consumer
falls behind, the newest events are dropped (`onBackpressureDrop`) rather than
blocking or failing the publisher — extending "a slow consumer never fails
publishers" to the reactive surface. This is the same `Flux` Lumen's optional
streaming endpoint composes over (see [Production & Deployment](./20-production.md)).

## Production transports

Each transport crate implements the same `Broker` port; swap the constructor and
keep every handler. Code against `firefly_eda::Broker` and select the adapter at
wiring time. For Lumen this is a one-line change in `build_app`: replace the
in-memory broker with a Kafka one and the projection, the ledger, and every
command keep compiling unchanged.

| Crate                  | Backend         | Constructor                                  |
|------------------------|-----------------|----------------------------------------------|
| `firefly-eda-kafka`    | Apache Kafka    | `new_kafka_broker(KafkaConfig)?`             |
| `firefly-eda-rabbitmq` | RabbitMQ        | `RabbitMqBroker::new(RabbitMqBrokerConfig)`  |
| `firefly-eda-postgres` | Postgres outbox | `PostgresBroker::new(PostgresConfig::new(dsn))` |
| `firefly-eda-redis`    | Redis Streams   | `RedisStreamsBroker::connect(RedisConfig::new(url))?` |

Kafka, for example — note the handler body is identical to Lumen's:

```rust,no_run
use firefly_eda::{handler, Event};
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};

# async fn ex() -> firefly_eda::EdaResult<()> {
let broker = new_kafka_broker(KafkaConfig {
    brokers: vec!["kafka:9092".into()],
    client_id: "lumen".into(),
    consumer_group: "lumen-projections".into(),
    ..Default::default()
})?;

broker
    .subscribe("wallets.events", handler(|ev: Event| async move {
        println!("observed {}", ev.event_type);
        Ok(())
    }))
    .await?;

let ev = Event::new("wallets.events", "WalletOpened", "lumen", None);
broker.publish(ev).await?;
# Ok(())
# }
```

Redis Streams uses a connect-then-start lifecycle:

```rust,no_run
use firefly_eda::{handler, Event};
use firefly_eda_redis::{RedisConfig, RedisStreamsBroker};

# async fn ex() -> firefly_eda::EdaResult<()> {
let broker = RedisStreamsBroker::connect(
    RedisConfig::new("redis://localhost:6379/0")
        .with_streams(["wallets.events"])
        .with_group("lumen-projections"),
)?;
broker.subscribe("wallets.*", handler(|ev: Event| async move {
    println!("got {}", ev.event_type);
    Ok(())
})).await?;
broker.start().await?;
# Ok(())
# }
```

> **Note** — The Postgres broker is a **transactional outbox**: events are
> written in the same transaction as your state change and drained to consumers
> via `LISTEN`/`NOTIFY`, giving at-least-once delivery without a separate broker.
> That closes the append-then-publish gap discussed above; the
> [next chapter](./11-event-sourcing.md) covers the outbox primitive directly.

## Broker health

`EventPublisherHealthIndicator` adapts any broker implementing the
`BrokerHealth` ping probe to a `firefly_observability::Indicator`, surfacing
broker liveness on `/actuator/health` under the `eventPublisher` id — so when
Lumen graduates to a real broker, its readiness shows up alongside the rest of
the service's health (see [Observability](./15-observability.md)).

## Recap — what changed in Lumen

The CQRS loop is closed. Where Chapter 9's command handlers persisted events and
left the read side to repair itself, Lumen now publishes every persisted event
and projects it back automatically.

| Piece | Role |
|-------|------|
| `EVENTS_TOPIC` / `EVENT_SOURCE` | Shared constants the publisher and listener agree on |
| `to_envelope(&DomainEvent)` | Bridges a persisted domain event to the wire `Event` (key = wallet id, headers carry routing) |
| `Ledger::commit` | Appends, **then** publishes each event — save before you publish |
| `#[event_listener(topic = "wallets.events")]` | Generates `subscribe_project_wallet_event(broker)` |
| `project_wallet_event` | The idempotent rebuild-from-stream projection that feeds the read model |
| `bind_projection` / `projection_state` | Publish-once wiring so the free-fn listener reaches its collaborators |
| `web.broker` (`InMemoryBroker`) | The default transport — swap the constructor for Kafka/RabbitMQ/Redis, keep the listener |

Three principles carry forward: **save before you publish** so a subscriber
never sees an uncommitted fact; **make projections idempotent** so at-least-once
redelivery is harmless (Lumen re-folds the stream rather than applying a delta);
and **depend on the `Broker` port, not the adapter** so the in-memory broker
becomes Kafka with a one-line change.

The events Lumen publishes here are still backed by a transient in-memory store.
The [next chapter](./11-event-sourcing.md) makes those events the *source of
truth* — durable, replayable, the canonical record from which every balance is
recomputed.

## Exercises

1. **Add a `WelcomeNotifier` listener.** Write a second
   `#[event_listener(topic = "wallets.events")]` free fn that reacts only to
   `WalletOpened` (check `ev.event_type`) and logs a welcome line carrying the
   `aggregateId` header. Subscribe it next to the projection in `build_app` and
   confirm — via an `InMemoryBroker` unit test that publishes a `WalletOpened`
   envelope — that it fires, while the existing command handlers stay untouched.

2. **Prove idempotency.** In a test, build a `Ledger` over a `MemoryEventStore`
   and an `InMemoryBroker`, subscribe the projection, open a wallet, and deposit
   twice. Then publish the *same* `MoneyDeposited` envelope a second time with
   `broker.publish(to_envelope(&event))` and assert the read-model `WalletView`
   balance is unchanged — the rebuild-from-stream fold absorbs the redelivery.

3. **Gate by aggregate type.** Wrap the projection's handler with
   `with_filters` and a `HeaderEventFilter::new("aggregateType", r"^Wallet$")`,
   then publish an envelope whose `aggregateType` header is `"Account"` and
   confirm the projection does not run for it. Explain why a header filter is a
   cheaper guard than checking inside the handler body.

4. **Swap in a real broker (sketch).** Add the `eda-kafka` feature to the crate
   and write the `build_app` variant that constructs `new_kafka_broker(...)`
   instead of using `web.broker`. You do not need a running Kafka — the point is
   to confirm the projection, the ledger, and the command handlers compile
   unchanged against the `Broker` port.
