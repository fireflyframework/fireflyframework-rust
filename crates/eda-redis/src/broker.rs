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

//! The Redis Streams broker — `XGROUP CREATE MKSTREAM`, `XADD` publish,
//! `XREADGROUP` consume loop, `XACK` on success.

use std::sync::Arc;

use async_trait::async_trait;
use firefly_eda::{Broker, EdaError, EdaResult, Event, Handler, Publisher, Subscriber};
use firefly_kernel::FireflyError;
use globset::{Glob, GlobMatcher};
use redis::aio::MultiplexedConnection;
use redis::streams::{StreamReadOptions, StreamReadReply};
use redis::{AsyncCommands, Client, Value};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::RedisConfig;

/// The stream record field holding the serialized [`Event`] envelope —
/// pyfly stores entries as `{envelope: <bytes>}` so the wire format is
/// independent of any field-naming convention. Kept identical for
/// cross-port compatibility.
const ENVELOPE_FIELD: &str = "envelope";

/// A registered subscription: a compiled glob matcher over the event
/// topic plus the delivery [`Handler`].
struct Subscription {
    matcher: GlobMatcher,
    handler: Handler,
}

/// Shared mutable state, held behind `Arc` so the background consume
/// task and the public API observe the same handler list and closed
/// flag.
struct Shared {
    subscriptions: RwLock<Vec<Subscription>>,
    closed: std::sync::atomic::AtomicBool,
}

/// An [`EventPublisher`](firefly_eda::Publisher) /
/// [`Subscriber`](firefly_eda::Subscriber) backed by Redis Streams and
/// consumer groups — the Rust port of pyfly's `RedisStreamsEventBus`.
///
/// Each event's `topic` is the stream key it is `XADD`-ed to. The
/// consumer reads its configured streams via `XREADGROUP` against the
/// configured group, dispatches every entry to the handlers whose topic
/// pattern matches, `XACK`s on success, and — crucially — **leaves the
/// entry pending** (unacked) when a handler errors, so Redis redelivers
/// it to the group later. This is the at-least-once contract pyfly
/// implements by skipping the `XACK` on handler failure.
///
/// # Lifecycle
///
/// 1. [`start`](RedisStreamsBroker::start) issues `XGROUP CREATE …
///    MKSTREAM` for each configured stream (tolerating the `BUSYGROUP`
///    error a pre-existing group raises) and spawns the consume loop.
///    [`publish`](RedisStreamsBroker::publish) auto-starts the broker on
///    first use, like pyfly.
/// 2. The consume loop long-polls with `XREADGROUP … BLOCK <block_ms>`
///    and dispatches each batch.
/// 3. [`close`](Publisher::close) cancels the loop and marks the broker
///    closed; further publishes / subscribes return
///    [`EdaError::Closed`].
///
/// # Topic vs. event-type dispatch
///
/// pyfly matches handler patterns against the envelope's `event_type`;
/// the Rust port matches against the envelope's `topic`, consistent with
/// [`InMemoryBroker`](firefly_eda::InMemoryBroker) and the
/// [`Subscriber`] port contract across every Firefly transport. Glob
/// patterns (`*`, `?`, `[..]`, `{a,b}`) are honored via `globset`,
/// matching pyfly's `fnmatch` semantics. Set the event `topic` to the
/// value you would have matched on in pyfly.
pub struct RedisStreamsBroker {
    config: RedisConfig,
    client: Client,
    /// Connection used by [`publish`](RedisStreamsBroker::publish);
    /// lazily established on first use and reused thereafter.
    publish_conn: Mutex<Option<MultiplexedConnection>>,
    shared: Arc<Shared>,
    consume_task: Mutex<Option<JoinHandle<()>>>,
}

impl RedisStreamsBroker {
    /// Connects to Redis using `config` and returns an idle broker. The
    /// consume loop is not started until
    /// [`start`](RedisStreamsBroker::start) (or the first
    /// [`publish`](RedisStreamsBroker::publish)) runs, mirroring pyfly's
    /// lazy `start()`.
    ///
    /// Returns an error only if the URL is malformed; the TCP connection
    /// itself is established lazily on first use.
    pub fn connect(config: RedisConfig) -> EdaResult<Self> {
        let client = Client::open(config.url.as_str()).map_err(transport_err)?;
        Ok(Self {
            config,
            client,
            publish_conn: Mutex::new(None),
            shared: Arc::new(Shared {
                subscriptions: RwLock::new(Vec::new()),
                closed: std::sync::atomic::AtomicBool::new(false),
            }),
            consume_task: Mutex::new(None),
        })
    }

    /// Registers `h` for every event whose `topic` matches `topic` (an
    /// exact name or a glob pattern). Equivalent to pyfly's
    /// `subscribe(event_type_pattern, handler)`.
    ///
    /// Returns [`EdaError::Handler`] wrapping a `400` when `topic` is not
    /// a valid glob pattern, or [`EdaError::Closed`] when the broker has
    /// been closed.
    pub async fn subscribe(&self, topic: impl Into<String>, h: Handler) -> EdaResult<()> {
        self.ensure_open()?;
        let topic = topic.into();
        let matcher = Glob::new(&topic)
            .map_err(|e| {
                EdaError::Handler(FireflyError::bad_request(format!(
                    "firefly/eda-redis: invalid topic pattern {topic:?}: {e}"
                )))
            })?
            .compile_matcher();
        self.shared.subscriptions.write().await.push(Subscription {
            matcher,
            handler: h,
        });
        Ok(())
    }

    /// Publishes `ev` by `XADD`-ing `{envelope: <json>}` to the stream
    /// named by `ev.topic`. Auto-starts the consume loop on first call,
    /// like pyfly.
    pub async fn publish(&self, ev: Event) -> EdaResult<()> {
        self.ensure_open()?;
        // pyfly auto-starts the bus on first publish so events produced
        // before any explicit start() are still consumed.
        self.start().await?;

        let body = serde_json::to_vec(&ev).map_err(|e| {
            EdaError::Handler(FireflyError::internal(format!(
                "firefly/eda-redis: serialize event: {e}"
            )))
        })?;

        let mut guard = self.publish_conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.new_connection().await?);
        }
        let conn = guard.as_mut().expect("connection just established");
        let _: String = conn
            .xadd(&ev.topic, "*", &[(ENVELOPE_FIELD, body.as_slice())])
            .await
            .map_err(transport_err)?;
        Ok(())
    }

    /// Creates the consumer group on each configured stream (tolerating
    /// `BUSYGROUP`) and spawns the `XREADGROUP` consume loop. Idempotent:
    /// a second call is a no-op while the loop is running.
    pub async fn start(&self) -> EdaResult<()> {
        self.ensure_open()?;
        let mut task_guard = self.consume_task.lock().await;
        if task_guard.as_ref().is_some_and(|h| !h.is_finished()) {
            return Ok(());
        }

        // Create the consumer group on every stream, ignoring the
        // BUSYGROUP error a pre-existing group raises.
        let mut conn = self.new_connection().await?;
        for stream in &self.config.streams {
            create_group_mkstream(&mut conn, stream, &self.config.group).await?;
        }

        let shared = Arc::clone(&self.shared);
        let config = self.config.clone();
        let loop_conn = conn;
        let handle = tokio::spawn(async move {
            consume_loop(loop_conn, config, shared).await;
        });
        *task_guard = Some(handle);
        Ok(())
    }

    /// Cancels the consume loop, closes the broker, and rejects further
    /// operations with [`EdaError::Closed`]. Idempotent.
    pub async fn close(&self) -> EdaResult<()> {
        self.shared
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.consume_task.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
        self.shared.subscriptions.write().await.clear();
        *self.publish_conn.lock().await = None;
        Ok(())
    }

    /// Establishes a fresh multiplexed connection to the configured
    /// Redis instance.
    async fn new_connection(&self) -> EdaResult<MultiplexedConnection> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(transport_err)
    }

    /// Returns [`EdaError::Closed`] once the broker has been closed.
    fn ensure_open(&self) -> EdaResult<()> {
        if self.shared.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(EdaError::Closed);
        }
        Ok(())
    }
}

/// Issues `XGROUP CREATE <stream> <group> $ MKSTREAM`, swallowing the
/// `BUSYGROUP` error that signals the group already exists — pyfly's
/// `"BUSYGROUP" not in str(exc)` guard.
async fn create_group_mkstream(
    conn: &mut MultiplexedConnection,
    stream: &str,
    group: &str,
) -> EdaResult<()> {
    let res: redis::RedisResult<Value> = conn.xgroup_create_mkstream(stream, group, "$").await;
    match res {
        Ok(_) => Ok(()),
        Err(e) if is_busygroup(&e) => Ok(()),
        Err(e) => Err(transport_err(e)),
    }
}

/// Recognizes Redis's `BUSYGROUP` reply (the group already exists),
/// which pyfly treats as success.
fn is_busygroup(e: &redis::RedisError) -> bool {
    e.code() == Some("BUSYGROUP") || e.to_string().contains("BUSYGROUP")
}

/// The long-poll consume loop: `XREADGROUP … BLOCK`, dispatch, `XACK` on
/// success. Runs until the broker is closed (the task is aborted by
/// [`RedisStreamsBroker::close`]).
async fn consume_loop(mut conn: MultiplexedConnection, config: RedisConfig, shared: Arc<Shared>) {
    // `>` reads only never-delivered entries for this group.
    let stream_ids: Vec<&str> = config.streams.iter().map(|_| ">").collect();
    let opts = StreamReadOptions::default()
        .group(&config.group, &config.consumer_id)
        .count(config.count)
        .block(config.block_ms);

    while !shared.closed.load(std::sync::atomic::Ordering::SeqCst) {
        let reply: redis::RedisResult<StreamReadReply> = conn
            .xread_options(&config.streams, &stream_ids, &opts)
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(e) => {
                tracing::warn!(error = %e, "firefly/eda-redis: xreadgroup failed; retrying");
                // Brief backoff so a hard failure does not spin the CPU.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        for stream_key in reply.keys {
            for entry in stream_key.ids {
                dispatch_and_ack(&mut conn, &config, &shared, &stream_key.key, &entry).await;
            }
        }
    }
}

/// Dispatches one stream entry to its matching handlers and `XACK`s it
/// on success. On any *handler* error the entry is left pending (no
/// `XACK`) so Redis redelivers it — pyfly's at-least-once semantics. An
/// undeserializable or field-less entry is acked and skipped (logged),
/// because redelivering a poison message would loop forever.
async fn dispatch_and_ack(
    conn: &mut MultiplexedConnection,
    config: &RedisConfig,
    shared: &Shared,
    stream: &str,
    entry: &redis::streams::StreamId,
) {
    let raw = match entry.map.get(ENVELOPE_FIELD) {
        Some(value) => value,
        None => {
            tracing::warn!(
                stream,
                entry_id = %entry.id,
                "firefly/eda-redis: entry missing '{ENVELOPE_FIELD}' field; acking and skipping"
            );
            ack(conn, config, stream, &entry.id).await;
            return;
        }
    };
    let bytes = match value_as_bytes(raw) {
        Some(bytes) => bytes,
        None => {
            tracing::warn!(
                stream,
                entry_id = %entry.id,
                "firefly/eda-redis: envelope field not a string/bulk; acking and skipping"
            );
            ack(conn, config, stream, &entry.id).await;
            return;
        }
    };
    let event: Event = match serde_json::from_slice(&bytes) {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(
                stream,
                entry_id = %entry.id,
                error = %e,
                "firefly/eda-redis: failed to deserialize envelope; acking to prevent redelivery"
            );
            ack(conn, config, stream, &entry.id).await;
            return;
        }
    };

    // Snapshot matching handlers under the read lock, then dispatch
    // outside it so a handler may re-enter the broker safely.
    let handlers: Vec<Handler> = {
        let subs = shared.subscriptions.read().await;
        subs.iter()
            .filter(|s| s.matcher.is_match(event.topic.as_str()))
            .map(|s| Arc::clone(&s.handler))
            .collect()
    };

    for handler in handlers {
        if let Err(e) = handler(event.clone()).await {
            tracing::warn!(
                stream,
                entry_id = %entry.id,
                topic = %event.topic,
                error = %e,
                "firefly/eda-redis: handler errored; leaving entry pending for redelivery"
            );
            // Leave the entry unacked — at-least-once redelivery.
            return;
        }
    }
    ack(conn, config, stream, &entry.id).await;
}

/// `XACK`s a single entry, logging (but not propagating) a transport
/// failure — an ack failure is non-fatal and the entry will simply be
/// redelivered.
async fn ack(conn: &mut MultiplexedConnection, config: &RedisConfig, stream: &str, id: &str) {
    let res: redis::RedisResult<i64> = conn.xack(stream, &config.group, &[id]).await;
    if let Err(e) = res {
        tracing::warn!(stream, entry_id = id, error = %e, "firefly/eda-redis: xack failed");
    }
}

/// Extracts raw bytes from a RESP [`Value`], accepting both
/// `BulkString` (the on-the-wire form of an `XADD` field) and
/// `SimpleString`.
fn value_as_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::BulkString(bytes) => Some(bytes.clone()),
        Value::SimpleString(s) => Some(s.clone().into_bytes()),
        _ => None,
    }
}

/// Wraps a [`redis::RedisError`] as an [`EdaError::Handler`] carrying a
/// `500` — the broker-transport failure surface.
fn transport_err(e: redis::RedisError) -> EdaError {
    EdaError::Handler(FireflyError::internal(format!(
        "firefly/eda-redis: redis transport error: {e}"
    )))
}

#[async_trait]
impl Publisher for RedisStreamsBroker {
    async fn publish(&self, ev: Event) -> EdaResult<()> {
        RedisStreamsBroker::publish(self, ev).await
    }

    async fn close(&self) -> EdaResult<()> {
        RedisStreamsBroker::close(self).await
    }
}

#[async_trait]
impl Subscriber for RedisStreamsBroker {
    async fn subscribe(&self, topic: &str, h: Handler) -> EdaResult<()> {
        RedisStreamsBroker::subscribe(self, topic, h).await
    }

    async fn close(&self) -> EdaResult<()> {
        RedisStreamsBroker::close(self).await
    }
}

/// Connects to Redis with `config` and returns the broker boxed behind
/// the [`Broker`] trait object — the factory the starter invokes when
/// the EDA provider selects `redis`, paralleling
/// [`firefly_eda::new_kafka_broker`].
///
/// The consume loop starts lazily on the first
/// [`publish`](RedisStreamsBroker::publish) or an explicit
/// [`RedisStreamsBroker::start`]; subscribe before publishing if you
/// need every event delivered.
pub fn new_redis_broker(config: RedisConfig) -> EdaResult<Box<dyn Broker>> {
    Ok(Box::new(RedisStreamsBroker::connect(config)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_as_bytes_reads_bulk_and_simple() {
        assert_eq!(
            value_as_bytes(&Value::BulkString(b"abc".to_vec())),
            Some(b"abc".to_vec())
        );
        assert_eq!(
            value_as_bytes(&Value::SimpleString("xyz".into())),
            Some(b"xyz".to_vec())
        );
        assert_eq!(value_as_bytes(&Value::Int(7)), None);
        assert_eq!(value_as_bytes(&Value::Nil), None);
    }

    #[test]
    fn busygroup_is_recognized() {
        let busy = redis::RedisError::from((
            redis::ErrorKind::ExtensionError,
            "BUSYGROUP",
            "Consumer Group name already exists".to_string(),
        ));
        assert!(is_busygroup(&busy));

        let other = redis::RedisError::from((redis::ErrorKind::ResponseError, "boom"));
        assert!(!is_busygroup(&other));
    }

    #[test]
    fn connect_rejects_a_malformed_url() {
        match RedisStreamsBroker::connect(RedisConfig::new("not a url")) {
            Err(EdaError::Handler(_)) => {}
            Err(other) => panic!("expected Handler error, got {other:?}"),
            Ok(_) => panic!("malformed url should not connect"),
        }
    }
}
