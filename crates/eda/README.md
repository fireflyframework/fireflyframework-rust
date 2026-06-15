# `firefly-eda`

> **Tier:** Platform · **Status:** Stable (in-memory broker; dedicated Kafka/RabbitMQ/Redis/Postgres transport crates ship)

## Overview

`firefly-eda` is the framework's **event-driven architecture layer**. It
defines the `Event` envelope every Firefly event flows through, the
`Publisher` / `Subscriber` / `Broker` ports, and an in-process fan-out
`InMemoryBroker`. Production transports share the same ports and ship in
dedicated crates: `firefly-eda-kafka`, `firefly-eda-rabbitmq` (Stable),
`firefly-eda-redis` (Stable), and `firefly-eda-postgres`. Each provides
its own factory (e.g. `firefly_eda_kafka::new_kafka_broker`) and the
`firefly` facade feature-gates all four.

The in-crate `new_kafka_broker` / `new_rabbitmq_broker` are intentional
sentinels: they return the typed errors `EdaError::KafkaUnavailable` /
`EdaError::RabbitMqUnavailable` so a deployment that wires the abstract
crate without selecting a real transport fails loud at startup rather
than silently falling back to in-memory. Reach for each transport
crate's own factory to get a live broker.

`Event` has a stable, language-neutral wire format: fixed JSON field
names (`id`, `type`, `source`, `topic`, `correlationId`, `time`,
`headers`, `payload`), deterministic omission rules (`correlationId`
and `headers` are dropped when empty), and standard-base64 `payload`
encoding (`null` when absent).

## Design notes

- **Synchronous fan-out.** `InMemoryBroker::publish` awaits each
  subscribed handler sequentially on the publisher's task. The first
  handler error short-circuits dispatch and is returned to the publisher
  unchanged (wrapped transparently in `EdaError::Handler`).
- **Closed means closed.** After `close()`, publish and subscribe fail
  with `EdaError::Closed`; `close` itself stays idempotent.
- **Correlation propagation.** `Event::new` stamps `correlationId` from
  the kernel's task-local correlation scope
  (`firefly_kernel::with_correlation_id`).
- **Object-safe ports.** `Publisher` / `Subscriber` are `async_trait`
  traits, so adapters compose behind `Arc<dyn Broker>`. `Handler` is a
  reference-counted async closure; build one with the `handler(...)`
  helper.
- **Channel subscriptions (Rust extra).** `subscribe_channel(topic)`
  returns a `tokio::sync::mpsc::UnboundedReceiver<Event>` for
  stream-style consumption; a dropped receiver never fails publishers.

## Public surface

```rust
pub struct Event {            // JSON: id/type/source/topic/correlationId/time/headers/payload[/key]
    pub id: String,
    pub event_type: String,   // serialized as "type"
    pub source: String,
    pub topic: String,
    pub correlation_id: String,
    pub time: DateTime<Utc>,
    pub headers: BTreeMap<String, String>,
    pub payload: Option<Vec<u8>>,
    pub key: Option<Vec<u8>>, // base64; OMITTED when None (pyfly Message.key)
}
impl Event {
    fn new(topic, event_type, source, payload) -> Event;
    fn with_header(key, value) -> Event;
    fn with_key(key: impl Into<Vec<u8>>) -> Event;
}

pub type Handler = Arc<dyn Fn(Event) -> HandlerFuture + Send + Sync>;
pub fn handler(f) -> Handler;                 // wrap an async closure

#[async_trait] pub trait Publisher  { async fn publish(&self, ev: Event) -> EdaResult<()>; async fn close(&self) -> EdaResult<()>; }
#[async_trait] pub trait Subscriber {
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()>;                 // topic may be a glob pattern
    async fn subscribe_group(&self, topic: &str, group: &str, h: Handler) -> EdaResult<()>; // default delegates to subscribe
    async fn close(&self) -> EdaResult<()>;
}
pub trait Broker: Publisher + Subscriber {}   // blanket-implemented

pub struct InMemoryBroker;                    // fan-out + glob match + per-(topic,group) round-robin
pub fn new_kafka_broker(KafkaConfig) -> EdaResult<Box<dyn Broker>>;       // sentinel until wired
pub fn new_rabbitmq_broker(RabbitMqConfig) -> EdaResult<Box<dyn Broker>>; // sentinel until wired

// Retry + dead-letter listener wrapper (pyfly messaging.wrap_listener)
pub struct ListenerPolicy { pub retries: u32, pub retry_delay: Duration, pub dead_letter_topic: Option<String> }
pub fn wrap_listener(h: Handler, publisher: Arc<dyn Publisher>, policy: ListenerPolicy) -> Handler;
pub const HEADER_ORIGINAL_TOPIC: &str;        // "x-original-topic"
pub const HEADER_EXCEPTION: &str;             // "x-exception"

pub enum EdaError { KafkaUnavailable, RabbitMqUnavailable, Closed, Handler(FireflyError) }
pub type EdaResult<T> = Result<T, EdaError>;  // EdaError: Into<FireflyError>
```

## Quick start

```rust
use firefly_eda::{handler, Event, InMemoryBroker};

#[tokio::main]
async fn main() {
    let broker = InMemoryBroker::new();

    broker
        .subscribe(
            "orders.created",
            handler(|ev: Event| async move {
                println!("got order {}", ev.id);
                Ok(())
            }),
        )
        .unwrap();

    let ev = Event::new(
        "orders.created",
        "OrderCreated",
        "orders-svc",
        Some(br#"{"id":"o1"}"#.to_vec()),
    );
    broker.publish(ev).await.unwrap();
    broker.close().unwrap();
}
```

## Messaging surface

The abstraction-layer surface below covers raw-bytes publishing,
serialization, routing keys, glob topics, consumer groups, retry/DLQ,
filters, a queryable dead-letter store, and broker health (the
transports themselves live in the dedicated `firefly-eda-kafka` /
`firefly-eda-rabbitmq` crates):

### Raw-bytes publish convenience

`PublisherExt::publish_bytes(topic, type, source, payload)` is a one-call
helper over any `Publisher` — it builds the `Event` (stamping the
correlation id from the ambient scope) and publishes it, so the common
"send this payload to this topic" call needn't spell out `Event::new`:

```rust,ignore
use firefly_eda::{InMemoryBroker, PublisherExt};

broker
    .publish_bytes("orders.created", "OrderCreated", "orders-svc", Some(payload))
    .await?;
```

It is blanket-implemented for every publisher, including
`Arc<dyn Publisher>`.

### Pluggable event serializer

`EventSerializer` is the codec port a transport uses to turn an `Event`
into wire bytes and back. The built-in `JsonEventSerializer` encodes via
the **canonical `Event` JSON codec**, so its bytes are byte-for-byte
compatible with the in-memory broker's wire format; selecting it is a
zero-behaviour-change default. `AvroEventSerializer` /
`ProtobufEventSerializer` are failing-loud, not-yet-implemented sentinels
(construct them, but `serialize`/`deserialize` return
`EdaError::Serialization` until a Schema-Registry / descriptor adapter is
wired in). `serializer_for(name)` selects one by name for a
`serialization-format` config key:

```rust,ignore
use firefly_eda::{serializer_for, EventSerializer, JsonEventSerializer};

let codec = JsonEventSerializer::new();
let bytes = codec.serialize(&event)?;        // canonical Event JSON
let back = codec.deserialize(&bytes)?;

let chosen = serializer_for("avro")?;          // by config string ("json"|"avro"|"protobuf")
assert_eq!(chosen.name(), "avro");
```

A transport that adopts the port serializes via `serialize` on publish and
reconstructs via `deserialize` on receipt; one that does not keeps using
the canonical `Event` JSON codec directly (the identical bytes).

### Partition / routing key on `Event`

`Event` carries an optional `key: Option<Vec<u8>>` — the value brokers
use for Kafka partitioning and RabbitMQ routing. It serializes as a
standard-base64 string and is **omitted** from the wire when absent
(unlike `payload`, which encodes `null`), so events produced before the
field existed stay byte-for-byte identical. Set it with
`Event::with_key`:

```rust
use firefly_eda::Event;
let ev = Event::new("orders", "OrderPlaced", "svc", Some(b"{}".to_vec()))
    .with_key(b"customer-42".to_vec());
```

### Glob topic patterns

`subscribe(topic, …)` treats `topic` as a glob pattern (`*`, `?`,
`[..]`, `{a,b}`); a published event is delivered to a subscription when
the event's `topic` matches (`subscribe("user.*", …)` matches
`user.created`). A pattern with no glob metacharacters matches only its
literal, so exact subscriptions behave exactly as before. An invalid
pattern is rejected at subscribe time with a `400` `EdaError::Handler`.

### Consumer groups (round-robin)

`subscribe_group(topic, group, handler)` adds a `Subscriber` member to a
consumer `group`. Within a group each matching event goes to exactly
**one** member, chosen round-robin via a per-group `AtomicUsize` cursor;
distinct groups — and ungrouped subscriptions — each receive their own
copy. The trait default delegates to `subscribe` (correct for transports
whose broker enforces group delivery natively); `InMemoryBroker`
overrides it to implement competing-consumer delivery in-process.

### Retry + dead-letter listener wrapper

`wrap_listener(handler, publisher, ListenerPolicy { retries, retry_delay,
dead_letter_topic })` is the adapter-agnostic retry/DLQ wrapper, in the
style of retryable-topic patterns from mature messaging frameworks. A
failing delivery is retried up to `retries` times with **linear backoff**
(`retry_delay * attempt`); on exhaustion the event is republished to
`dead_letter_topic` (when set) carrying the original payload/key/headers
plus the `x-original-topic` (`HEADER_ORIGINAL_TOPIC`) and `x-exception`
(`HEADER_EXCEPTION`, the failing error's stable code) diagnostic headers.
With no retries and no DLQ the original handler `Arc` is returned
unchanged (zero overhead).

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
        .dead_letter_topic("orders.DLT"),
);
broker.subscribe("orders", wrapped).unwrap();
# });
```

### Event filters (delivery gates)

`EventFilter` is a per-envelope delivery gate layered *over* the broker's
glob topic matching. Where the broker decides *which* subscriptions a
topic reaches, a filter decides whether a reached subscription actually
*runs*. Two filters ship:

- `HeaderEventFilter::new(name, pattern)` — accepts events whose header
  `name` matches a start-anchored regular expression (a missing header
  is treated as the empty string).
- `PredicateEventFilter::new(closure)` — accepts events for which an
  arbitrary `Fn(&Event) -> bool` returns `true`.

Attach a chain to a handler with `with_filters` (or `with_filter_chain`
for a pre-boxed `Vec<Arc<dyn EventFilter>>`). An event must pass *every*
filter to be delivered; a non-matching event is dropped before the
handler body runs (the wrapped handler returns `Ok(())`). An empty chain
returns the original handler `Arc` unchanged (zero overhead).

```rust
use firefly_eda::{handler, with_filters, Event, HeaderEventFilter, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = InMemoryBroker::new();
let inner = handler(|_ev: Event| async { Ok(()) });
let gated = with_filters(inner, [HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap()]);
broker.subscribe("orders", gated).unwrap();
# });
```

### Queryable dead-letter store

`EdaDeadLetterStore` is a store of *failed events* captured for operator
inspection and replay. It is distinct from the routing DLQ above:
`wrap_listener`'s `dead_letter_topic` republishes an
exhausted event to a dead-letter *topic*, whereas the store keeps an
inspectable, queryable record (`add` / `list(limit)` / `get(id)` /
`remove(id)`) of the failed events themselves. `EdaDeadLetterEntry`
carries the full failing `Event`, a stable id, the error `code`/detail,
the capture timestamp, and the total attempt count; `list` returns
entries most-recent-first.

Wire a store into the retry/DLQ path with
`ListenerPolicy::dead_letter_store`: an exhausted event is captured into
the store (and, when a topic is also set, republished). With a store
configured the wrapped handler does not re-raise on exhaustion.

```rust
use std::sync::Arc;
use firefly_eda::{handler, wrap_listener, InMemoryBroker, InMemoryEdaDeadLetterStore, ListenerPolicy};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let store = Arc::new(InMemoryEdaDeadLetterStore::new());
let inner = handler(|_ev| async { Err(firefly_kernel::FireflyError::validation("bad")) });
let wrapped = wrap_listener(
    inner,
    broker.clone(),
    ListenerPolicy::with_retries(2).dead_letter_store(store.clone()),
);
broker.subscribe("orders", wrapped).unwrap();
# });
```

### Broker health indicator

`EventPublisherHealthIndicator` adapts any broker that implements the
`BrokerHealth` trait (a `ping()` liveness probe) to a
`firefly_observability::Indicator`, surfacing broker liveness on
`/actuator/health` under the `eventPublisher` id. `InMemoryBroker`
implements `BrokerHealth` (live unless closed); the Kafka / RabbitMQ
transport
crates can implement it with a real connection probe and register their
own indicator.

```rust
use std::sync::Arc;
use firefly_eda::{EventPublisherHealthIndicator, InMemoryBroker};
use firefly_observability::{Indicator, Status};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let indicator = EventPublisherHealthIndicator::new(broker.clone());
assert_eq!(indicator.check().await.status, Status::Up);
broker.close().unwrap();
assert_eq!(indicator.check().await.status, Status::Down);
# });
```

For Kafka in production:

```rust
use firefly_eda::{new_kafka_broker, KafkaConfig};

let broker = new_kafka_broker(KafkaConfig {
    brokers: vec!["kafka:9092".into()],
    client_id: "orders".into(),
    consumer_group: "orders-group".into(),
    ..KafkaConfig::default()
});
// `broker` is an EdaResult<Box<dyn Broker>> satisfying both Publisher
// and Subscriber once a Kafka-backed crate is registered; today it is
// Err(EdaError::KafkaUnavailable).
```

## Bridge to the in-process event system

Firefly keeps two event layers distinct, exactly as Spring (and Spring
Modulith) do:

- **In-process application events** — `firefly::publish_event(e)` with
  `#[firefly::application_event_listener]` /
  `#[firefly::transactional_event_listener]` (the
  [`firefly-transactional`](../transactional) crate). Synchronous,
  in-VM, transaction-aware; never touches a broker.
- **The message broker** — this crate. The
  `#[event_listener("topic")]` macro is the **broker consumer** side: it
  expands to a `Subscriber::subscribe` against the registered broker
  (Spring's `@KafkaListener`), so it receives events that arrive *over
  the wire*. It is **not** an in-process listener — do not confuse it
  with `#[firefly::application_event_listener]`.

The `registry` module is the bridge between the two: it lets a committed
in-process event be forwarded to the broker, so a message is published
for work that committed and never for work that rolled back (Spring
Modulith's `@Externalized`).

### Process-wide broker handle

`register_broker(Arc<dyn Broker>)` installs a single process-wide broker;
`broker()` returns it as `Option<Arc<dyn Broker>>`. First registration
wins (it returns `false` if one was already set), mirroring the
transaction manager and cache adapter, so a starter / auto-configuration
wires it once at startup and any service or free-function listener can
reach the broker without threading it through.

```rust,ignore
use std::sync::Arc;
use firefly_eda::{register_broker, broker, Broker, InMemoryBroker};

let b: Arc<dyn Broker> = Arc::new(InMemoryBroker::new());
assert!(register_broker(b));        // first wins
assert!(broker().is_some());
```

### Publish a payload to the broker

`publish_to_broker(topic, event_type, source, &payload)` serializes a
`Serialize` value to JSON and publishes it through the process-wide
`broker()`. It errors with `EdaError::BrokerUnavailable` when no broker
is registered. Call it from anywhere — an in-process listener body, a
service method — to push a domain payload onto the broker.
`publish_to_broker_on(&broker, …)` is the explicit-broker form for tests
and multi-broker setups.

```rust,ignore
use serde::Serialize;
use firefly_eda::publish_to_broker;

#[derive(Serialize)]
struct OrderPlaced { id: u32, total: u64 }

publish_to_broker("orders", "order.placed", "orders-svc",
    &OrderPlaced { id: 7, total: 350 }).await?;
```

### Externalize in-process events after commit

`externalize_after_commit::<E>(topic, event_type)` registers an
**after-commit** transactional listener that serializes every in-process
event of type `E` (those published with `firefly::publish_event`) and
forwards it to `topic`. Call it once at startup per externalized event
type; thereafter a plain `publish_event(e)` reaches the broker when —
and only when — the surrounding transaction commits.

Because it rides the after-commit phase, a **rolled-back** transaction
publishes nothing to the broker. With no active transaction the event is
forwarded immediately (the no-transaction fallback). Forwarding is
best-effort: a missing broker or a publish failure *after* commit is
swallowed (it does not unwind the already-committed transaction); use a
real outbox if you need at-least-once delivery.

```rust,ignore
use serde::Serialize;
use firefly_eda::externalize_after_commit;

#[derive(Serialize, Clone)]
struct OrderPlaced { id: u32 }

// once at startup: forward committed OrderPlaced events to the broker
externalize_after_commit::<OrderPlaced>("orders", "order.placed");

// later, inside a transaction:
firefly::publish_event(OrderPlaced { id: 7 }).await; // broker sees it iff the tx commits
```

The full bridge is also re-exported through the facade as
`firefly::eda::{register_broker, broker, publish_to_broker,
externalize_after_commit}`.

## Reactive

An **additive** reactive-streams-style surface layers over
`InMemoryBroker`, built on the [`firefly-reactive`](../reactive) crate's
`Flux<T>` / `Mono<T>`. It is strictly additive: every existing
`Publisher` / `Subscriber` / `InMemoryBroker` signature and wire format
is untouched; the reactive entry points sit alongside.

- `InMemoryBroker::subscribe_reactive(topic) -> EdaResult<Flux<Event>>` —
  the reactive twin of `subscribe_channel`: a `Flux` that emits every
  event delivered to `topic`, so it composes with the whole reactive
  operator set (`take`, `filter`, `map`, `collect_list`, …). Deliveries
  are buffered through a **bounded** channel (default
  `DEFAULT_REACTIVE_BUFFER` = 256); when the downstream consumer falls
  behind and the buffer fills, the newest events are *dropped*
  (`onBackpressureDrop`) rather than blocking or failing the publisher —
  extending the broker's "a slow/gone consumer never fails publishers"
  invariant to the reactive surface. Size the window with
  `subscribe_reactive_with_buffer(topic, n)`. The `Flux` **terminates**
  when the broker is `close()`d (the retained subscription, and thus the
  channel sender, is dropped).
- `Arc<InMemoryBroker>::publish_mono(event) -> Mono<()>` — the **cold**
  reactive publish helper: building the `Mono` does nothing; the publish
  fan-out runs only when the `Mono` is subscribed/awaited. A handler
  error or a closed broker surfaces as the `Mono`'s error signal.

```rust
use std::sync::Arc;
use firefly_eda::{Event, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = Arc::new(InMemoryBroker::new());
let flux = broker.subscribe_reactive("orders.*").unwrap();

broker
    .publish_mono(Event::new("orders.created", "OrderCreated", "svc", None))
    .block()
    .await
    .unwrap();
broker.close().unwrap(); // terminates the Flux

let events = flux.take(1).collect_list().block().await.unwrap().unwrap();
assert_eq!(events[0].topic, "orders.created");
# });
```

## Testing

```bash
cargo test -p firefly-eda
```

Covers in-memory fan-out across multiple subscribers, correlation-id
propagation through `Event::new`, handler-error short-circuit, the
Kafka / RabbitMQ sentinel returns, closed-broker semantics, channel
subscriptions, object safety of the ports, and the stable byte-for-byte
JSON envelope format (including base64 payloads and omission rules). The
reactive surface is covered too: `subscribe_reactive` yielding published
events as a `Flux` (`take(n).collect_list`), backpressure-drop under a
slow consumer, `close()` terminating the `Flux`, the cold `publish_mono`,
and composition with reactive operators.
