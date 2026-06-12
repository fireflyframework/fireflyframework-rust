//! Transactional outbox — at-least-once delivery of stored events to an
//! event broker.
//!
//! Ports pyfly's `eventsourcing.outbox` module. The classic outbox pattern:
//! a writer [`enqueue`](TransactionalOutbox::enqueue)s a [`DomainEvent`]
//! alongside its aggregate write, then a background relay (started with
//! [`start`](TransactionalOutbox::start)) polls the outbox and forwards each
//! pending record to an [`OutboxSink`]. Delivery is at-least-once: a record
//! is retried until it succeeds or exhausts `max_attempts`, after which it is
//! moved aside as a dead letter for inspection / manual retry.
//!
//! The default sink — [`EdaSink`] — bridges [`DomainEvent`]s onto a
//! `firefly-eda` [`Publisher`], the Rust analog of pyfly's
//! `EventSourcingPublisher`.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use firefly_eda::{Event, Publisher};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::aggregate::DomainEvent;

/// The mutable state of one outbox record, shared between the relay task
/// and any handle the enqueuer holds.
#[derive(Debug)]
struct RecordState {
    id: String,
    event: DomainEvent,
    attempts: u32,
    delivered: bool,
    last_error: Option<String>,
}

/// A handle to one enqueued outbox record.
///
/// Returned by [`TransactionalOutbox::enqueue`]; reading it reflects the
/// live state mutated by the relay task, so a caller can poll
/// [`delivered`](OutboxRecord::delivered) / [`attempts`](OutboxRecord::attempts)
/// to observe progress — matching pyfly, where the same `OutboxRecord`
/// dataclass instance is mutated in place by the publish loop.
#[derive(Debug, Clone)]
pub struct OutboxRecord {
    state: Arc<Mutex<RecordState>>,
}

impl OutboxRecord {
    /// The record's unique id (a fresh UUID v4, like pyfly's
    /// `uuid.uuid4()`).
    pub fn id(&self) -> String {
        self.lock().id.clone()
    }

    /// A copy of the stored event awaiting delivery.
    pub fn event(&self) -> DomainEvent {
        self.lock().event.clone()
    }

    /// How many delivery attempts have failed so far.
    pub fn attempts(&self) -> u32 {
        self.lock().attempts
    }

    /// Whether the event was successfully delivered to the sink.
    pub fn delivered(&self) -> bool {
        self.lock().delivered
    }

    /// The most recent delivery error message, if any.
    pub fn last_error(&self) -> Option<String> {
        self.lock().last_error.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecordState> {
        self.state.lock().expect("outbox record lock poisoned")
    }
}

/// The destination an outbox relay forwards events to.
///
/// Implementations publish a single [`DomainEvent`] and must return `Err`
/// (with a human-readable message) on failure so the outbox can retry — the
/// Rust analog of pyfly's `publish: Callable[[StoredEventEnvelope],
/// Awaitable[None]]` that raises on failure.
#[async_trait]
pub trait OutboxSink: Send + Sync {
    /// Publishes `event`. Returning `Err(message)` causes the relay to count
    /// a failed attempt and retry on the next poll.
    async fn publish(&self, event: &DomainEvent) -> Result<(), String>;
}

/// An [`OutboxSink`] that bridges stored events onto a `firefly-eda`
/// [`Publisher`] — the Rust counterpart of pyfly's `EventSourcingPublisher`.
///
/// Each [`DomainEvent`] is wrapped in an [`Event`] published to the
/// configured `destination` topic, tagged with the event's
/// `aggregateId` / `aggregateType` / `version` headers so downstream
/// consumers can route without decoding the payload.
pub struct EdaSink {
    publisher: Arc<dyn Publisher>,
    destination: String,
    source: String,
}

impl EdaSink {
    /// Builds a sink that publishes to `destination` from logical producer
    /// `source` (the value stamped onto every [`Event::source`]).
    pub fn new(
        publisher: Arc<dyn Publisher>,
        destination: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        EdaSink {
            publisher,
            destination: destination.into(),
            source: source.into(),
        }
    }
}

#[async_trait]
impl OutboxSink for EdaSink {
    async fn publish(&self, event: &DomainEvent) -> Result<(), String> {
        let mut ev = Event::new(
            self.destination.clone(),
            event.event_type.clone(),
            self.source.clone(),
            Some(event.payload.clone()),
        )
        .with_header("aggregate_id", event.aggregate_id.clone())
        .with_header("aggregate_type", event.aggregate_type.clone())
        .with_header("version", event.version.to_string());
        // Forward any string-valued metadata entries as headers, matching
        // pyfly's EventSourcingPublisher.
        for (key, value) in &event.metadata {
            if let Some(s) = value.as_str() {
                ev = ev.with_header(key.clone(), s.to_string());
            }
        }
        self.publisher
            .publish(ev)
            .await
            .map_err(|err| err.to_string())
    }
}

/// Shared outbox storage: all enqueued records, newest last.
type Records = Arc<Mutex<Vec<Arc<Mutex<RecordState>>>>>;

/// A transactional outbox with a background polling relay.
///
/// Stores [`DomainEvent`]s and forwards them to an [`OutboxSink`] on a
/// fixed poll interval, retrying failures up to `max_attempts`. Ports
/// pyfly's `TransactionalOutbox`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::time::Duration;
/// use async_trait::async_trait;
/// use firefly_eventsourcing::{AggregateRoot, DomainEvent, OutboxSink, TransactionalOutbox};
///
/// struct Collecting(std::sync::Mutex<Vec<DomainEvent>>);
/// #[async_trait]
/// impl OutboxSink for Collecting {
///     async fn publish(&self, event: &DomainEvent) -> Result<(), String> {
///         self.0.lock().unwrap().push(event.clone());
///         Ok(())
///     }
/// }
///
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let sink = Arc::new(Collecting(Default::default()));
/// let outbox = TransactionalOutbox::new(sink.clone())
///     .with_poll_interval(Duration::from_millis(5));
///
/// let mut agg = AggregateRoot::new("acc-1", "Account");
/// agg.raise("AccountOpened", br#"{"owner":"Ada"}"#);
/// let record = outbox.enqueue(agg.take_uncommitted().remove(0)).await;
///
/// outbox.start().await;
/// for _ in 0..100 {
///     tokio::time::sleep(Duration::from_millis(2)).await;
///     if record.delivered() { break; }
/// }
/// outbox.stop().await;
/// assert!(record.delivered());
/// # });
/// ```
pub struct TransactionalOutbox {
    sink: Arc<dyn OutboxSink>,
    records: Records,
    max_attempts: u32,
    poll_interval: std::time::Duration,
    stop_tx: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl TransactionalOutbox {
    /// Builds an outbox over `sink` with the pyfly defaults: 5 attempts and
    /// a 1-second poll interval.
    pub fn new(sink: Arc<dyn OutboxSink>) -> Self {
        let (stop_tx, _) = watch::channel(false);
        TransactionalOutbox {
            sink,
            records: Arc::new(Mutex::new(Vec::new())),
            max_attempts: 5,
            poll_interval: std::time::Duration::from_secs(1),
            stop_tx,
            task: Mutex::new(None),
        }
    }

    /// Overrides the number of delivery attempts before a record becomes a
    /// dead letter. Default: 5.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Overrides how often the relay scans the outbox. Default: 1 second.
    #[must_use]
    pub fn with_poll_interval(mut self, interval: std::time::Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Enqueues `event` for at-least-once delivery and returns a live
    /// handle to its record.
    pub async fn enqueue(&self, event: DomainEvent) -> OutboxRecord {
        let state = Arc::new(Mutex::new(RecordState {
            id: firefly_kernel_new_uuid(),
            event,
            attempts: 0,
            delivered: false,
            last_error: None,
        }));
        self.records
            .lock()
            .expect("outbox lock poisoned")
            .push(Arc::clone(&state));
        OutboxRecord { state }
    }

    /// Starts the background relay. A second call while the relay is running
    /// is a no-op, matching pyfly.
    pub async fn start(&self) {
        let mut task = self.task.lock().expect("outbox task lock poisoned");
        if task.is_some() {
            return;
        }
        // Reset the stop signal only when restarting after a previous stop,
        // so a fresh outbox does not trigger a spurious immediate wakeup.
        if *self.stop_tx.borrow() {
            let _ = self.stop_tx.send(false);
        }
        let sink = Arc::clone(&self.sink);
        let records = Arc::clone(&self.records);
        let max_attempts = self.max_attempts;
        let poll_interval = self.poll_interval;
        let stop_rx = self.stop_tx.subscribe();
        *task = Some(tokio::spawn(async move {
            relay_loop(sink, records, max_attempts, poll_interval, stop_rx).await;
        }));
    }

    /// Stops the background relay and waits for it to exit. A call before
    /// [`start`](TransactionalOutbox::start) is a no-op.
    ///
    /// The stop signal is carried on a `watch` channel whose latest value is
    /// retained, so there is no lost-wakeup race: the relay observes the
    /// stop even if `stop` is called while it is between poll cycles.
    pub async fn stop(&self) {
        let handle = {
            let mut task = self.task.lock().expect("outbox task lock poisoned");
            task.take()
        };
        if let Some(handle) = handle {
            let _ = self.stop_tx.send(true);
            let _ = handle.await;
        }
    }

    /// The records still eligible for delivery — not yet delivered and not
    /// yet exhausted. Dead letters are deliberately excluded so the relay
    /// stops re-attempting them, matching pyfly's `pending()`.
    pub async fn pending(&self) -> Vec<OutboxRecord> {
        self.snapshot(|s| !s.delivered && s.attempts < self.max_attempts)
    }

    /// Records that exhausted `max_attempts` without being delivered.
    /// Retained for inspection / manual retry — at-least-once delivery
    /// holds up to `max_attempts`. Mirrors pyfly's `dead_letters()`.
    pub async fn dead_letters(&self) -> Vec<OutboxRecord> {
        self.snapshot(|s| !s.delivered && s.attempts >= self.max_attempts)
    }

    fn snapshot(&self, predicate: impl Fn(&RecordState) -> bool) -> Vec<OutboxRecord> {
        self.records
            .lock()
            .expect("outbox lock poisoned")
            .iter()
            .filter(|state| predicate(&state.lock().expect("outbox record lock poisoned")))
            .map(|state| OutboxRecord {
                state: Arc::clone(state),
            })
            .collect()
    }
}

/// The relay loop: scan, publish pending records, retry on failure, then
/// wait `poll_interval` (or until stopped). A direct port of pyfly's
/// `TransactionalOutbox._loop`. The stop signal rides a `watch` channel
/// whose latest value is retained, so the loop never misses a stop sent
/// while it was busy publishing (no lost-wakeup race).
async fn relay_loop(
    sink: Arc<dyn OutboxSink>,
    records: Records,
    max_attempts: u32,
    poll_interval: std::time::Duration,
    mut stop_rx: watch::Receiver<bool>,
) {
    loop {
        if *stop_rx.borrow() {
            break;
        }

        // Snapshot the pending records under the lock, then release it
        // before awaiting the (possibly slow) sink.
        let pending: Vec<Arc<Mutex<RecordState>>> = {
            let guard = records.lock().expect("outbox lock poisoned");
            guard
                .iter()
                .filter(|state| {
                    let s = state.lock().expect("outbox record lock poisoned");
                    !s.delivered && s.attempts < max_attempts
                })
                .map(Arc::clone)
                .collect()
        };

        for state in pending {
            let event = {
                let s = state.lock().expect("outbox record lock poisoned");
                s.event.clone()
            };
            match sink.publish(&event).await {
                Ok(()) => {
                    state.lock().expect("outbox record lock poisoned").delivered = true;
                }
                Err(err) => {
                    let mut s = state.lock().expect("outbox record lock poisoned");
                    s.attempts += 1;
                    s.last_error = Some(err);
                }
            }
        }

        // Wait for the next poll tick or a stop signal. `changed()` resolves
        // immediately if the value was already updated to `true` before we
        // started waiting, so a stop can never be missed.
        tokio::select! {
            res = stop_rx.changed() => {
                // Sender dropped or stop requested — either way, exit.
                if res.is_err() || *stop_rx.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

/// A UUID v4 string, matching pyfly's `str(uuid.uuid4())`. Kept local so the
/// crate does not take a direct `uuid` dependency in its public surface.
fn firefly_kernel_new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
