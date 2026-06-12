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
pub struct Event {            // JSON: id/type/source/topic/correlationId/time/headers/payload
    pub id: String,
    pub event_type: String,   // serialized as "type"
    pub source: String,
    pub topic: String,
    pub correlation_id: String,
    pub time: DateTime<Utc>,
    pub headers: BTreeMap<String, String>,
    pub payload: Option<Vec<u8>>,
}
impl Event { fn new(topic, event_type, source, payload) -> Event; fn with_header(key, value) -> Event }

pub type Handler = Arc<dyn Fn(Event) -> HandlerFuture + Send + Sync>;
pub fn handler(f) -> Handler;                 // wrap an async closure

#[async_trait] pub trait Publisher  { async fn publish(&self, ev: Event) -> EdaResult<()>; async fn close(&self) -> EdaResult<()>; }
#[async_trait] pub trait Subscriber { async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()>; async fn close(&self) -> EdaResult<()>; }
pub trait Broker: Publisher + Subscriber {}   // blanket-implemented

pub struct InMemoryBroker;                    // fan-out broker, sequential handler invocation
pub fn new_kafka_broker(KafkaConfig) -> EdaResult<Box<dyn Broker>>;       // sentinel until wired
pub fn new_rabbitmq_broker(RabbitMqConfig) -> EdaResult<Box<dyn Broker>>; // sentinel until wired

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
