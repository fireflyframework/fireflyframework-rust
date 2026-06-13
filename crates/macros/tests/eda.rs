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

//! Behavioral test for `#[event_listener]`: the generated `subscribe_<fn>`
//! helper subscribes the annotated fn to a topic on a real `InMemoryBroker`,
//! and a published event reaches it.

use std::sync::atomic::{AtomicU32, Ordering};

use firefly::eda::{Event, InMemoryBroker};
use firefly::event_listener;
use firefly::kernel::FireflyResult;

static DELIVERED: AtomicU32 = AtomicU32::new(0);

#[event_listener("orders.created")]
async fn on_order_created(ev: Event) -> FireflyResult<()> {
    assert_eq!(ev.topic, "orders.created");
    DELIVERED.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[tokio::test]
async fn event_listener_subscribes_and_receives() {
    let broker = InMemoryBroker::new();

    // The generated helper wires the listener onto the broker.
    subscribe_on_order_created(&broker)
        .await
        .expect("subscribe");

    let ev = Event::new(
        "orders.created",
        "OrderCreated",
        "test",
        Some(b"{}".to_vec()),
    );
    broker.publish(ev).await.expect("publish");

    // The in-memory broker delivers synchronously on publish.
    assert_eq!(
        DELIVERED.load(Ordering::SeqCst),
        1,
        "the listener should have received exactly one event"
    );
}

// ===========================================================================
// Regression: a positional topic must NOT discard a named `group`.
//
// `#[event_listener("t.evt", group = "g1")]` used to be parsed by darling's
// `from_list`, which errors on the positional literal; the previous code
// swallowed that error with `unwrap_or_default()`, dropping the `group`
// entirely and emitting a plain `subscribe(...)` (fan-out) instead of a
// `subscribe_group(...)` (competing-consumer). Two same-group members must
// therefore receive exactly ONE delivery in total, not two.
// ===========================================================================

static GROUP_DELIVERED: AtomicU32 = AtomicU32::new(0);

#[event_listener("billing.charged", group = "settlement")]
async fn on_charged_a(ev: Event) -> FireflyResult<()> {
    assert_eq!(ev.topic, "billing.charged");
    GROUP_DELIVERED.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[event_listener("billing.charged", group = "settlement")]
async fn on_charged_b(ev: Event) -> FireflyResult<()> {
    assert_eq!(ev.topic, "billing.charged");
    GROUP_DELIVERED.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[tokio::test]
async fn event_listener_positional_topic_preserves_group() {
    let broker = InMemoryBroker::new();

    // Both helpers join the SAME consumer group via a positional topic.
    subscribe_on_charged_a(&broker).await.expect("subscribe a");
    subscribe_on_charged_b(&broker).await.expect("subscribe b");

    let ev = Event::new("billing.charged", "Charged", "test", Some(b"{}".to_vec()));
    broker.publish(ev).await.expect("publish");

    // Competing-consumer delivery: exactly one of the two group members fires.
    // Before the fix the `group` was dropped, both subscribed ungrouped, and
    // the count was 2 (fan-out).
    assert_eq!(
        GROUP_DELIVERED.load(Ordering::SeqCst),
        1,
        "a named group on a positional-topic listener must give competing-consumer \
         (one delivery), not fan-out (two)"
    );
}
