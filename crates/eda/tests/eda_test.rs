//! Ported 1:1 from the Go module's `eda_test.go`, plus Rust-specific
//! coverage (object safety, channel subscription, closed-broker
//! semantics, kernel error conversion).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use firefly_eda::{
    handler, new_kafka_broker, new_rabbitmq_broker, wrap_listener, Broker, EdaError, Event,
    InMemoryBroker, KafkaConfig, ListenerPolicy, Publisher, RabbitMqConfig, Subscriber,
    HEADER_EXCEPTION, HEADER_ORIGINAL_TOPIC,
};
use firefly_kernel::{with_correlation_id_sync, FireflyError};

/// Go: `TestInMemoryFanout`.
#[tokio::test]
async fn in_memory_fanout() {
    let broker = InMemoryBroker::new();

    let a_calls = Arc::new(AtomicU32::new(0));
    let b_calls = Arc::new(AtomicU32::new(0));

    let a = Arc::clone(&a_calls);
    broker
        .subscribe(
            "orders.created",
            handler(move |_ev: Event| {
                let a = Arc::clone(&a);
                async move {
                    a.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .expect("subscribe a");

    let b = Arc::clone(&b_calls);
    broker
        .subscribe(
            "orders.created",
            handler(move |_ev: Event| {
                let b = Arc::clone(&b);
                async move {
                    b.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .expect("subscribe b");

    // Go: ctx := kernel.WithCorrelationID(context.Background(), "corr-1")
    let ev = with_correlation_id_sync("corr-1", || {
        Event::new(
            "orders.created",
            "OrderCreated",
            "orders-service",
            Some(br#"{"id":"o1"}"#.to_vec()),
        )
    });
    assert_eq!(ev.correlation_id, "corr-1");

    broker.publish(ev).await.expect("publish");
    assert_eq!(a_calls.load(Ordering::SeqCst), 1);
    assert_eq!(b_calls.load(Ordering::SeqCst), 1);

    broker.close().expect("close");
}

/// Go: `TestInMemoryHandlerError` — the first handler error is returned
/// to the publisher unchanged, and short-circuits remaining handlers.
#[tokio::test]
async fn in_memory_handler_error() {
    let broker = InMemoryBroker::new();

    broker
        .subscribe(
            "t",
            handler(|_ev: Event| async { Err(FireflyError::internal("downstream")) }),
        )
        .unwrap();

    // Rust extra: a later handler must NOT run after the short-circuit.
    let later_calls = Arc::new(AtomicU32::new(0));
    let later = Arc::clone(&later_calls);
    broker
        .subscribe(
            "t",
            handler(move |_ev: Event| {
                let later = Arc::clone(&later);
                async move {
                    later.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    let err = broker
        .publish(Event::new("t", "X", "src", None))
        .await
        .expect_err("publish must fail");
    match err {
        EdaError::Handler(fe) => {
            assert_eq!(fe.detail, "downstream");
            assert_eq!(fe.to_string(), err_display());
        }
        other => panic!("expected Handler error, got: {other}"),
    }
    assert_eq!(later_calls.load(Ordering::SeqCst), 0);

    broker.close().unwrap();
}

fn err_display() -> String {
    FireflyError::internal("downstream").to_string()
}

/// Go: `TestKafkaRabbitPlaceholders`.
#[test]
fn kafka_rabbit_placeholders() {
    let kafka = new_kafka_broker(KafkaConfig::default());
    assert!(matches!(kafka, Err(EdaError::KafkaUnavailable)));

    let rabbit = new_rabbitmq_broker(RabbitMqConfig::default());
    assert!(matches!(rabbit, Err(EdaError::RabbitMqUnavailable)));
}

/// Rust extra: typed configs travel through the scaffolds unchanged.
#[test]
fn scaffold_configs_hold_typed_wiring() {
    let kafka_cfg = KafkaConfig {
        brokers: vec!["kafka:9092".into()],
        client_id: "orders".into(),
        consumer_group: "orders-group".into(),
        tls: true,
        schema_reg_url: "http://registry:8081".into(),
    };
    assert!(new_kafka_broker(kafka_cfg.clone()).is_err());
    assert_eq!(kafka_cfg.brokers, vec!["kafka:9092".to_string()]);

    let rabbit_cfg = RabbitMqConfig {
        url: "amqp://guest:guest@rabbit:5672/".into(),
        exchange: "firefly".into(),
        queue: "orders".into(),
    };
    assert!(new_rabbitmq_broker(rabbit_cfg.clone()).is_err());
    assert_eq!(rabbit_cfg.queue, "orders");
}

/// Rust extra: a closed broker rejects publish and subscribe with
/// `EdaError::Closed` (Go returns `context.Canceled`); `close` stays
/// idempotent.
#[tokio::test]
async fn closed_broker_rejects_operations() {
    let broker = InMemoryBroker::new();
    broker.close().unwrap();
    broker.close().expect("close is idempotent");

    let err = broker
        .publish(Event::new("t", "X", "src", None))
        .await
        .expect_err("publish after close");
    assert!(matches!(err, EdaError::Closed));

    let err = broker
        .subscribe("t", handler(|_ev: Event| async { Ok(()) }))
        .expect_err("subscribe after close");
    assert!(matches!(err, EdaError::Closed));
}

/// Rust extra: events published to other topics do not fan out.
#[tokio::test]
async fn topics_are_isolated() {
    let broker = InMemoryBroker::new();
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    broker
        .subscribe(
            "topic.a",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    broker
        .publish(Event::new("topic.b", "X", "src", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    broker
        .publish(Event::new("topic.a", "X", "src", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Rust extra: the channel-subscription convenience receives every
/// publish; a dropped receiver never fails the publisher.
#[tokio::test]
async fn subscribe_channel_receives_events() {
    let broker = InMemoryBroker::new();
    let mut rx = broker.subscribe_channel("orders.created").unwrap();

    broker
        .publish(Event::new("orders.created", "OrderCreated", "src", None))
        .await
        .unwrap();
    broker
        .publish(Event::new("orders.created", "OrderShipped", "src", None))
        .await
        .unwrap();

    assert_eq!(rx.try_recv().unwrap().event_type, "OrderCreated");
    assert_eq!(rx.try_recv().unwrap().event_type, "OrderShipped");

    drop(rx);
    broker
        .publish(Event::new("orders.created", "OrderClosed", "src", None))
        .await
        .expect("publish survives dropped receiver");
}

/// Rust extra: the ports are object-safe and the blanket `Broker` impl
/// covers the in-memory broker.
#[tokio::test]
async fn broker_is_object_safe() {
    let broker: Arc<dyn Broker> = Arc::new(InMemoryBroker::new());

    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    Subscriber::subscribe(
        &*broker,
        "t",
        handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }),
    )
    .await
    .unwrap();

    Publisher::publish(&*broker, Event::new("t", "X", "src", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    Publisher::close(&*broker).await.unwrap();
    let err = Publisher::publish(&*broker, Event::new("t", "X", "src", None))
        .await
        .expect_err("closed");
    assert!(matches!(err, EdaError::Closed));
}

/// Rust extra: `Event::new` mints a fresh 32-hex id and leaves the
/// correlation id empty outside any correlation scope.
#[test]
fn event_new_stamps_id_and_time() {
    let ev = Event::new("t", "X", "src", None);
    assert_eq!(ev.id.len(), 32);
    assert!(ev.id.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(ev.correlation_id.is_empty());
    assert!(ev.headers.is_empty());
    assert!(ev.payload.is_none());

    let other = Event::new("t", "X", "src", None);
    assert_ne!(ev.id, other.id, "ids must be unique");
}

/// Rust extra: `EdaError` renders into the kernel error family —
/// handler errors pass through unchanged, the sentinels map to 500, a
/// closed broker maps to 409.
#[test]
fn eda_error_converts_to_firefly_error() {
    let fe: FireflyError = EdaError::KafkaUnavailable.into();
    assert_eq!(fe.status, 500);
    assert!(fe.detail.contains("kafka adapter not registered"));

    let fe: FireflyError = EdaError::RabbitMqUnavailable.into();
    assert_eq!(fe.status, 500);
    assert!(fe.detail.contains("rabbitmq adapter not registered"));

    let fe: FireflyError = EdaError::Closed.into();
    assert_eq!(fe.status, 409);
    assert_eq!(fe.detail, "firefly/eda: broker closed");

    let original = FireflyError::not_found("order missing");
    let fe: FireflyError = EdaError::Handler(original).into();
    assert_eq!(fe.status, 404);
    assert_eq!(fe.detail, "order missing");
}

// ---------------------------------------------------------------------
// pyfly-parity surface: glob subscriptions, consumer groups, and the
// retry/dead-letter listener wrapper.
// ---------------------------------------------------------------------

/// pyfly `test_wildcard_subscription`: a `user.*` subscription matches
/// `user.created` / `user.updated` but not `order.created`.
#[tokio::test]
async fn glob_subscription_matches_by_pattern() {
    let broker = InMemoryBroker::new();
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    broker
        .subscribe(
            "user.*",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    broker
        .publish(Event::new("user.created", "X", "s", None))
        .await
        .unwrap();
    broker
        .publish(Event::new("user.updated", "X", "s", None))
        .await
        .unwrap();
    broker
        .publish(Event::new("order.created", "X", "s", None))
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    broker.close().unwrap();
}

/// An exact topic with no glob metacharacters still matches only itself
/// — existing exact-topic subscriptions keep working unchanged.
#[tokio::test]
async fn exact_topic_still_matches_only_itself() {
    let broker = InMemoryBroker::new();
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    broker
        .subscribe(
            "orders.created",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    broker
        .publish(Event::new("orders.shipped", "X", "s", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    broker
        .publish(Event::new("orders.created", "X", "s", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    broker.close().unwrap();
}

/// An invalid glob pattern is rejected at subscribe time.
#[tokio::test]
async fn invalid_glob_pattern_is_rejected() {
    let broker = InMemoryBroker::new();
    let err = broker
        .subscribe("orders.[", handler(|_ev: Event| async { Ok(()) }))
        .expect_err("invalid glob");
    match err {
        EdaError::Handler(fe) => assert_eq!(fe.status, 400),
        other => panic!("expected Handler/400, got {other}"),
    }
}

/// pyfly `test_consumer_group_delivers_once`: within a group exactly one
/// member receives each event.
#[tokio::test]
async fn consumer_group_delivers_once() {
    let broker = InMemoryBroker::new();
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));

    let ac = Arc::clone(&a);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let ac = Arc::clone(&ac);
                async move {
                    ac.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let bc = Arc::clone(&b);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let bc = Arc::clone(&bc);
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    broker
        .publish(Event::new("orders", "X", "s", None))
        .await
        .unwrap();
    assert_eq!(
        a.load(Ordering::SeqCst) + b.load(Ordering::SeqCst),
        1,
        "exactly one group member receives the event"
    );
    broker.close().unwrap();
}

/// Round-robin within a group: two events go to two different members
/// (Rust extra over pyfly's single-delivery assertion).
#[tokio::test]
async fn consumer_group_round_robins_across_members() {
    let broker = InMemoryBroker::new();
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));
    let ac = Arc::clone(&a);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let ac = Arc::clone(&ac);
                async move {
                    ac.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let bc = Arc::clone(&b);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let bc = Arc::clone(&bc);
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    for _ in 0..4 {
        broker
            .publish(Event::new("orders", "X", "s", None))
            .await
            .unwrap();
    }
    assert_eq!(a.load(Ordering::SeqCst), 2);
    assert_eq!(b.load(Ordering::SeqCst), 2);
    broker.close().unwrap();
}

/// Regression: a consumer group spanning two topics with differently
/// sized matching sets must keep an independent round-robin cursor per
/// topic, so dispatches to one topic do not perturb the modulo base used
/// for the other (pyfly keys its cursor by the `(topic, group)` pair —
/// `messaging/adapters/memory.py`). Before the fix the cursor was keyed
/// by group alone: interleaving `orders`/`payments` left handler B on
/// `orders` starved (A=4, B=0 over four `orders` events instead of 2/2).
#[tokio::test]
async fn consumer_group_cursor_is_per_topic_not_starved_across_topics() {
    let broker = InMemoryBroker::new();
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));
    let c = Arc::new(AtomicU32::new(0));

    // Group `workers` has TWO members on `orders` ...
    let ac = Arc::clone(&a);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let ac = Arc::clone(&ac);
                async move {
                    ac.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let bc = Arc::clone(&b);
    broker
        .subscribe_group(
            "orders",
            "workers",
            handler(move |_ev: Event| {
                let bc = Arc::clone(&bc);
                async move {
                    bc.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    // ... and ONE member on `payments` (a different-sized matching set).
    let cc = Arc::clone(&c);
    broker
        .subscribe_group(
            "payments",
            "workers",
            handler(move |_ev: Event| {
                let cc = Arc::clone(&cc);
                async move {
                    cc.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    // Interleave orders/payments four times each.
    for _ in 0..4 {
        broker
            .publish(Event::new("orders", "X", "s", None))
            .await
            .unwrap();
        broker
            .publish(Event::new("payments", "Y", "s", None))
            .await
            .unwrap();
    }

    // The `orders` events must split fairly between A and B (2/2), and
    // every `payments` event hits the sole member C.
    assert_eq!(a.load(Ordering::SeqCst), 2, "handler A on orders");
    assert_eq!(b.load(Ordering::SeqCst), 2, "handler B on orders");
    assert_eq!(c.load(Ordering::SeqCst), 4, "handler C on payments");
    broker.close().unwrap();
}

/// Distinct groups each receive their own copy of an event; ungrouped
/// subscribers also receive it independently.
#[tokio::test]
async fn distinct_groups_and_ungrouped_each_receive() {
    let broker = InMemoryBroker::new();
    let g1 = Arc::new(AtomicU32::new(0));
    let g2 = Arc::new(AtomicU32::new(0));
    let plain = Arc::new(AtomicU32::new(0));

    let c = Arc::clone(&g1);
    broker
        .subscribe_group(
            "orders",
            "billing",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let c = Arc::clone(&g2);
    broker
        .subscribe_group(
            "orders",
            "shipping",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();
    let c = Arc::clone(&plain);
    broker
        .subscribe(
            "orders",
            handler(move |_ev: Event| {
                let c = Arc::clone(&c);
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .unwrap();

    broker
        .publish(Event::new("orders", "X", "s", None))
        .await
        .unwrap();
    assert_eq!(g1.load(Ordering::SeqCst), 1);
    assert_eq!(g2.load(Ordering::SeqCst), 1);
    assert_eq!(plain.load(Ordering::SeqCst), 1);
    broker.close().unwrap();
}

/// `subscribe_group` is reachable through `dyn Broker` and groups apply
/// across the object-safe surface too.
#[tokio::test]
async fn subscribe_group_through_dyn_broker() {
    let broker: Arc<dyn Broker> = Arc::new(InMemoryBroker::new());
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    Subscriber::subscribe_group(
        &*broker,
        "t",
        "g",
        handler(move |_ev: Event| {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }),
    )
    .await
    .unwrap();
    Publisher::publish(&*broker, Event::new("t", "X", "s", None))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Publisher::close(&*broker).await.unwrap();
}

/// The retry/DLQ wrapper routes an exhausted event to the dead-letter
/// topic with the `x-original-topic` / `x-exception` diagnostic headers,
/// observable end-to-end through the in-memory broker.
#[tokio::test]
async fn wrap_listener_routes_to_dlq_end_to_end() {
    let broker = Arc::new(InMemoryBroker::new());

    // Capture DLQ deliveries on the same broker.
    let mut dlq_rx = broker.subscribe_channel("orders.DLT").unwrap();

    let inner = handler(|_ev: Event| async { Err(FireflyError::validation("bad order")) });
    let wrapped = wrap_listener(
        inner,
        broker.clone(),
        ListenerPolicy::with_retries(1).dead_letter_topic("orders.DLT"),
    );
    broker.subscribe("orders", wrapped).unwrap();

    let ev =
        Event::new("orders", "OrderPlaced", "svc", Some(b"body".to_vec())).with_key(b"pk".to_vec());
    broker.publish(ev).await.expect("publish must not fail");

    let dead = dlq_rx.try_recv().expect("event routed to DLQ");
    assert_eq!(dead.topic, "orders.DLT");
    assert_eq!(dead.payload.as_deref(), Some(&b"body"[..]));
    assert_eq!(dead.key.as_deref(), Some(&b"pk"[..]));
    assert_eq!(
        dead.headers.get(HEADER_ORIGINAL_TOPIC).map(String::as_str),
        Some("orders")
    );
    assert_eq!(
        dead.headers.get(HEADER_EXCEPTION).map(String::as_str),
        Some(FireflyError::validation("x").code.as_str())
    );
    broker.close().unwrap();
}
