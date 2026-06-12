//! A queryable dead-letter store for *failed events*.
//!
//! Mirrors pyfly's `eda.dlq` package
//! (`EdaDeadLetterEntry` / `EdaDeadLetterStore` / `InMemoryEdaDeadLetterStore`):
//! a store of events that exhausted their retries, captured for operator
//! inspection and replay. This is distinct from the *routing* DLQ in
//! [`wrap_listener`](crate::wrap_listener) — the routing path republishes
//! an exhausted event to a dead-letter *topic*, whereas this store keeps
//! an inspectable, queryable record (list / get / remove) of the failed
//! events themselves.
//!
//! The two compose: [`wrap_listener`](crate::wrap_listener) can be given
//! an [`EdaDeadLetterStore`] so an exhausted event is *both* captured in
//! the store *and* (when a dead-letter topic is configured) republished.
//!
//! ```
//! use std::sync::Arc;
//! use firefly_eda::{EdaDeadLetterEntry, EdaDeadLetterStore, Event, InMemoryEdaDeadLetterStore};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let store = InMemoryEdaDeadLetterStore::new();
//! let ev = Event::new("orders", "OrderPlaced", "svc", None);
//! let entry = EdaDeadLetterEntry::new(ev, "ValidationError", "bad order", 3);
//! let id = entry.id.clone();
//! store.add(entry).await;
//!
//! let listed = store.list(100).await;
//! assert_eq!(listed.len(), 1);
//! assert!(store.get(&id).await.is_some());
//! assert!(store.remove(&id).await);
//! # });
//! ```

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Event;

/// One captured failed event, the Rust analog of pyfly's
/// `EdaDeadLetterEntry`. Carries the full failing [`Event`], a stable
/// `id` (a fresh correlation-style id minted by [`EdaDeadLetterEntry::new`]),
/// the error classification (`error_type` / `error_message`), the wall
/// clock `timestamp` of capture, and the number of `attempts` made
/// before giving up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdaDeadLetterEntry {
    /// Stable entry id (distinct from the event's own id).
    pub id: String,
    /// The failing event, captured verbatim.
    pub event: Event,
    /// Error classification — the Rust analog of pyfly's exception class
    /// name (e.g. the failing [`FireflyError`](firefly_kernel::FireflyError)
    /// code).
    pub error_type: String,
    /// Human-readable error detail.
    pub error_message: String,
    /// Wall-clock instant (UTC) the entry was captured.
    pub timestamp: DateTime<Utc>,
    /// Number of delivery attempts made before dead-lettering.
    pub attempts: u32,
}

impl EdaDeadLetterEntry {
    /// Builds an entry for `event` with a fresh id, the current time, and
    /// the given error classification and attempt count.
    pub fn new(
        event: Event,
        error_type: impl Into<String>,
        error_message: impl Into<String>,
        attempts: u32,
    ) -> Self {
        Self {
            id: firefly_kernel::new_correlation_id(),
            event,
            error_type: error_type.into(),
            error_message: error_message.into(),
            timestamp: Utc::now(),
            attempts,
        }
    }
}

/// A queryable store of dead-lettered events. The Rust analog of pyfly's
/// `EdaDeadLetterStore` protocol — `add` / `list` / `get` / `remove`.
///
/// Object-safe (`async-trait`) so a store can be shared as
/// `Arc<dyn EdaDeadLetterStore>` between [`wrap_listener`](crate::wrap_listener)
/// and an operator-facing inspection endpoint.
#[async_trait]
pub trait EdaDeadLetterStore: Send + Sync {
    /// Captures `entry`.
    async fn add(&self, entry: EdaDeadLetterEntry);

    /// Returns up to `limit` entries, most recent first (by capture
    /// `timestamp`) — pyfly's `list(limit=…)`.
    async fn list(&self, limit: usize) -> Vec<EdaDeadLetterEntry>;

    /// Fetches a single entry by its [`EdaDeadLetterEntry::id`], if
    /// present. (pyfly's protocol exposes only list/delete; `get` is the
    /// Rust addition that makes the store fully queryable.)
    async fn get(&self, entry_id: &str) -> Option<EdaDeadLetterEntry>;

    /// Removes the entry with `entry_id`, returning whether it existed —
    /// pyfly's `delete(entry_id) -> bool`.
    async fn remove(&self, entry_id: &str) -> bool;
}

/// In-memory [`EdaDeadLetterStore`], the Rust analog of pyfly's
/// `InMemoryEdaDeadLetterStore`. Backed by a `Mutex<HashMap>` keyed by
/// entry id; [`list`](EdaDeadLetterStore::list) returns entries sorted by
/// capture time, most recent first.
#[derive(Default)]
pub struct InMemoryEdaDeadLetterStore {
    entries: Mutex<HashMap<String, EdaDeadLetterEntry>>,
}

impl InMemoryEdaDeadLetterStore {
    /// Returns an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of currently captured entries — a convenience for
    /// tests and operator dashboards.
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("firefly/eda: lock poisoned")
            .len()
    }

    /// Whether the store holds no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl EdaDeadLetterStore for InMemoryEdaDeadLetterStore {
    async fn add(&self, entry: EdaDeadLetterEntry) {
        self.entries
            .lock()
            .expect("firefly/eda: lock poisoned")
            .insert(entry.id.clone(), entry);
    }

    async fn list(&self, limit: usize) -> Vec<EdaDeadLetterEntry> {
        let mut entries: Vec<EdaDeadLetterEntry> = self
            .entries
            .lock()
            .expect("firefly/eda: lock poisoned")
            .values()
            .cloned()
            .collect();
        // Most recent first, exactly like pyfly's
        // `sorted(..., key=timestamp, reverse=True)[:limit]`.
        entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        entries.truncate(limit);
        entries
    }

    async fn get(&self, entry_id: &str) -> Option<EdaDeadLetterEntry> {
        self.entries
            .lock()
            .expect("firefly/eda: lock poisoned")
            .get(entry_id)
            .cloned()
    }

    async fn remove(&self, entry_id: &str) -> bool {
        self.entries
            .lock()
            .expect("firefly/eda: lock poisoned")
            .remove(entry_id)
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> Event {
        Event::new("orders.events", "OrderPlaced", "svc", Some(b"{}".to_vec()))
    }

    /// pyfly `test_dlq_round_trip`: add → list → delete.
    #[tokio::test]
    async fn dlq_round_trip() {
        let store = InMemoryEdaDeadLetterStore::new();
        let entry = EdaDeadLetterEntry::new(event(), "X", "boom", 1);
        let id = entry.id.clone();
        store.add(entry).await;

        let listed = store.list(100).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].event.event_type, "OrderPlaced");
        assert_eq!(listed[0].error_type, "X");
        assert_eq!(listed[0].error_message, "boom");

        assert!(store.remove(&id).await);
        assert!(store.list(100).await.is_empty());
    }

    /// `get` fetches a single entry by id; `remove` of a missing id is
    /// `false`.
    #[tokio::test]
    async fn get_and_remove_by_id() {
        let store = InMemoryEdaDeadLetterStore::new();
        let entry = EdaDeadLetterEntry::new(event(), "Boom", "msg", 2);
        let id = entry.id.clone();
        store.add(entry).await;

        let got = store.get(&id).await.expect("entry present");
        assert_eq!(got.attempts, 2);
        assert!(store.get("nope").await.is_none());
        assert!(!store.remove("nope").await);
        assert!(store.remove(&id).await);
        assert!(store.is_empty());
    }

    /// `list` returns most-recent-first and honours the limit, exactly
    /// like pyfly's reverse-sorted, sliced result.
    #[tokio::test]
    async fn list_is_most_recent_first_and_limited() {
        let store = InMemoryEdaDeadLetterStore::new();
        // Stamp explicit, ordered timestamps so the sort is deterministic
        // regardless of how fast the entries are created.
        let base = Utc::now();
        for i in 0..5u32 {
            let mut entry = EdaDeadLetterEntry::new(event(), "X", "m", i);
            entry.timestamp = base + chrono::Duration::seconds(i as i64);
            store.add(entry).await;
        }

        let listed = store.list(3).await;
        assert_eq!(listed.len(), 3, "limit honoured");
        // Most recent (highest attempts, since timestamp tracks i) first.
        assert_eq!(listed[0].attempts, 4);
        assert_eq!(listed[1].attempts, 3);
        assert_eq!(listed[2].attempts, 2);
    }

    /// An entry round-trips through JSON so it can be served on an
    /// inspection endpoint.
    #[test]
    fn entry_serializes_to_json() {
        let entry = EdaDeadLetterEntry::new(event(), "ValidationError", "bad", 3);
        let json = serde_json::to_string(&entry).unwrap();
        let back: EdaDeadLetterEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }
}
