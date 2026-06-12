//! Event-recording broker for asserting which events a handler emitted.

use std::sync::Mutex;

/// The test view of an event handed to [`SpyBroker::record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEvent {
    /// Topic the event was published to.
    pub topic: String,
    /// Event type discriminator (e.g. `"OrderPlaced"`).
    pub event_type: String,
    /// Raw event payload bytes.
    pub payload: Vec<u8>,
}

/// An EDA-compatible recorder used by tests to assert which events a
/// handler emitted. It does NOT fan out to subscribers — see the `eda`
/// crate's in-memory broker when you need real fan-out.
///
/// All methods take `&self` and are safe to call from multiple threads or
/// tasks concurrently; interior state is guarded by a mutex.
#[derive(Debug, Default)]
pub struct SpyBroker {
    items: Mutex<Vec<RecordedEvent>>,
}

impl SpyBroker {
    /// Returns an empty `SpyBroker`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores an event-shaped tuple — call it from your custom test
    /// publisher closure to bridge into `SpyBroker` without depending on
    /// the `eda` crate. The payload bytes are copied.
    pub fn record(&self, topic: &str, event_type: &str, payload: &[u8]) {
        self.items
            .lock()
            .expect("SpyBroker mutex poisoned")
            .push(RecordedEvent {
                topic: topic.to_string(),
                event_type: event_type.to_string(),
                payload: payload.to_vec(),
            });
    }

    /// Returns every recorded event whose `event_type` equals `event_type`.
    pub fn find_by_type(&self, event_type: &str) -> Vec<RecordedEvent> {
        self.items
            .lock()
            .expect("SpyBroker mutex poisoned")
            .iter()
            .filter(|e| e.event_type == event_type)
            .cloned()
            .collect()
    }

    /// Returns a snapshot of every recorded event, in publication order.
    pub fn items(&self) -> Vec<RecordedEvent> {
        self.items.lock().expect("SpyBroker mutex poisoned").clone()
    }

    /// Clears the recorder.
    pub fn reset(&self) {
        self.items.lock().expect("SpyBroker mutex poisoned").clear();
    }

    /// Returns the number of recorded events.
    pub fn len(&self) -> usize {
        self.items.lock().expect("SpyBroker mutex poisoned").len()
    }

    /// Reports whether no events have been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Port of Go TestSpyBroker.
    #[test]
    fn record_filter_reset() {
        let b = SpyBroker::new();
        b.record("orders", "OrderPlaced", br#"{"id":1}"#);
        b.record("orders", "OrderShipped", br#"{"id":1}"#);
        assert_eq!(b.len(), 2);
        let got = b.find_by_type("OrderPlaced");
        assert_eq!(got.len(), 1, "filtered: {got:?}");
        b.reset();
        assert_eq!(b.len(), 0);
        assert!(b.is_empty());
    }

    #[test]
    fn recorded_event_carries_topic_type_and_payload_copy() {
        let b = SpyBroker::new();
        let mut payload = b"{\"id\":1}".to_vec();
        b.record("orders", "OrderPlaced", &payload);
        payload[0] = b'X'; // mutating the caller's buffer must not affect the record
        let got = b.items();
        assert_eq!(
            got,
            vec![RecordedEvent {
                topic: "orders".to_string(),
                event_type: "OrderPlaced".to_string(),
                payload: b"{\"id\":1}".to_vec(),
            }]
        );
    }

    #[test]
    fn find_by_type_returns_empty_for_unknown_type() {
        let b = SpyBroker::new();
        b.record("orders", "OrderPlaced", b"{}");
        assert!(b.find_by_type("Nope").is_empty());
    }

    // Rust-specific: SpyBroker must be shareable across threads/tasks.
    #[test]
    fn spy_broker_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SpyBroker>();
        assert_send_sync::<RecordedEvent>();
    }

    // Rust-specific: concurrent recording is race-free (the Go port guards
    // Items with a sync.Mutex; this is the -race equivalent check).
    #[test]
    fn concurrent_record_is_race_free() {
        let b = Arc::new(SpyBroker::new());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let b = Arc::clone(&b);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        b.record("orders", if i % 2 == 0 { "Even" } else { "Odd" }, b"{}");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(b.len(), 400);
        assert_eq!(b.find_by_type("Even").len(), 200);
        assert_eq!(b.find_by_type("Odd").len(), 200);
    }
}
