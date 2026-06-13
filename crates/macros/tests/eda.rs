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
