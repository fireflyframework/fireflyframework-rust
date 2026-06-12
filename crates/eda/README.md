# `firefly-eda`

> **Tier:** Platform · **Status:** Partial (in-memory full; Kafka/RabbitMQ scaffolds) · **Java original:** `firefly-common-eda` · **Go module:** `eda`

## Overview

`firefly-eda` is the framework's **event-driven architecture port**. It
defines the `Event` envelope every Firefly event flows through, the
`Publisher` / `Subscriber` / `Broker` ports, and an in-process fan-out
`InMemoryBroker`. Production transports — Kafka and RabbitMQ — share
the same ports and slot in via `new_kafka_broker(cfg)` /
`new_rabbitmq_broker(cfg)` once the dedicated transport crates ship.

Until those land, `new_kafka_broker` and `new_rabbitmq_broker` return
the typed sentinels `EdaError::KafkaUnavailable` /
`EdaError::RabbitMqUnavailable` so a misconfigured deployment fails
loud at startup rather than silently falling back to in-memory.

`Event` is wire-compatible with the Java/.NET/Go/Python ports: the same
JSON field names (`id`, `type`, `source`, `topic`, `correlationId`,
`time`, `headers`, `payload`), the same omission rules (`correlationId`
and `headers` are dropped when empty), and the same `payload` encoding
(standard base64, `null` when absent — Go's `[]byte`).

## Design notes

- **Synchronous fan-out.** `InMemoryBroker::publish` awaits each
  subscribed handler sequentially on the publisher's task — the Rust
  analog of the Go broker invoking handlers in the publisher's
  goroutine. The first handler error short-circuits dispatch and is
  returned to the publisher unchanged (wrapped transparently in
  `EdaError::Handler`), matching the Java/.NET semantics.
- **Closed means closed.** After `close()`, publish and subscribe fail
  with `EdaError::Closed` (the Go broker returns `context.Canceled`);
  `close` itself stays idempotent.
- **Correlation propagation.** `Event::new` stamps `correlationId` from
  the kernel's task-local correlation scope
  (`firefly_kernel::with_correlation_id`) — the Rust analog of Go's
  `NewEvent(ctx, …)` reading `kernel.CorrelationIDFrom(ctx)`.
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

## pyfly parity

`firefly-eda` mirrors the abstraction-layer surface of pyfly's
`messaging` and `eda` packages (the transports themselves live in the
dedicated `firefly-eda-kafka` / `firefly-eda-rabbitmq` crates):

### Partition / routing key on `Event`

`Event` carries an optional `key: Option<Vec<u8>>` — pyfly's
`Message.key`, the value brokers use for Kafka partitioning and
RabbitMQ routing. It serializes as a standard-base64 string and is
**omitted** from the wire when absent (unlike `payload`, which encodes
`null`), so events produced before the field existed stay byte-for-byte
identical and cross-port wire-compatible. Set it with `Event::with_key`:

```rust
use firefly_eda::Event;
let ev = Event::new("orders", "OrderPlaced", "svc", Some(b"{}".to_vec()))
    .with_key(b"customer-42".to_vec());
```

### Glob topic patterns

`subscribe(topic, …)` treats `topic` as a glob pattern (`*`, `?`,
`[..]`, `{a,b}`); a published event is delivered to a subscription when
the event's `topic` matches. This is pyfly's `fnmatch`-based event-type
dispatch (`bus.subscribe("user.*", …)` matches `user.created`). A
pattern with no glob metacharacters matches only its literal, so exact
subscriptions behave exactly as before. An invalid pattern is rejected
at subscribe time with a `400` `EdaError::Handler`.

### Consumer groups (round-robin)

`subscribe_group(topic, group, handler)` adds a `Subscriber` member to a
consumer `group`. Within a group each matching event goes to exactly
**one** member, chosen round-robin via a per-group `AtomicUsize` cursor;
distinct groups — and ungrouped subscriptions — each receive their own
copy. The trait default delegates to `subscribe` (correct for transports
whose broker enforces group delivery natively); `InMemoryBroker`
overrides it to implement competing-consumer delivery in-process. This
is pyfly's `subscribe(topic, handler, group=…)`.

### Retry + dead-letter listener wrapper

`wrap_listener(handler, publisher, ListenerPolicy { retries, retry_delay,
dead_letter_topic })` is the adapter-agnostic retry/DLQ wrapper — pyfly's
`messaging.wrap_listener` and Spring Kafka's `@RetryableTopic`. A failing
delivery is retried up to `retries` times with **linear backoff**
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

## Testing

```bash
cargo test -p firefly-eda
```

Covers in-memory fan-out across multiple subscribers, correlation-id
propagation through `Event::new`, handler-error short-circuit, the
Kafka / RabbitMQ sentinel returns, closed-broker semantics, channel
subscriptions, object safety of the ports, and byte-for-byte JSON
parity with the Go envelope (including base64 payloads and omission
rules).
