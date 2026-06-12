# Event-Driven Architecture & Messaging

`firefly-eda` is the framework's **event-driven architecture port**. It defines
the `Event` envelope every Firefly event flows through, the
`Publisher`/`Subscriber`/`Broker` ports, an in-process `InMemoryBroker`, and the
messaging machinery — glob topics, consumer groups, retry/DLQ, event filters,
and a reactive `Flux` subscription surface. The production transports (Kafka,
RabbitMQ, Postgres outbox, Redis Streams) implement the same ports and slot in
at wiring time.

> **Spring parity** — The `Broker` port is the Spring Cloud Stream binder
> abstraction. `wrap_listener`'s retry/DLQ is `@RetryableTopic`; the glob
> subscription is `bus.subscribe("user.*", …)`.

## The `Event` envelope

`Event` is wire-compatible across the Java/.NET/Go/Python/Rust ports — the same
JSON field names and omission rules. Construct one with `Event::new`, which also
stamps `correlationId` from the kernel's task-local correlation scope:

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

## The in-process broker

`InMemoryBroker` is the default — fan-out delivery, glob topic matching, and
per-`(topic, group)` round-robin, with no external dependency. Subscribe a
handler, publish an event:

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

    let ev = Event::new("orders.created", "OrderCreated", "orders-svc",
        Some(br#"{"id":"o1"}"#.to_vec()));
    broker.publish(ev).await.unwrap();
    broker.close().unwrap();
}
```

`InMemoryBroker::publish` awaits each subscribed handler sequentially on the
publisher's task; the first handler error short-circuits and is returned to the
publisher. After `close()`, publish and subscribe fail with `EdaError::Closed`.

## Glob topics and consumer groups

A subscription topic is a glob pattern (`*`, `?`, `[..]`, `{a,b}`); a published
event is delivered to every subscription whose pattern matches its topic:

```rust,ignore
broker.subscribe("orders.*", handler(|ev| async move { Ok(()) })).unwrap();
// matches orders.created, orders.shipped, ...
```

Consumer groups give competing-consumer delivery: within a group each matching
event goes to exactly **one** member (round-robin); distinct groups each get
their own copy:

```rust,ignore
broker.subscribe_group("orders.*", "fulfillment", handler1).unwrap();
broker.subscribe_group("orders.*", "fulfillment", handler2).unwrap();
// each orders.* event reaches exactly one of handler1/handler2
```

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
        .dead_letter_topic("orders.DLT"),
);
broker.subscribe("orders", wrapped).unwrap();
# });
```

For an inspectable record of failures (rather than a routing topic), wire an
`EdaDeadLetterStore` via `ListenerPolicy::dead_letter_store`: an exhausted event
is captured into the store (queryable with `list` / `get` / `remove`).

## Event filters

`EventFilter` is a per-envelope delivery gate layered over topic matching. Where
the broker decides *which* subscriptions a topic reaches, a filter decides
whether a reached subscription actually *runs*. Two ship — a header regex filter
and an arbitrary predicate filter:

```rust
use firefly_eda::{handler, with_filters, Event, HeaderEventFilter, InMemoryBroker};

# tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
let broker = InMemoryBroker::new();
let inner = handler(|_ev: Event| async { Ok(()) });
let gated = with_filters(inner, [HeaderEventFilter::new("x-tenant", r"^acme-.+$").unwrap()]);
broker.subscribe("orders", gated).unwrap();
# });
```

An event must pass *every* filter to be delivered; a non-matching event is
dropped before the handler body runs.

## The reactive subscription surface

`InMemoryBroker::subscribe_reactive(topic)` is the reactive twin of
`subscribe_channel` — a `Flux<Event>` that emits every event delivered to the
topic, composing with the whole Reactor operator set. `publish_mono(event)` is
the cold reactive publish (nothing happens until the `Mono` is subscribed):

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

Deliveries are buffered through a bounded channel; when the downstream consumer
falls behind, the newest events are dropped (`onBackpressureDrop`) rather than
blocking or failing the publisher — extending "a slow consumer never fails
publishers" to the reactive surface.

## Production transports

Each transport crate implements the same `Broker` port; swap the constructor and
keep every handler. Code against `firefly_eda::Broker` and select the adapter at
wiring time.

| Crate                  | Backend         | Constructor                                  |
|------------------------|-----------------|----------------------------------------------|
| `firefly-eda-kafka`    | Apache Kafka    | `new_kafka_broker(KafkaConfig)?`             |
| `firefly-eda-rabbitmq` | RabbitMQ        | `RabbitMqBroker::new(RabbitMqBrokerConfig)`  |
| `firefly-eda-postgres` | Postgres outbox | `PostgresBroker::new(PostgresConfig::new(dsn))` |
| `firefly-eda-redis`    | Redis Streams   | `RedisStreamsBroker::connect(RedisConfig::new(url))?` |

Kafka, for example:

```rust,no_run
use firefly_eda::{handler, Event};
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};

# async fn ex() -> firefly_eda::EdaResult<()> {
let broker = new_kafka_broker(KafkaConfig {
    brokers: vec!["kafka:9092".into()],
    client_id: "orders".into(),
    consumer_group: "orders-svc".into(),
    ..Default::default()
})?;

broker
    .subscribe("orders.created", handler(|ev: Event| async move {
        println!("got order {}", ev.id);
        Ok(())
    }))
    .await?;

let ev = Event::new("orders.created", "OrderCreated", "orders-svc", None);
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
        .with_streams(["orders"])
        .with_group("orders-svc"),
)?;
broker.subscribe("orders.*", handler(|ev: Event| async move {
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

## Broker health

`EventPublisherHealthIndicator` adapts any broker implementing the
`BrokerHealth` ping probe to a `firefly_observability::Indicator`, surfacing
broker liveness on `/actuator/health` under the `eventPublisher` id.

For durable, replayable event history (rather than transient pub/sub), the next
chapter covers [Event Sourcing](./11-event-sourcing.md).
