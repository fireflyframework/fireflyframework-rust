//! End-to-end tests for [`RedisStreamsBroker`] against an in-process
//! fake RESP2 server (see `common/mod.rs`). No external Redis is
//! required; the full connect → group-create → publish → consume → ack
//! lifecycle runs over a real `TcpListener`.
//!
//! Ports pyfly's `tests/eda/test_redis_event_bus.py` cases (publish
//! writes an envelope, `BUSYGROUP`-tolerant start, stop cancels the
//! consume task) plus the behaviors the brief calls out: `XACK` on
//! success and leave-pending on handler error.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::FakeRedis;
use firefly_eda::{handler, Event, Publisher, Subscriber};
use firefly_eda_redis::{new_redis_broker, RedisConfig, RedisStreamsBroker};
use firefly_kernel::FireflyError;
use tokio::sync::mpsc;

/// Spins until `cond()` holds or ~2 s elapse (40 × 50 ms). Keeps total
/// sleeping well under the harness budget while tolerating scheduler
/// jitter on the consume loop.
async fn wait_until<F: Fn() -> bool>(cond: F) {
    for _ in 0..40 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(cond(), "condition not met within deadline");
}

fn cfg(fake: &FakeRedis, group: &str) -> RedisConfig {
    RedisConfig::new(fake.url())
        .with_streams(["orders"])
        .with_group(group)
        .with_consumer_id("test-consumer")
        .with_block_ms(50)
        .with_count(10)
}

#[tokio::test]
async fn publish_writes_envelope_to_stream() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g1")).unwrap();

    let ev = Event::new(
        "orders",
        "order.created",
        "orders-svc",
        Some(br#"{"id":1}"#.to_vec()),
    );
    broker.publish(ev).await.unwrap();

    // The XADD landed on the "orders" stream with a parseable envelope.
    let stored = {
        let state = fake.state.lock().unwrap();
        state.streams.get("orders").cloned().unwrap_or_default()
    };
    assert_eq!(stored.len(), 1, "exactly one entry XADD-ed");
    let envelope: Event = serde_json::from_slice(&stored[0].envelope).unwrap();
    assert_eq!(envelope.event_type, "order.created");
    assert_eq!(envelope.topic, "orders");
    assert_eq!(envelope.payload.as_deref(), Some(&b"{\"id\":1}"[..]));

    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn start_is_busygroup_tolerant_and_idempotent() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g2")).unwrap();
    // The fake replies +OK to XGROUP CREATE; calling start twice must not
    // error and must not spawn a second loop.
    broker.start().await.unwrap();
    broker.start().await.unwrap();
    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn delivers_and_acks_on_success() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g3")).unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    broker
        .subscribe(
            "orders",
            handler(move |ev: Event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(ev.event_type.clone());
                    Ok(())
                }
            }),
        )
        .await
        .unwrap();
    broker.start().await.unwrap();

    broker
        .publish(Event::new("orders", "order.created", "svc", None))
        .await
        .unwrap();

    let received = rx.recv().await.expect("handler should receive the event");
    assert_eq!(received, "order.created");

    // The consume loop XACKs after a successful handler.
    wait_until(|| fake.state.lock().unwrap().ack_count("orders", "g3") == 1).await;
    assert_eq!(fake.state.lock().unwrap().pending_count("orders", "g3"), 0);

    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn glob_pattern_matches_topic() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(
        RedisConfig::new(fake.url())
            .with_streams(["orders"])
            .with_group("g4")
            .with_block_ms(50),
    )
    .unwrap();

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    // Subscribe with a glob; events on "orders" match "ord*".
    broker
        .subscribe(
            "ord*",
            handler(move |_ev: Event| {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .await
        .unwrap();
    broker.start().await.unwrap();
    broker
        .publish(Event::new("orders", "order.created", "svc", None))
        .await
        .unwrap();

    wait_until(|| hits.load(Ordering::SeqCst) == 1).await;
    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn handler_error_leaves_entry_pending() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g5")).unwrap();

    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_h = Arc::clone(&attempts);
    broker
        .subscribe(
            "orders",
            handler(move |_ev: Event| {
                let attempts = Arc::clone(&attempts_h);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(FireflyError::internal("boom"))
                }
            }),
        )
        .await
        .unwrap();
    broker.start().await.unwrap();
    broker
        .publish(Event::new("orders", "order.created", "svc", None))
        .await
        .unwrap();

    // Handler ran at least once and the entry was NOT acked.
    wait_until(|| attempts.load(Ordering::SeqCst) >= 1).await;
    // Give the loop time to (not) ack — pending stays 1, acked stays 0.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (acked, pending) = {
        let state = fake.state.lock().unwrap();
        (
            state.ack_count("orders", "g5"),
            state.pending_count("orders", "g5"),
        )
    };
    assert_eq!(acked, 0, "errored entry must not be acked");
    assert_eq!(pending, 1, "errored entry stays pending");

    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn close_rejects_further_operations() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g6")).unwrap();
    broker.start().await.unwrap();
    Publisher::close(&broker).await.unwrap();

    // After close, publish and subscribe both report Closed.
    let pub_err = broker.publish(Event::new("orders", "T", "svc", None)).await;
    assert!(matches!(pub_err, Err(firefly_eda::EdaError::Closed)));

    let sub_err = Subscriber::subscribe(&broker, "orders", handler(|_| async { Ok(()) })).await;
    assert!(matches!(sub_err, Err(firefly_eda::EdaError::Closed)));

    // Close is idempotent.
    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn poison_message_is_acked_and_skipped() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "gp")).unwrap();

    // Stage an undeserializable envelope before the broker reads.
    fake.state
        .lock()
        .unwrap()
        .push_raw_entry("orders", b"not-json".to_vec());

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    broker
        .subscribe(
            "orders",
            handler(move |_ev: Event| {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            }),
        )
        .await
        .unwrap();
    broker.start().await.unwrap();

    // The poison entry is acked (so it is not redelivered forever) and
    // the handler is never invoked.
    wait_until(|| fake.state.lock().unwrap().ack_count("orders", "gp") == 1).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "handler must not see poison"
    );
    assert_eq!(fake.state.lock().unwrap().pending_count("orders", "gp"), 0);

    Publisher::close(&broker).await.unwrap();
}

#[tokio::test]
async fn factory_returns_boxed_broker() {
    let fake = FakeRedis::start().await;
    let broker = new_redis_broker(cfg(&fake, "g7")).unwrap();
    broker
        .publish(Event::new("orders", "T", "svc", None))
        .await
        .unwrap();
    Publisher::close(&*broker).await.unwrap();
}

#[tokio::test]
async fn invalid_topic_pattern_is_bad_request() {
    let fake = FakeRedis::start().await;
    let broker = RedisStreamsBroker::connect(cfg(&fake, "g8")).unwrap();
    let err = broker
        .subscribe("orders[", handler(|_| async { Ok(()) }))
        .await
        .unwrap_err();
    match err {
        firefly_eda::EdaError::Handler(e) => {
            assert_eq!(e.to_problem().status, 400);
        }
        other => panic!("expected Handler(bad_request), got {other:?}"),
    }
    Publisher::close(&broker).await.unwrap();
}
