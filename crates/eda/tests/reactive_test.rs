//! Tests for the additive reactive (Reactor / WebFlux-style) surface:
//! `InMemoryBroker::subscribe_reactive` (a `Flux<Event>`) and
//! `InMemoryBroker::publish_mono` (a cold `Mono<()>`).

use std::sync::Arc;

use firefly_eda::{Event, InMemoryBroker};

/// `subscribe_reactive` yields published events as a `Flux`, consumed
/// with `take(n).collect_list()` — the Reactor idiom for bounded
/// stream consumption.
#[tokio::test]
async fn subscribe_reactive_yields_published_events() {
    let broker = Arc::new(InMemoryBroker::new());
    let flux = broker
        .subscribe_reactive("orders.created")
        .expect("subscribe_reactive");

    for i in 0..3 {
        broker
            .publish(Event::new(
                "orders.created",
                "OrderCreated",
                "orders-svc",
                Some(format!(r#"{{"id":"o{i}"}}"#).into_bytes()),
            ))
            .await
            .expect("publish");
    }

    // take(3) bounds the otherwise-open stream; collect_list gathers it.
    let events = flux
        .take(3)
        .collect_list()
        .block()
        .await
        .expect("no error")
        .expect("a list");

    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|e| e.topic == "orders.created"));
    assert_eq!(events[0].payload.as_deref(), Some(&b"{\"id\":\"o0\"}"[..]));
}

/// The reactive subscription honors glob topic patterns, exactly as the
/// non-reactive `subscribe`.
#[tokio::test]
async fn subscribe_reactive_matches_glob() {
    let broker = Arc::new(InMemoryBroker::new());
    let flux = broker.subscribe_reactive("orders.*").expect("subscribe");

    broker
        .publish(Event::new("orders.created", "C", "svc", None))
        .await
        .unwrap();
    broker
        .publish(Event::new("payments.created", "P", "svc", None))
        .await
        .unwrap();
    broker
        .publish(Event::new("orders.shipped", "S", "svc", None))
        .await
        .unwrap();
    broker.close().unwrap();

    let events = flux.collect_list().block().await.unwrap().unwrap();
    let topics: Vec<_> = events.iter().map(|e| e.topic.as_str()).collect();
    assert_eq!(topics, vec!["orders.created", "orders.shipped"]);
}

/// A slow subscriber (a tiny buffer that fills before draining) must not
/// fail or block the publisher: excess events are dropped
/// (onBackpressureDrop), and the publisher still succeeds. Verifies the
/// "a slow consumer never fails publishers" invariant on the reactive
/// surface.
#[tokio::test]
async fn subscribe_reactive_backpressure_drops_for_slow_consumer() {
    let broker = Arc::new(InMemoryBroker::new());
    // Buffer of 2: with no concurrent draining, the 3rd+ events overflow
    // and are dropped rather than blocking the publisher.
    let flux = broker
        .subscribe_reactive_with_buffer("metrics", 2)
        .expect("subscribe");

    for i in 0..50 {
        // Every publish must succeed even though the buffer is tiny and
        // nothing is draining yet.
        broker
            .publish(Event::new("metrics", "Tick", "svc", Some(vec![i as u8])))
            .await
            .expect("publish never blocks or fails on a slow consumer");
    }
    broker.close().unwrap();

    // Now drain: we get at most the buffer's worth, never more, never an
    // error — the rest were dropped.
    let events = flux.collect_list().block().await.unwrap().unwrap();
    assert!(
        events.len() <= 2,
        "expected <= buffer (2) buffered events, got {}",
        events.len()
    );
    assert!(!events.is_empty(), "at least the first events buffered");
}

/// `close()` terminates the `Flux`: after close the stream completes, so
/// `collect_list` resolves rather than hanging.
#[tokio::test]
async fn close_terminates_the_flux() {
    let broker = Arc::new(InMemoryBroker::new());
    let flux = broker.subscribe_reactive("orders").expect("subscribe");

    broker
        .publish(Event::new("orders", "O", "svc", None))
        .await
        .unwrap();
    broker.close().unwrap(); // drops the sender → terminates the stream

    // If close did not terminate the Flux this would hang forever; the
    // test harness would time out instead of completing.
    let events = flux.collect_list().block().await.unwrap().unwrap();
    assert_eq!(events.len(), 1);
}

/// `publish_mono` is *cold*: building the `Mono` performs no publish, and
/// the event is only delivered once the `Mono` is subscribed/awaited —
/// the Reactor `Mono<Void>` publish contract.
#[tokio::test]
async fn publish_mono_is_cold_and_delivers_on_subscribe() {
    let broker = Arc::new(InMemoryBroker::new());
    let flux = broker.subscribe_reactive("orders").expect("subscribe");

    // Build but do NOT subscribe the Mono — nothing should be delivered.
    let mono = broker.publish_mono(Event::new("orders", "O", "svc", None));

    // Now drive it: the publish runs and the event reaches the Flux.
    mono.block().await.expect("publish_mono succeeds");
    broker.close().unwrap();

    let n = flux.count().block().await.unwrap();
    assert_eq!(n, Some(1), "exactly the one event published on subscribe");
}

/// `publish_mono`'s error signal surfaces a closed broker as the `Mono`'s
/// error, mapping `EdaError::Closed` → a 409 `FireflyError`.
#[tokio::test]
async fn publish_mono_errors_on_closed_broker() {
    let broker = Arc::new(InMemoryBroker::new());
    broker.close().unwrap();

    let result = broker
        .publish_mono(Event::new("orders", "O", "svc", None))
        .block()
        .await;

    assert!(result.is_err(), "publishing on a closed broker errors");
}

/// The reactive surface composes with the rest of the Reactor operator
/// set (`filter`, `map`) end-to-end.
#[tokio::test]
async fn subscribe_reactive_composes_with_operators() {
    let broker = Arc::new(InMemoryBroker::new());
    let flux = broker.subscribe_reactive("nums").expect("subscribe");

    for i in 0..6u8 {
        broker
            .publish(Event::new("nums", "N", "svc", Some(vec![i])))
            .await
            .unwrap();
    }
    broker.close().unwrap();

    // Keep only even-payload events, project to their first byte.
    let evens = flux
        .filter(|e| e.payload.as_ref().is_some_and(|p| p[0] % 2 == 0))
        .map(|e| e.payload.unwrap()[0])
        .collect_list()
        .block()
        .await
        .unwrap()
        .unwrap();

    assert_eq!(evens, vec![0, 2, 4]);
}
