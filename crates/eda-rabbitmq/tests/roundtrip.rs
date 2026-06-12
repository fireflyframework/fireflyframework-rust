//! End-to-end round-trip against a **real** RabbitMQ broker.
//!
//! This declares a durable `direct` exchange and a bound queue, publishes
//! with publisher confirms, consumes with manual ack, and asserts the
//! handler observed the event — the full topology the adapter builds. It is
//! **env-gated**, not `#[ignore]`d: it reads the AMQP URL from
//! `FIREFLY_TEST_RABBITMQ_URL` (falling back to the older `RABBITMQ_URL` then
//! `AMQP_URL`). When none is set the test prints a one-line `skipping` notice
//! and returns — so `cargo test` on a bare machine stays green — and when set
//! it performs the genuine declare -> publish -> consume -> ack round-trip.
//!
//! ```sh
//! FIREFLY_TEST_RABBITMQ_URL=amqp://guest:guest@localhost:5672/%2f \
//!   cargo test -p firefly-eda-rabbitmq
//! ```
//!
//! Exchange / queue / group names are unique per test (test fn name + pid +
//! an atomic counter — never `rand`), and every created resource is torn
//! down on `stop`, so runs are idempotent and safe in parallel.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_rabbitmq::{RabbitMqBroker, RabbitMqBrokerConfig};

/// Process-wide monotonic counter for unique resource suffixes. Combined
/// with the pid and the test name this yields collision-free exchange /
/// queue / group names per test invocation without relying on randomness.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the AMQP URL, preferring the standard `FIREFLY_TEST_RABBITMQ_URL`
/// and falling back to the legacy `RABBITMQ_URL` then `AMQP_URL`. Returns
/// `None` when none is set.
fn url_from_env() -> Option<String> {
    std::env::var("FIREFLY_TEST_RABBITMQ_URL")
        .or_else(|_| std::env::var("RABBITMQ_URL"))
        .or_else(|_| std::env::var("AMQP_URL"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Builds a unique resource suffix from `test_name`, the process id, and
/// the atomic counter — deterministic and collision-free, no randomness.
fn unique_suffix(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{test_name}.{}.{n}", std::process::id())
}

/// Declares a fresh exchange + bound queue, publishes one event, and
/// asserts the manual-ack consumer dispatched it to the subscribed handler.
#[tokio::test]
async fn publish_consume_round_trip() {
    let Some(url) = url_from_env() else {
        eprintln!(
            "skipping publish_consume_round_trip: \
             FIREFLY_TEST_RABBITMQ_URL (or RABBITMQ_URL/AMQP_URL) not set"
        );
        return;
    };

    let suffix = unique_suffix("publish_consume_round_trip");
    let exchange = format!("firefly-rt-test.{suffix}");
    // The destination doubles as the AMQP routing key and the event topic.
    let destination = format!("orders.{suffix}");
    let group = format!("firefly-rt.{suffix}");

    let cfg = RabbitMqBrokerConfig::default()
        .with_url(url)
        .with_exchange(&exchange)
        .with_destinations([destination.clone()])
        .with_group(&group);

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
        .expect("subscribe pattern");

    broker.start().await.expect("start (declare + consume)");

    // Topic == destination == routing key, so the message lands in our queue.
    let ev = Event::new(
        &destination,
        "order.created",
        "orders-svc",
        Some(br#"{"id":1}"#.to_vec()),
    );
    broker.publish(ev).await.expect("publish (with confirm)");

    // Bounded wait: a missing message fails (not hangs) within a few seconds.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while seen.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "consumer did not deliver the published event within the timeout"
    );

    // Tear down the connection and abort the consumer tasks.
    broker.stop().await.expect("stop");
}
