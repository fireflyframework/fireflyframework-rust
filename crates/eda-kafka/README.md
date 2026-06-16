# firefly-eda-kafka

Apache Kafka transport for the Firefly Framework [`firefly-eda`] event-driven
architecture, built on [`rdkafka`] (a binding over the `librdkafka` C
library).

`KafkaBroker` implements the same `Publisher` / `Subscriber` / `Broker`
surfaces as the in-memory broker, so services written against `firefly-eda`
switch to Kafka by swapping the constructor — no handler changes.

## Usage

```rust,no_run
use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_kafka::{new_kafka_broker, KafkaConfig};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let broker = new_kafka_broker(KafkaConfig {
    brokers: vec!["localhost:9092".into()],
    consumer_group: "orders-svc".into(),
    ..Default::default()
})?;

broker
    .subscribe(
        "orders.created",
        handler(|ev: Event| async move {
            println!("got order {}", ev.id);
            Ok(())
        }),
    )
    .await?;

let ev = Event::new("orders.created", "OrderCreated", "orders-svc", None);
broker.publish(ev).await?;
Publisher::close(&*broker).await?;
# Ok(())
# }
```

## Wire format (`Event` ↔ Kafka record)

| Kafka record field | Source |
|--------------------|--------|
| value              | canonical `Event` JSON (`id`/`type`/`source`/`topic`/`correlationId`/`time`/`headers`/`payload`/`key`) |
| key                | `Event.correlation_id`, falling back to `Event.id` |
| topic              | `Event.topic` |
| headers            | every `Event` header copied as a UTF-8 Kafka header |

The consumer deserializes the record value back into an `Event` and dispatches
it to every handler subscribed to that topic. The per-topic consumer loop
isolates per-message failures: a record that fails to deserialize is logged and
skipped, and a handler that returns an error is logged with the loop continuing
— one poison message never stalls the stream.

## Configuration

`KafkaConfig` is field-for-field the shape of `firefly_eda::KafkaConfig` (so the
starter can hand the same config to either the scaffold or this adapter) plus a
`with_property` escape hatch for arbitrary `librdkafka` tuning (`acks`, SASL
credentials, `auto.offset.reset`, …). The consumer defaults to auto-commit
enabled and `auto.offset.reset=earliest`.

## Testing

Unit tests cover config building and the `Event` ↔ record mapping (using
`rdkafka`'s `OwnedMessage`/`OwnedHeaders` directly). The broker round-trip
against a live cluster lives in `tests/kafka_roundtrip.rs` as an **env-gated**
integration test (no `#[ignore]`): it reads `FIREFLY_TEST_KAFKA_BROKERS`
(falling back to the legacy `KAFKA_BROKERS`) and skips with a one-line notice
when unset, so a bare `cargo test` stays green. Point it at a live broker to run
the real produce → consumer-group consume round-trip:

```text
FIREFLY_TEST_KAFKA_BROKERS=localhost:9092 cargo test -p firefly-eda-kafka
```

## Design notes

`KafkaBroker` pairs a producer with a consumer-group loop and per-message error
isolation. Because the `firefly-eda` `Subscriber` port is topic-based,
`KafkaBroker` subscribes by Kafka topic and uses the canonical `Event` JSON
codec directly (Avro / Protobuf are not yet supported).

[`firefly-eda`]: ../eda
[`rdkafka`]: https://docs.rs/rdkafka
