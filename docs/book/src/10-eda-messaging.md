# Event-Driven Architecture & Messaging

By the end of the [CQRS chapter](./09-cqrs.md), Lumen could open a wallet,
deposit, withdraw, and read a balance — but the command side and the query side
were quietly cheating. The `Wallet` aggregate raised crisp domain events
(`WalletOpened`, `MoneyDeposited`, `MoneyWithdrawn`), the `Ledger` persisted
them, and then nothing carried them anywhere. The read model the `GetWallet`
query serves had to be repaired on the fly by re-folding the event stream every
time it was read.

By the end of *this* chapter, Lumen closes that loop. Every event the ledger
persists is also **published** to a broker, and a read-model **projection** — a
bean whose method consumes those published events — keeps the query side current
without the write side ever knowing it exists. That is event-driven
architecture: a fact is published once, and any number of independent reactions
subscribe to it. The audit trail, the welcome notification, the balance read
model — each becomes a subscriber you can add months later without touching a
single command handler.

We will build the loop the way Lumen's own source builds it: a one-function
bridge that turns a persisted domain event into a wire envelope, a publish call
at the end of the ledger's commit, and a projection bean that the framework
discovers and wires for you. Then we will tour the messaging machinery around it
— glob topics, consumer groups, retry/dead-letter, filters, the reactive
surface, in-process events, and the production transports — so you know which
tool reaches for which job.

By the end of this chapter you will:

- Distinguish a **domain event** (event-sourcing's durable fact) from a
  **messaging event** (the wire envelope), and bridge one to the other with a
  single mapping function.
- Publish every committed event from the `Ledger` to a `Broker`, in the order
  that guarantees a subscriber never sees an uncommitted fact.
- Write the read-model **projection** as a `#[derive(Service)]` bean whose
  `#[event_listener]` method the framework discovers and subscribes for you —
  and understand why rebuilding from the stream makes it idempotent.
- Use the broker's reach: glob topic patterns, consumer groups, retry with
  dead-lettering, per-envelope filters, and the reactive `Flux` surface.
- Tell the broker's three event roles apart — `#[event_listener]`,
  `#[application_event_listener]` / `#[transactional_event_listener]`, and
  `externalize_after_commit` — and swap the in-memory broker for Kafka, RabbitMQ,
  Postgres, or Redis without changing a handler.

## Concepts you will meet

Before the first line of code, here are the ideas this chapter leans on. Each is
reintroduced in context where it is first used; this is the short version.

> **Note** **Key term — event-driven architecture (EDA).** A style in which
> components communicate by *publishing facts* rather than calling each other. A
> producer announces that something happened; any number of *subscribers* react,
> and the producer neither knows nor cares who they are. The Spring analog is
> Spring Cloud Stream / Spring for Apache Kafka — a publish/subscribe layer over
> a message broker.

> **Note** **Key term — broker.** A *broker* is the transport that carries a
> published event to its subscribers. `firefly-eda` defines a transport-agnostic
> `Broker` *port*; the in-process `InMemoryBroker` is the default, and Kafka,
> RabbitMQ, Postgres, and Redis implement the same port. This is the role
> Spring's `MessageChannel` / `KafkaTemplate` + listener container plays.

> **Note** **Key term — projection.** A *projection* consumes a stream of events
> and maintains a derived, query-optimized view (the *read model*). It is the
> read side of Command/Query Responsibility Segregation, kept current by
> reacting to the write side's events. Spring developers build these with
> `@KafkaListener` / `@EventListener` methods writing into a query store.

> **Note** **Key term — idempotent.** An operation is *idempotent* when applying
> it more than once has the same effect as applying it once. Under at-least-once
> delivery a broker may hand the same event to a subscriber twice; an idempotent
> projection absorbs the redelivery without corrupting its view.

`firefly-eda` is the framework's **event-driven architecture port**. It defines
the `Event` envelope every Firefly event flows through, the
`Publisher` / `Subscriber` / `Broker` ports, an in-process `InMemoryBroker`, and
the messaging machinery — glob topics, consumer groups, retry/DLQ, event
filters, and a reactive `Flux` subscription surface. The production transports
(Kafka, RabbitMQ, Postgres outbox, Redis Streams) implement the same ports and
slot in at wiring time, so Lumen's projection never changes when the broker
does.

> **Design note.** The `Broker` is Firefly's transport-agnostic messaging port:
> you publish an `Event` to it and subscribe handlers to it. `wrap_listener` adds
> retry and dead-lettering; subscriptions accept glob topic patterns. Because
> every production transport implements the same port, a handler never changes
> when the broker does — the wiring chooses the adapter, the code stays put.

## Step 1 — Tell the two kinds of "event" apart

Before wiring anything, it is worth being precise about the word *event*,
because Lumen ends up using it for two different things and confusing them leads
to reaching for the wrong port.

> **Note** **Key term — domain event vs messaging event.** A *domain event* (the
> event-sourcing kind) is the durable, versioned fact an aggregate raises; it
> lives in the event *store* and is the [next chapter](./11-event-sourcing.md)'s
> subject. A *messaging event* is the wire envelope that carries a fact *to
> subscribers*; it lives on the *broker*. In Spring terms: a domain event is a
> JPA-persisted record in your event table; a messaging event is the payload you
> hand to `KafkaTemplate.send(...)`.

A **domain event** in the event-sourcing sense — `firefly::eventsourcing`'s
`DomainEvent` — is the durable, versioned fact the `Wallet` aggregate raises and
the next chapter makes the source of truth. It lives in the event *store*.

A **messaging event** — `firefly::eda`'s `Event` — is the wire envelope that
carries a fact *to subscribers*. It lives on the *broker*. Lumen bridges the two
with one function (`to_envelope`, built in Step 3): the ledger persists a
`DomainEvent`, then maps it onto an `Event` and publishes it. This chapter is
about the second kind — getting the fact onto the wire and reacting to it. The
first kind is the next chapter's subject.

> **Tip** **Checkpoint.** You can state, in one sentence each, what a domain
> event is (a durable fact in the store) and what a messaging event is (a wire
> envelope on the broker), and which chapter owns each. Keep that distinction in
> hand — every code path below sits firmly on one side of it.

## Step 2 — Read the `Event` envelope

`Event` is Firefly's canonical wire envelope. It has a *stable JSON shape* —
fixed field names and omission rules — so producers and consumers agree on the
bytes regardless of broker or service. Any system that honours the contract
interoperates: the same envelope is wire-compatible across the Java, .NET, Go,
and Python ports of Firefly.

Construct one with `Event::new`, which also stamps `correlation_id` from the
kernel's task-local correlation scope (so a published event carries the same
correlation id as the request that produced it):

```rust
use firefly_eda::Event;

let ev = Event::new(
    "orders.created",   // topic — where it is published
    "OrderCreated",     // event type — the logical name
    "orders-svc",       // source — the producing service
    Some(br#"{"id":"o1"}"#.to_vec()), // payload (base64 on the wire)
)
.with_header("x-tenant", "acme")     // arbitrary routing/metadata header
.with_key(b"customer-42".to_vec());  // partition / routing key
```

What just happened, field by field. `Event::new` takes four positional
arguments — `topic`, `event_type`, `source`, and an optional `payload` — and
fills in the rest: a fresh `id`, the current `time` (UTC), and the ambient
`correlation_id`. The two builder methods are additive: `with_header` inserts a
string→string header into a sorted map (so the encoding is deterministic), and
`with_key` sets the optional partition/routing key.

The `key` carries the intended partition/routing key per the `Event` contract;
it is *omitted* from the wire when absent, so events produced before the field
existed stay byte-for-byte identical. One honest caveat: the current adapters do
not yet route off `key`. The Kafka adapter derives its record key from
`correlation_id` (falling back to the event id), and the RabbitMQ adapter routes
on the topic. Treating `key` as the partition/routing key is the contract's
*design intent* rather than a guarantee of today's adapters.

> **Tip** **Checkpoint.** You can build an `Event` and read back its fields:
> `Event::new("t", "T", "s", None).with_header("k", "v").headers.get("k")`
> returns `Some("v")`, and `.with_key(b"abc".to_vec()).key` is
> `Some(b"abc".to_vec())`. An absent key never appears on the wire.

## Step 3 — Bridge a domain event to the envelope

Lumen never builds an `Event` by hand inside a handler. The ledger owns one
mapping function that turns a persisted `DomainEvent` into the canonical
envelope — carrying the JSON-encoded domain event as the payload and the wallet
id as the intended partition key. Put it in `samples/lumen/src/ledger.rs`
alongside the two shared constants the publisher and the projection both key off:

```rust
use firefly::eda::Event;
use firefly::eventsourcing::DomainEvent;

use crate::domain::AGGREGATE_TYPE; // the const "Wallet"

/// The EDA topic every wallet domain event is published to. The projection
/// and any external subscriber key off it.
pub const EVENTS_TOPIC: &str = "wallets.events";

/// The logical EDA source stamped on published events.
pub const EVENT_SOURCE: &str = "lumen";

/// Maps a persisted `DomainEvent` onto the canonical EDA `Event` envelope,
/// carrying the JSON-encoded domain event as the payload and the wallet id as
/// the partition key (so per-wallet events stay ordered on a real broker).
pub fn to_envelope(event: &DomainEvent) -> Event {
    let payload = serde_json::to_vec(event).expect("domain event serialises");
    Event::new(
        EVENTS_TOPIC,
        event.event_type.clone(),
        EVENT_SOURCE,
        Some(payload),
    )
    .with_key(event.aggregate_id.clone().into_bytes())
    .with_header("aggregateType", AGGREGATE_TYPE)
    .with_header("aggregateId", event.aggregate_id.clone())
    .with_header("version", event.version.to_string())
}
```

What just happened, with three design choices worth pausing on:

- The **topic** (`wallets.events`) is a shared constant. The publisher and the
  projection key off the *same* `EVENTS_TOPIC` value, so the channel name can
  never drift between them — a renamed constant moves both sides at once.
- The **key** is the wallet id. It is the intended partition key so that, once a
  broker routes off it, every event for one wallet lands on the same partition
  and stays in order. (Today's Kafka adapter keys records on `correlation_id`
  and RabbitMQ routes on the topic, so this is the contract's design intent, not
  a current guarantee — exactly as Step 2 explained.)
- The **headers** (`aggregateType`, `aggregateId`, `version`) carry just enough
  routing metadata for a subscriber to find and re-fold the affected aggregate
  *without decoding the payload*. That is precisely what Lumen's projection does
  in Step 5 — it reads `aggregateId` from a header and never touches the body.

> **Tip** **Checkpoint.** A unit test on `to_envelope` (Lumen ships one) asserts
> `env.topic == EVENTS_TOPIC`, `env.event_type == "WalletOpened"`,
> `env.key == Some(b"wlt_x".to_vec())`, and the `aggregateId` / `version` headers
> are set. If those hold, the bridge is faithful.

## Step 4 — Publish from the ledger (save before you publish)

The `Ledger` is the single write path every command and the transfer saga call.
After it appends an aggregate's uncommitted events to the store with optimistic
concurrency, it publishes each one — `to_envelope` then `broker.publish` — so the
projection downstream can react. Here is the `commit` method on `Ledger`:

```rust
use firefly::eda::Broker;
use firefly::eventsourcing::EventSourcingError;

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

What just happened. `take_uncommitted()` drains the events the domain command
produced; if there are none, there is nothing to do. Then `store.append(...)`
persists them at the `expected` version — the optimistic-concurrency check. Only
*after* that succeeds does the loop turn each event into an envelope and publish
it. The `broker` here is an `Arc<dyn Broker>`: the ledger codes against the
*port*, never a concrete transport.

Notice the ordering: **append before publish.** A subscriber must never see a
fact that did not persist. If the append fails — including the
optimistic-concurrency race — the loop is never reached, so no event is
broadcast. The store backing this ledger is the in-memory event store; the
[next chapter](./11-event-sourcing.md) is where that store earns the name
*event-sourced*.

> **Note** Append before publish: a subscriber must never see a fact that did
> not persist. The gap between append and publish — where a crash could persist a
> fact but drop the broadcast — is exactly what the transactional outbox in the
> [next chapter](./11-event-sourcing.md) eliminates.

> **Tip** **Checkpoint.** `cargo test -p lumen` still passes: the ledger's
> open/deposit/withdraw round-trip persists three events and publishes three
> envelopes, in that order, with no subscriber yet attached.

## Step 5 — Watch the in-process broker fan out

`InMemoryBroker` is the default transport — fan-out delivery, glob topic
matching, and per-`(topic, group)` round-robin, with no external dependency. It
is the broker the framework's web stack exposes (and registers into the DI
container as the `Arc<dyn Broker>` port), and it is everything the teaching build
and the test suite need. Before wiring Lumen's projection, see the broker in
isolation — subscribe a handler, publish an event:

```rust
use firefly_eda::{handler, Event, InMemoryBroker};

#[tokio::main]
async fn main() {
    let broker = InMemoryBroker::new();

    broker
        .subscribe(
            "wallets.events",
            handler(|ev: Event| async move {
                println!(
                    "observed {} for {}",
                    ev.event_type,
                    ev.headers.get("aggregateId").map(String::as_str).unwrap_or("?")
                );
                Ok(())
            }),
        )
        .unwrap();

    let ev = Event::new(
        "wallets.events",
        "WalletOpened",
        "lumen",
        Some(br#"{"wallet_id":"wlt_1"}"#.to_vec()),
    );
    broker.publish(ev).await.unwrap();
    broker.close().unwrap();
}
```

What just happened. `handler(closure)` wraps an async closure as a reference-
counted delivery callback (the type the broker stores per subscription).
`subscribe(topic, handler)` registers it for the topic; the inherent method on
the concrete `InMemoryBroker` is synchronous, so it returns a `Result` you
`.unwrap()` rather than `.await`. `publish(ev).await` then runs every matching
handler sequentially on the publisher's task. `close()` releases the broker.

> **Note** `InMemoryBroker::publish` awaits each subscribed handler sequentially
> on the publisher's task; the first handler error short-circuits and is returned
> to the publisher (wrapped in `EdaError::Handler`). After `close()`, both
> publish and subscribe fail with `EdaError::Closed`. (When you reach for the
> `dyn Broker` *port* instead of the concrete type — as Lumen's ledger does —
> the trait methods are `async`, so you `.await` `subscribe` too. The concrete
> inherent methods are sync; the port methods are async. Same broker, two
> surfaces.)

> **Tip** **Checkpoint.** Run that `main`. It prints `observed WalletOpened for
> wlt_1`. If you swap the subscribe topic to a glob like `wallets.*`, it still
> matches — that is Step 6.

## Step 6 — Close the loop with a projection bean

Here is where Lumen closes the CQRS loop. The **projection** is a DI bean — the
Rust analog of a Spring `@Component` with an `@EventListener` method.
`WalletProjection` is a `#[derive(Service)]` whose collaborators are
`#[autowired]` from the container: the `Ledger` (for the event store it replays)
and the `ReadModel` it feeds — the *same* `ReadModel` the `GetWallet` query
reads. A `#[handlers]` impl marks its method with
`#[event_listener(topic = "wallets.events")]`, so for each delivered event the
framework calls it; it reaches its collaborators through `self`, reloads the
affected wallet's stream, folds it into a `WalletView`, and upserts it.

> **Note** **Key term — `#[derive(Service)]` / `#[handlers]` / `#[event_listener]`.**
> `#[derive(Service)]` marks a struct as a singleton DI bean whose `#[autowired]`
> fields the container fills (Spring's `@Service`/`@Component`). `#[handlers]` on
> the impl tells the framework to scan its methods for handler attributes.
> `#[event_listener(topic = …)]` subscribes one method to a broker topic — the
> `@KafkaListener` analog. You write the reaction; the framework does the
> subscribing.

Add this to `samples/lumen/src/ledger.rs`:

```rust
use std::sync::Arc;

use firefly::eda::Event;
use firefly::prelude::*;

use crate::domain::Wallet;
// `Ledger` and `ReadModel` are defined earlier in this same module.

/// The read-model **projection bean** — Spring's `@Component @EventListener`. It
/// `#[autowired]`s the `Ledger` (for the event store it replays) and the
/// `ReadModel` it feeds; `#[handlers]` subscribes its `project` method to
/// `EVENTS_TOPIC`. The idempotent rebuild-from-stream projection that closes the
/// CQRS loop, wired entirely through the DI container with no process-global.
#[derive(Service)]
struct WalletProjection {
    /// The application service whose event store the projection replays
    /// (autowired).
    #[autowired]
    ledger: Arc<Ledger>,
    /// The read model the projection upserts (autowired) — the same instance the
    /// `GetWallet` query reads.
    #[autowired]
    read_model: Arc<ReadModel>,
}

#[handlers]
impl WalletProjection {
    /// Projects one delivered wallet event into the read model.
    #[event_listener(topic = "wallets.events")]
    async fn project(&self, ev: Event) -> FireflyResult<()> {
        let Some(wallet_id) = ev.headers.get("aggregateId") else {
            return Ok(());
        };
        // A transient store miss is swallowed so one poison message never stalls
        // the projection — the EDA at-least-once contract.
        if let Ok(events) = self.ledger.store().load(wallet_id).await {
            let view = Wallet::rehydrate(wallet_id, &events).view();
            self.read_model.upsert(view);
        }
        Ok(())
    }
}
```

What just happened, line by line. The struct has two `#[autowired]` fields, so
the container constructs `WalletProjection` by handing it the existing `Ledger`
and `ReadModel` singletons — no `new`, no wiring code. The `project` method reads
`aggregateId` from a header (the routing metadata Step 3 stamped); if it is
missing, the event is not for us and we return `Ok(())`. Otherwise we `load` the
wallet's full event stream, `rehydrate` the aggregate, take its `.view()`, and
`upsert` it into the read model. The method returns `FireflyResult<()>` — the
framework's `Result<(), FireflyError>`.

Two properties make this a *good* projection rather than merely a working one.

It is **idempotent.** Rather than mutating the read-model row from the single
delivered event (`balance += amount`), it reloads the wallet's full stream and
rebuilds the view from scratch. Under EDA's at-least-once delivery a redelivered
`MoneyDeposited` would double-count if you applied the delta — but re-folding the
same stream converges on the same `WalletView` no matter how many times the
event arrives. The header carries the `aggregateId`; that is all the projection
needs to find the stream.

It is **decoupled.** `WalletProjection` imports no command, calls no handler,
and has no idea a deposit was processed. It reacts purely to the published fact.
You can add a `FraudDetector` or a `WelcomeNotifier` subscriber next to it
without touching a line of the command path — which is exactly Exercise 1.

> **Note** `#[event_listener(topic = "wallets.events")]` on a `#[handlers]` bean
> method submits a `BeanListenerRegistration` into the `inventory` registry the
> framework drains. At boot, `FireflyApplication` resolves `WalletProjection`
> from the container — autowiring its `Ledger` + `ReadModel` — and subscribes its
> `project` method to the topic via
> `subscribe_discovered_listener_beans(broker, container)`. The subscription is
> wired for you; you write only the reaction.

> **Tip** **Checkpoint.** `cargo test -p lumen` passes the full HTTP loop: a
> `POST /api/v1/wallets/:id/deposit` flows command → ledger → store → broker →
> projection → read model, and the next `GET /api/v1/wallets/:id` is served from
> the projected view — no manual repair.

## Step 7 — Understand how the projection is wired (no composition root)

Because the projection is a regular container bean, its collaborators arrive by
**constructor injection** through `#[autowired]` fields — no process-global to
seed, no `bind` step. The container hands `WalletProjection` the same `Ledger`
(hence the same event store) and the same `ReadModel` it hands the CQRS handlers,
so the events the handlers publish are exactly the events the projection consumes
and projects into the read the `GetWallet` query serves.

That is why the `ledger` `#[bean]` factory in `samples/lumen/src/web.rs` is now a
**pure factory** — it builds the `Ledger` and returns it, with no
projection-seeding side effect:

```rust,ignore
// samples/lumen/src/web.rs — the `ledger` #[bean] factory.
#[bean]
fn ledger(&self, store: Arc<MemoryEventStore>, broker: Arc<dyn Broker>) -> Ledger {
    let store: Arc<dyn EventStore> = store;
    Ledger::new(store, broker)
}
```

What just happened. The factory's parameters are themselves autowired: the
container provides the `MemoryEventStore` bean and the `Arc<dyn Broker>` port
(which the web stack registers, defaulting to `InMemoryBroker`). The factory
upcasts the concrete store to the `dyn EventStore` port and constructs the
`Ledger`. There is no subscribe call here, and no composition root anywhere.

The subscription itself is wired by `FireflyApplication` at boot:
`subscribe_discovered_listener_beans(broker, container)` resolves the projection
bean and drains its `#[event_listener]` method onto the broker (alongside the
free-`fn` `subscribe_discovered_listeners` for listeners that need no injected
collaborators). So neither the `ledger` factory nor any composition root calls a
subscribe helper by hand.

> **Design note.** The whole loop is *declared*, not *assembled*. You declare the
> store bean, the read-model bean, the ledger factory, and the projection bean;
> the framework discovers each, autowires its dependencies, and subscribes the
> listener — the Spring `@Configuration` + `@Bean` + component-scan story, with
> the listener-endpoint registry replaced by the inventory drain. When a listener
> needs *no* injected collaborators, the simpler free-`fn` form — a bare
> `#[event_listener(topic = "…")] async fn(ev: Event) -> FireflyResult<()>` — is
> the alternative, discovered the same way.

> **Tip** **Checkpoint.** Read Lumen's startup report. The
> `:: cqrs handlers: … | event listeners: … | scheduled tasks: …` line now counts
> at least one event listener — the projection the framework just subscribed.

## Step 8 — Reach further: glob topics and consumer groups

A subscription topic is a glob *pattern* (`*`, `?`, `[..]`, `{a,b}`); a published
event is delivered to every subscription whose pattern matches its topic. Lumen
subscribes to the exact `wallets.events`, but a multi-event service could fan a
single listener across a family:

```rust,ignore
broker.subscribe("wallets.*", handler(|ev| async move { Ok(()) })).unwrap();
// matches wallets.events, wallets.audit, ...
```

> **Note** **Key term — consumer group.** A *consumer group* is a set of
> subscribers that *compete* for a topic's events: each matching event goes to
> exactly **one** member of the group (round-robin), while distinct groups each
> get their own copy. This is Kafka's consumer-group model and Spring's
> `group` / `@KafkaListener(groupId=…)` — the way you scale a workload
> horizontally without double-processing.

```rust,ignore
broker.subscribe_group("wallets.events", "projections", handler1).unwrap();
broker.subscribe_group("wallets.events", "projections", handler2).unwrap();
// each event reaches exactly one of handler1/handler2
```

This is how you would scale Lumen's projection horizontally: run several
projector instances in one group and the broker shares the events among them
(round-robin per `(topic, group)`), each instance owning a slice of the wallet
space. An ungrouped subscription — the kind `#[event_listener]` makes by default
— always receives its own copy.

> **Tip** **Checkpoint.** In an `InMemoryBroker` test, subscribe two handlers to
> the same group and publish two events; each handler runs once. Subscribe two
> *ungrouped* handlers and publish one event; both run. That is fan-out versus
> competing-consumer, in four lines.

## Step 9 — Make failures survivable: retry and dead-letter

`wrap_listener(handler, publisher, policy)` is the adapter-agnostic retry/DLQ
wrapper. A failing delivery is retried up to `retries` times with linear backoff
(`retry_delay * attempt`); on exhaustion the event is republished to the
dead-letter topic (when set), carrying the original payload, key, and headers
plus `x-original-topic` and `x-exception` diagnostic headers:

> **Note** **Key term — dead-letter topic (DLT/DLQ).** When a message keeps
> failing, you do not want it blocking the stream forever. A *dead-letter topic*
> is where exhausted messages are parked for later inspection or replay. This is
> Spring Kafka's `DefaultErrorHandler` dead-letter routing and
> `@RetryableTopic`.

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

What just happened. `ListenerPolicy::with_retries(3)` sets three retries after
the first attempt (four attempts total); `.retry_delay(...)` adds linear
backoff; `.dead_letter_topic(...)` names the topic to park exhausted events on.
`wrap_listener` returns a new `Handler` you subscribe in place of the inner one.
A policy with no retries, no topic, and no store is a pass-through — it returns
the original handler unchanged, so wrapping is zero-overhead when unconfigured.

Lumen's projection takes a gentler path — it *swallows* a transient store miss
and returns `Ok(())` rather than failing the delivery, so a single poison message
never stalls the stream. That is the right call for a rebuild-from-stream
projection: the next redelivery, or the next event for the wallet, converges
anyway. A *side-effecting* listener — one that sends an email or calls an
external API — is where `wrap_listener` and a dead-letter topic earn their keep,
because there the work cannot simply be re-derived.

For an inspectable record of failures (rather than a routing topic), wire an
`EdaDeadLetterStore` via `ListenerPolicy::dead_letter_store`: an exhausted event
is captured into the store (queryable with `list` / `get` / `remove`). You can
set both — capture *and* route — on one policy.

> **Tip** **Checkpoint.** Wrap an always-failing handler with
> `ListenerPolicy::with_retries(2).dead_letter_topic("orders.DLT")` over a broker
> that records publishes; after one delivery, exactly one event lands on
> `orders.DLT` with an `x-original-topic` header, and the wrapped handler returns
> `Ok(())` rather than erroring.

## Step 10 — Gate delivery with event filters

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

What just happened. `HeaderEventFilter::new(name, pattern)` compiles an anchored
regex against the named header (a missing header is treated as the empty string).
`with_filters(handler, [filters])` wraps the handler so it runs only for events
that pass *every* filter; a non-matching event is dropped before the handler body
runs — the wrapped handler simply returns `Ok(())`. An empty filter list returns
the handler unchanged (zero overhead). `PredicateEventFilter::new(closure)` is
the escape hatch when a regex over a header is not enough — it gates on any
property of the envelope.

> **Tip** **Checkpoint.** Build `HeaderEventFilter::new("aggregateType",
> r"^Wallet$")`, wrap a counting handler with `with_filters`, then deliver one
> envelope whose `aggregateType` is `"Account"` and one that is `"Wallet"`. Only
> the second increments the counter. A header filter is cheaper than an `if`
> inside the handler because the drop happens before your code runs — Exercise 3.

## Step 11 — Consume reactively as a `Flux`

`InMemoryBroker::subscribe_reactive(topic)` is the reactive twin of `subscribe` —
a `Flux<Event>` that emits every event delivered to the topic, composing with
Firefly's full reactive-streams operator set. `publish_mono(event)` is the cold
reactive publish: nothing happens until the returned `Mono` is subscribed.

> **Note** **Key term — `Flux` / `Mono`.** `Flux<T>` is a reactive stream of
> *many* `T`; `Mono<T>` is a reactive stream of *at most one* `T`. They are
> Firefly's port of Project Reactor's `Flux` / `Mono` (Spring WebFlux). Both are
> *cold* and *lazy*: building one does no work; the work runs when you subscribe
> (here, `.block().await`).

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

What just happened. `subscribe_reactive("wallets.*")` returns a `Flux<Event>`
backed by a bounded channel. `publish_mono(...)` builds a cold `Mono<()>`;
`.block().await` drives it, running the publish. Closing the broker drops the
sender, which terminates the `Flux`. Then `flux.take(1).collect_list()` composes
two operators into a `Mono<Vec<Event>>`; `.block().await` yields a
`Result<Option<Vec<Event>>, _>`, so the two `.unwrap()`s unwrap the `Result` and
then the `Option`.

> **Note** Deliveries are buffered through a bounded channel; when the downstream
> consumer falls behind, the newest events are dropped (`onBackpressureDrop`)
> rather than blocking or failing the publisher — extending the broker's "a slow
> consumer never fails publishers" invariant to the reactive surface. This is the
> same `Flux` Lumen's optional streaming endpoint composes over (see
> [Production & Deployment](./20-production.md)).

> **Tip** **Checkpoint.** The assertion above holds: one published event arrives
> on the `Flux`, and its `topic` is `wallets.events` even though you subscribed to
> the glob `wallets.*`.

## Step 12 — In-process events and after-commit externalization

The broker carries events *between* services. Inside one service you often want
the same decoupling without a network hop: one component raises a fact, others
react, and none of them knows the others exist. That is Spring's
`ApplicationEventPublisher` / `@EventListener`, and Firefly ships it as a
thread-safe, async, in-process bus alongside the broker.

> **Note** **Key term — in-process event bus.** A *process-local* publish/subscribe
> bus: you `publish_event(value)` and any `#[application_event_listener]` for that
> type reacts — no broker, no network, no serialization. Spring's
> `ApplicationEventPublisher.publishEvent(...)` + `@EventListener`.

Publish with `publish_event`, and listen with `#[application_event_listener]`
on a free async function that takes the event by shared reference. Listeners are
discovered across the crate graph (the same `inventory` scan that finds your
components), so there is no manual registration:

```rust,ignore
use firefly::prelude::*;

struct WalletOpened { id: String }

#[firefly::application_event_listener]
async fn audit_opening(event: &WalletOpened) {
    tracing::info!(wallet = %event.id, "wallet opened");
}

// somewhere in a command handler:
publish_event(WalletOpened { id: wallet_id }).await;
```

### Listening relative to a transaction

A plain listener runs the instant you publish. Often that is too early: you do
not want to send a "wallet opened" notification until the database transaction
that opened it has actually committed. `#[transactional_event_listener]` binds
the listener to a phase of the surrounding `#[transactional]` boundary —
`after_commit` (the default), `before_commit`, `after_rollback`, or
`after_completion`:

```rust,ignore
#[firefly::transactional_event_listener]               // after_commit
async fn notify_owner(event: &WalletOpened) {
    // Runs only once the opening transaction commits; never on a rollback.
    mailer.send_welcome(&event.id).await;
}
```

Events published inside a transaction are buffered and dispatched at the chosen
phase; a rolled-back transaction fires the `after_rollback` listeners and never
the `after_commit` ones, so a failed write can never leak a "success"
side-effect. With no transaction active the listener falls back to running
immediately (treating the work as already committed), so the same handler is
useful in a unit test or a datasource-less path. If you want transactional
event semantics without a SQL datasource at all, register the
`LocalTransactionManager` (the Rust equivalent of Spring's
`ResourcelessTransactionManager`).

### Bridging in-process events to the broker

The two layers compose into the pattern you almost always want: do the in-process
work, and once it commits, publish an integration event to the broker — never a
"ghost" message for a transaction that rolled back. That is Spring Modulith's
event externalization, and `externalize_after_commit` wires it in one line:

```rust,ignore
// at startup, once per externalized event type:
firefly::eda::externalize_after_commit::<WalletOpened>("wallet.events", "wallet.opened");

// thereafter, an ordinary in-process publish inside a transaction...
publish_event(WalletOpened { id: wallet_id }).await;
// ...is serialized to JSON and published to the "wallet.events" topic on the
// registered broker the moment the transaction commits.
```

`externalize_after_commit` simply registers an `after_commit` listener that
forwards through `publish_to_broker` (which serializes the payload and publishes
via the `register_broker`-registered `Broker`). A committed transaction reaches
Kafka, RabbitMQ, or whichever transport you wired; a rolled-back one publishes
nothing. Forwarding after commit is best-effort — a missing broker or a publish
failure does not unwind the already-committed transaction; reach for a real
outbox (next chapter) when you need at-least-once.

Three distinct roles, easy to keep straight:

- `#[event_listener("topic")]` *consumes* from a broker topic — the
  `@KafkaListener` analog (Lumen's projection in Step 6).
- `#[application_event_listener]` / `#[transactional_event_listener]` handle
  *in-process* events.
- `externalize_after_commit` is the *bridge* from the second to a broker producer.

> **Tip** **Checkpoint.** You can name, for each of those three, whether it
> crosses a process boundary (only the first and the bridge do) and whether it is
> transaction-aware (the transactional listener and the bridge are).

## Step 13 — Swap in a production transport

Each transport crate implements the same `Broker` port; swap the constructor and
keep every handler. Code against `firefly_eda::Broker` and select the adapter at
wiring time — for a `FireflyApplication` service that is a `firefly.*` config knob
(or a `#[bean]` that provides the `dyn Broker` port). Replace the in-memory broker
with a Kafka one and the projection, the ledger, and every command keep compiling
unchanged.

| Crate                  | Backend         | Constructor                                       |
|------------------------|-----------------|---------------------------------------------------|
| `firefly-eda-kafka`    | Apache Kafka    | `new_kafka_broker(KafkaConfig)?`                  |
| `firefly-eda-rabbitmq` | RabbitMQ        | `RabbitMqBroker::new(RabbitMqBrokerConfig)`       |
| `firefly-eda-postgres` | Postgres outbox | `PostgresBroker::new(PostgresConfig::new(dsn))`   |
| `firefly-eda-redis`    | Redis Streams   | `RedisStreamsBroker::connect(RedisConfig::new(url))?` |

Kafka, for example — note the handler body is identical to Lumen's, and because
you hold a `Box<dyn Broker>` here the trait methods are `async`:

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

What just happened. `new_kafka_broker(KafkaConfig { … })?` returns a
`Box<dyn Broker>` (hence the `?`). On the *port*, `subscribe` and `publish` are
`async` trait methods, so you `.await` both — the only difference from the
concrete `InMemoryBroker` in Step 5, whose inherent methods are sync. The closure
inside `handler(...)` is byte-for-byte what you would write for the in-memory
broker.

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

What just happened. `RedisStreamsBroker::connect(config)?` dials Redis and
returns the broker; `RedisConfig::new(url).with_streams([...]).with_group(...)`
is the builder. You `subscribe` before `start()` — `start()` begins consuming
from the declared streams. (RabbitMQ has the same connect/start shape;
`RabbitMqBroker::new(config)` returns the broker and `start()` declares its
topology.)

> **Note** The Postgres broker is a **transactional outbox**: events are written
> in the same transaction as your state change and drained to consumers via
> `LISTEN`/`NOTIFY`, giving at-least-once delivery without a separate broker. That
> closes the append-then-publish gap from Step 4; the
> [next chapter](./11-event-sourcing.md) covers the outbox primitive directly.

> **Tip** **Checkpoint.** Add the `eda-kafka` feature and provide the `dyn Broker`
> port as a `#[bean]` that constructs `new_kafka_broker(...)`. You do not need a
> running Kafka — `cargo build` confirms the projection, the ledger, and the
> command handlers compile unchanged against the port. That is Exercise 4.

## Broker health

`EventPublisherHealthIndicator` adapts any broker implementing the `BrokerHealth`
ping probe to a `firefly_observability::Indicator`, surfacing broker liveness on
`/actuator/health` under the `eventPublisher` id — so when Lumen graduates to a
real broker, its readiness shows up alongside the rest of the service's health
(see [Observability](./15-observability.md)). The in-memory broker reports `UP`
until it is closed.

## Recap — what changed in Lumen

The CQRS loop is closed. Where Chapter 9's command handlers persisted events and
left the read side to repair itself, Lumen now publishes every persisted event
and projects it back automatically.

| Piece | Role |
|-------|------|
| `EVENTS_TOPIC` / `EVENT_SOURCE` | Shared constants the publisher and listener agree on |
| `to_envelope(&DomainEvent)` | Bridges a persisted domain event to the wire `Event` (key = wallet id, headers carry routing) |
| `Ledger::commit` | Appends, **then** publishes each event — save before you publish |
| `WalletProjection` (`#[derive(Service)]` + `#[handlers]`) | The projection **bean**: `#[autowired]`s the `Ledger` + `ReadModel`, its `#[event_listener]` method rebuilds the read model from the stream |
| `#[event_listener(topic = "wallets.events")]` | Marks the bean method; submits a `BeanListenerRegistration` the framework drains (`subscribe_discovered_listener_beans`) — resolving the bean and subscribing the method |
| Constructor injection | The projection reaches its collaborators through `#[autowired]` fields — no `OnceLock`, no `bind`; the `ledger` `#[bean]` is a pure factory |
| framework `Broker` (`InMemoryBroker`) | The default transport — swap the adapter for Kafka/RabbitMQ/Redis/Postgres, keep the listener |

You also now know:

- The difference between a **domain event** (durable, in the store) and a
  **messaging event** (the wire envelope, on the broker) — and which chapter owns
  each.
- The broker's reach: glob topics, fan-out vs. competing-consumer **groups**,
  `wrap_listener` retry/dead-letter, per-envelope **filters**, and the reactive
  `Flux` surface.
- The three event roles — `#[event_listener]` (broker consume),
  `#[application_event_listener]` / `#[transactional_event_listener]` (in-process),
  and `externalize_after_commit` (the bridge).

Three principles carry forward: **save before you publish** so a subscriber never
sees an uncommitted fact; **make projections idempotent** so at-least-once
redelivery is harmless (Lumen re-folds the stream rather than applying a delta);
and **depend on the `Broker` port, not the adapter** so the in-memory broker
becomes Kafka with a one-line change.

The events Lumen publishes here are still backed by a transient in-memory store.
The [next chapter](./11-event-sourcing.md) makes those events the *source of
truth* — durable, replayable, the canonical record from which every balance is
recomputed.

## Exercises

1. **Add a `WelcomeNotifier` listener.** Because the notifier needs no injected
   collaborators, reach for the simpler free-`fn` form: write a
   `#[event_listener(topic = "wallets.events")] async fn` that reacts only to
   `WalletOpened` (check `ev.event_type`) and logs a welcome line carrying the
   `aggregateId` header. The framework drains the new listener automatically —
   you add no subscribe call. Confirm — via an `InMemoryBroker` unit test that
   publishes a `WalletOpened` envelope — that it fires, while the existing command
   handlers stay untouched.

2. **Prove idempotency.** In a test, build a `Ledger` over a `MemoryEventStore`
   and an `InMemoryBroker`, subscribe the projection, open a wallet, and deposit
   twice. Then publish the *same* `MoneyDeposited` envelope a second time with
   `broker.publish(to_envelope(&event)).await` and assert the read-model
   `WalletView` balance is unchanged — the rebuild-from-stream fold absorbs the
   redelivery.

3. **Gate by aggregate type.** Wrap the projection's handler with `with_filters`
   and a `HeaderEventFilter::new("aggregateType", r"^Wallet$")`, then publish an
   envelope whose `aggregateType` header is `"Account"` and confirm the
   projection does not run for it. Explain why a header filter is a cheaper guard
   than checking inside the handler body. (Hint: the drop happens before the
   handler is invoked.)

4. **Swap in a real broker (sketch).** Add the `eda-kafka` feature to the crate
   and provide the `dyn Broker` port as a `#[bean]` that constructs
   `new_kafka_broker(...)` instead of relying on the default in-memory broker. You
   do not need a running Kafka — the point is to confirm the projection, the
   ledger, and the command handlers compile unchanged against the `Broker` port.

5. **Route a failure.** Wrap an always-failing handler with
   `wrap_listener(inner, broker.clone(), ListenerPolicy::with_retries(2)
   .dead_letter_topic("wallets.events.DLT"))`, subscribe it, and subscribe a
   second handler to `wallets.events.DLT`. Publish one event and assert the
   dead-letter handler observes it carrying an `x-original-topic` header of
   `wallets.events`.

## Where to go next

- Make these events durable and replayable in **[Event
  Sourcing](./11-event-sourcing.md)** — where the in-memory store becomes the
  source of truth and the transactional outbox closes the append-then-publish gap.
- Surface broker liveness and request metrics in
  **[Observability](./15-observability.md)** — the `eventPublisher` health
  indicator joins the rest of the actuator surface.
- Wire a real Kafka or RabbitMQ transport and the reactive streaming endpoint in
  **[Production & Deployment](./20-production.md)**.
