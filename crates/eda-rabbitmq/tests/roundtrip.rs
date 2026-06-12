//! End-to-end round-trip against a live RabbitMQ.
//!
//! Ignored by default — it requires a broker reachable at
//! `amqp://guest:guest@localhost:5672/`. Run with:
//!
//! ```sh
//! cargo test -p firefly-eda-rabbitmq -- --ignored
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_rabbitmq::{RabbitMqBroker, RabbitMqBrokerConfig};

#[tokio::test]
#[ignore = "requires rabbitmq"]
async fn publish_consume_round_trip() {
    let cfg = RabbitMqBrokerConfig::default()
        .with_url("amqp://guest:guest@localhost:5672/")
        .with_exchange("firefly-rt-test")
        .with_destinations(["orders"])
        .with_group("firefly-rt");

    let broker = RabbitMqBroker::new(cfg);

    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = seen.clone();
    broker
        .subscribe(
            "order.*",
            handler(move |ev: Event| {
                let seen2 = seen2.clone();
                async move {
                    assert_eq!(ev.event_type, "order.created");
                    seen2.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .await
        .expect("subscribe");

    broker.start().await.expect("start");

    let ev = Event::new(
        "orders",
        "order.created",
        "orders-svc",
        Some(br#"{"id":1}"#.to_vec()),
    );
    broker.publish(ev).await.expect("publish");

    // Give the consumer a moment to deliver (bounded — never long-sleep).
    for _ in 0..20 {
        if seen.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    broker.stop().await.expect("stop");
}
