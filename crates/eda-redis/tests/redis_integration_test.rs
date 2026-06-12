//! Env-gated **live** integration tests for
//! [`firefly_eda_redis::RedisStreamsBroker`] against a real Redis.
//!
//! These complement the in-process fake-RESP server tests in
//! `broker_test.rs` (which always run, with no external service) by
//! driving the genuine Redis Streams consumer-group lifecycle end to end:
//! `XGROUP CREATE … MKSTREAM`, `XADD`, `XREADGROUP … BLOCK`, and `XACK`.
//!
//! ## How they gate
//!
//! Each test reads `FIREFLY_TEST_REDIS_URL` (falling back to the older
//! `REDIS_URL`). When **unset**, the test prints a one-line `skipping …`
//! notice and returns — so `cargo test` on a machine with no Redis is
//! green. When **set**, it performs a real round-trip.
//!
//! ## Isolation & cleanup
//!
//! Every test uses a unique stream key and consumer-group name derived
//! from the test function name plus a process-unique atomic counter
//! (never `rand`), so concurrent and repeated runs never collide. Each
//! test `DEL`s the stream it created on the way out.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use firefly_eda::{handler, Event, Publisher};
use firefly_eda_redis::{RedisConfig, RedisStreamsBroker};
use redis::AsyncCommands;
use tokio::sync::mpsc;

/// The standard Firefly integration env var (preferred), with the older
/// `REDIS_URL` accepted as a fallback.
fn redis_url() -> Option<String> {
    std::env::var("FIREFLY_TEST_REDIS_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Process-unique, monotonically increasing counter for collision-free
/// stream / group names — no random source.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A `(stream, group)` pair unique to this test invocation.
fn unique_names(test: &str) -> (String, String) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (
        format!("firefly:it:eda:{test}:{n}:stream"),
        format!("firefly-it-{test}-{n}-grp"),
    )
}

/// Best-effort `DEL` of a stream key via a direct connection, so each run
/// leaves Redis as it found it.
async fn drop_stream(url: &str, stream: &str) {
    if let Ok(client) = redis::Client::open(url) {
        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
            let _: redis::RedisResult<i64> = conn.del(stream).await;
        }
    }
}

/// Counts pending (delivered-but-unacked) entries for `(stream, group)`
/// via `XPENDING <stream> <group>`. Returns `0` when the group/stream is
/// absent. Used to assert the broker actually `XACK`-ed.
async fn pending_count(url: &str, stream: &str, group: &str) -> i64 {
    let client = redis::Client::open(url).expect("open redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("connect redis");
    // XPENDING summary form: [count, min-id, max-id, [[consumer, n], ...]].
    let reply: redis::RedisResult<redis::Value> = redis::cmd("XPENDING")
        .arg(stream)
        .arg(group)
        .query_async(&mut conn)
        .await;
    match reply {
        Ok(redis::Value::Array(items)) => match items.first() {
            Some(redis::Value::Int(n)) => *n,
            _ => 0,
        },
        // NOGROUP (group/stream absent) ⇒ nothing pending.
        _ => 0,
    }
}

fn cfg(url: &str, stream: &str, group: &str) -> RedisConfig {
    RedisConfig::new(url)
        .with_streams([stream.to_string()])
        .with_group(group)
        .with_consumer_id("it-consumer")
        .with_block_ms(50)
        .with_count(10)
}

// ---------------------------------------------------------------------------
// XADD → XREADGROUP → XACK consumer-group round-trip on a unique stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consumer_group_round_trip_delivers_and_acks() {
    let Some(url) = redis_url() else {
        eprintln!("skipping consumer_group_round_trip_delivers_and_acks: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let (stream, group) = unique_names("round_trip");
    // Start clean (a previous crashed run may have left the key behind).
    drop_stream(&url, &stream).await;

    let broker =
        RedisStreamsBroker::connect(cfg(&url, &stream, &group)).expect("connect to real Redis");

    let (tx, mut rx) = mpsc::unbounded_channel();
    broker
        .subscribe(
            &stream,
            handler(move |ev: Event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((ev.event_type.clone(), ev.payload.clone()));
                    Ok(())
                }
            }),
        )
        .await
        .unwrap();
    // XGROUP CREATE … MKSTREAM + spawn the XREADGROUP loop.
    broker.start().await.unwrap();

    // XADD an envelope to the stream named by the event's topic.
    broker
        .publish(Event::new(
            &stream,
            "order.created",
            "it-svc",
            Some(br#"{"id":42}"#.to_vec()),
        ))
        .await
        .unwrap();

    // The consume loop delivers the event to the handler.
    let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("handler should receive the event within 5s")
        .expect("channel open");
    assert_eq!(received.0, "order.created");
    assert_eq!(received.1.as_deref(), Some(&b"{\"id\":42}"[..]));

    // The loop XACKs after the successful handler: nothing stays pending.
    // Poll XPENDING until the ack round-trips (or the deadline elapses).
    let mut pending = pending_count(&url, &stream, &group).await;
    for _ in 0..60 {
        if pending == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        pending = pending_count(&url, &stream, &group).await;
    }
    assert_eq!(pending, 0, "successful delivery must be XACK-ed");

    Publisher::close(&broker).await.unwrap();
    drop_stream(&url, &stream).await;
}

#[tokio::test]
async fn handler_error_leaves_entry_pending() {
    let Some(url) = redis_url() else {
        eprintln!("skipping handler_error_leaves_entry_pending: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let (stream, group) = unique_names("pending");
    drop_stream(&url, &stream).await;

    let broker =
        RedisStreamsBroker::connect(cfg(&url, &stream, &group)).expect("connect to real Redis");

    let (tx, mut rx) = mpsc::unbounded_channel();
    broker
        .subscribe(
            &stream,
            handler(move |_ev: Event| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(());
                    // Erroring leaves the entry unacked (at-least-once).
                    Err(firefly_kernel::FireflyError::internal("boom"))
                }
            }),
        )
        .await
        .unwrap();
    broker.start().await.unwrap();

    broker
        .publish(Event::new(&stream, "order.created", "it-svc", None))
        .await
        .unwrap();

    // Handler ran at least once.
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("handler should run within 5s")
        .expect("channel open");

    // The errored entry is NOT acked ⇒ it stays pending for redelivery.
    let pending = wait_until_pending_ge_1(&url, &stream, &group).await;
    assert!(
        pending >= 1,
        "errored entry must remain pending, got {pending}"
    );

    Publisher::close(&broker).await.unwrap();
    drop_stream(&url, &stream).await;
}

/// Polls `XPENDING` until at least one entry is pending or the deadline
/// elapses; returns the last observed count.
async fn wait_until_pending_ge_1(url: &str, stream: &str, group: &str) -> i64 {
    let mut pending = 0;
    for _ in 0..60 {
        pending = pending_count(url, stream, group).await;
        if pending >= 1 {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pending
}

#[tokio::test]
async fn start_is_busygroup_tolerant_against_real_redis() {
    let Some(url) = redis_url() else {
        eprintln!("skipping start_is_busygroup_tolerant_against_real_redis: FIREFLY_TEST_REDIS_URL (or REDIS_URL) is unset");
        return;
    };
    let (stream, group) = unique_names("busygroup");
    drop_stream(&url, &stream).await;

    // First broker creates the group; a second connect+start against the
    // same stream/group must tolerate the BUSYGROUP reply.
    let b1 = RedisStreamsBroker::connect(cfg(&url, &stream, &group)).expect("connect b1");
    b1.start().await.unwrap();
    b1.start().await.unwrap(); // idempotent on the same broker

    let b2 = RedisStreamsBroker::connect(cfg(&url, &stream, &group)).expect("connect b2");
    // Group already exists ⇒ XGROUP CREATE returns BUSYGROUP, swallowed.
    b2.start().await.unwrap();

    Publisher::close(&b1).await.unwrap();
    Publisher::close(&b2).await.unwrap();
    drop_stream(&url, &stream).await;
}
