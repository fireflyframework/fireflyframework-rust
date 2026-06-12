// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Round-trip integration test against a **real** Kafka broker.
//!
//! This exercises the full publish -> consume-group consume path through
//! `librdkafka`, so it needs a live cluster. It is **env-gated**, not
//! `#[ignore]`d: it reads the broker list from `FIREFLY_TEST_KAFKA_BROKERS`
//! (falling back to the older `KAFKA_BROKERS`). When neither is set the test
//! prints a one-line `skipping` notice and returns `Ok` — so `cargo test` on a
//! bare machine stays green — and when set it performs the genuine round-trip.
//!
//! ```text
//! FIREFLY_TEST_KAFKA_BROKERS=localhost:9092 cargo test -p firefly-eda-kafka
//! ```
//!
//! Resource names are unique per test (test fn name + pid + an atomic
//! counter — never `rand`), so runs are idempotent and safe in parallel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_kafka::{KafkaBroker, KafkaConfig};
use tokio::sync::mpsc;

/// Process-wide monotonic counter for unique resource suffixes. Combined
/// with the pid and the test name this yields a collision-free topic /
/// group per test invocation without relying on randomness.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the Kafka broker list, preferring the standard
/// `FIREFLY_TEST_KAFKA_BROKERS` and falling back to the legacy
/// `KAFKA_BROKERS`. Returns `None` when neither is set.
fn brokers_from_env() -> Option<String> {
    std::env::var("FIREFLY_TEST_KAFKA_BROKERS")
        .or_else(|_| std::env::var("KAFKA_BROKERS"))
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Builds a unique resource suffix from `test_name`, the process id, and
/// the atomic counter — deterministic and collision-free, no randomness.
fn unique_suffix(test_name: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{test_name}.{}.{n}", std::process::id())
}

/// Publishes one event and asserts the subscriber receives it back intact
/// through a real broker, via a freshly-named consumer group on a
/// uniquely-named topic.
#[tokio::test]
async fn publish_then_consume_round_trips_the_event() {
    let Some(brokers) = brokers_from_env() else {
        eprintln!(
            "skipping publish_then_consume_round_trips_the_event: \
             FIREFLY_TEST_KAFKA_BROKERS (or KAFKA_BROKERS) not set"
        );
        return;
    };

    let suffix = unique_suffix("publish_then_consume_round_trips_the_event");
    let topic = format!("firefly.eda.kafka.it.{suffix}");

    let broker = KafkaBroker::new(KafkaConfig {
        brokers: brokers
            .split(',')
            .map(str::trim)
            .map(String::from)
            .collect(),
        consumer_group: format!("firefly-it-{suffix}"),
        ..Default::default()
    })
    .expect("construct kafka broker");

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
        .expect("subscribe to topic");

    // Give the consumer a moment to join the group before producing, so the
    // record is not produced into a partition no one is reading yet.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let sent = Event::new(&topic, "ItHappened", "it-svc", Some(b"{\"n\":42}".to_vec()))
        .with_header("tenant", "acme");
    broker.publish(sent.clone()).await.expect("publish event");

    // Bounded wait: a missing message fails (not hangs) within a few seconds.
    let received = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("did not receive event within timeout")
        .expect("subscriber channel closed before delivery");

    assert_eq!(received.id, sent.id);
    assert_eq!(received.event_type, sent.event_type);
    assert_eq!(received.topic, sent.topic);
    assert_eq!(received.payload, sent.payload);
    assert_eq!(received.headers, sent.headers);

    // Close releases the consumer loop and flushes the producer.
    Publisher::close(&broker).await.expect("close broker");
}
