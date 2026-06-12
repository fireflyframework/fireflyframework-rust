//! Round-trip integration test against a real Kafka broker.
//!
//! This exercises the full publish -> consume path through
//! `librdkafka` and therefore needs a Kafka cluster reachable at
//! `localhost:9092` (override with the `KAFKA_BROKERS` env var). It is
//! `#[ignore]` by default so `cargo test` stays green on a bare machine;
//! run it explicitly against a live broker with:
//!
//! ```text
//! KAFKA_BROKERS=localhost:9092 cargo test -p firefly-eda-kafka -- --ignored
//! ```

use std::time::Duration;

use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_kafka::{KafkaBroker, KafkaConfig};
use tokio::sync::mpsc;

/// Publishes one event and asserts the subscriber receives it back
/// intact through a real broker.
#[tokio::test]
#[ignore = "requires kafka"]
async fn publish_then_consume_round_trips_the_event() {
    let brokers = std::env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".into());
    // A unique topic + group per run avoids cross-test offset bleed.
    let suffix = firefly_eda::Event::new("x", "x", "x", None).id;
    let topic = format!("firefly.eda.kafka.it.{suffix}");

    let broker = KafkaBroker::new(KafkaConfig {
        brokers: brokers.split(',').map(String::from).collect(),
        consumer_group: format!("firefly-it-{suffix}"),
        ..Default::default()
    })
    .expect("broker");

    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
    broker
        .subscribe(
            &topic,
            handler(move |ev: Event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(ev);
                    Ok(())
                }
            }),
        )
        .await
        .expect("subscribe");

    // Give the consumer a moment to join the group before producing.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut sent = Event::new(&topic, "ItHappened", "it-svc", Some(b"{\"n\":42}".to_vec()));
    sent = sent.with_header("tenant", "acme");
    broker.publish(sent.clone()).await.expect("publish");

    let received = tokio::time::timeout(Duration::from_secs(15), rx.recv())
        .await
        .expect("did not receive event in time")
        .expect("channel closed");

    assert_eq!(received.id, sent.id);
    assert_eq!(received.event_type, sent.event_type);
    assert_eq!(received.topic, sent.topic);
    assert_eq!(received.payload, sent.payload);
    assert_eq!(received.headers, sent.headers);

    Publisher::close(&broker).await.expect("close");
}
