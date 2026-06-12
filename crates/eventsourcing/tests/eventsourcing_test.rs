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

//! Ported 1:1 from the Go module's `eventsourcing_test.go`, plus
//! Rust-specific coverage: wire-format pinning, Send/Sync bounds and
//! concurrency races.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcingError, EventStore, MemoryEventStore,
    MemorySnapshotStore, Projection, ProjectionRunner, Snapshot, SnapshotStore,
};

/// Test double mirroring Go's `projFunc`: counts every applied event.
struct CountingProjection {
    name: &'static str,
    count: AtomicUsize,
}

#[async_trait::async_trait]
impl Projection for CountingProjection {
    fn name(&self) -> &str {
        self.name
    }

    async fn apply(&self, _event: &DomainEvent) -> Result<(), EventSourcingError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Projection that always fails, for short-circuit assertions.
struct FailingProjection;

#[async_trait::async_trait]
impl Projection for FailingProjection {
    fn name(&self) -> &str {
        "failing"
    }

    async fn apply(&self, _event: &DomainEvent) -> Result<(), EventSourcingError> {
        Err(EventSourcingError::Projection("boom".into()))
    }
}

// --- Go: TestAggregateRaise -------------------------------------------------

#[test]
fn aggregate_raise_buffers_uncommitted_and_clear() {
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", br#"{"name":"a"}"#);
    a.raise("UserRenamed", br#"{"name":"b"}"#);
    assert_eq!(a.version, 2);
    assert_eq!(a.uncommitted().len(), 2);
    a.clear();
    assert!(a.uncommitted().is_empty());
}

#[test]
fn raise_stamps_event_fields() {
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", br#"{"name":"a"}"#);
    let ev = &a.uncommitted()[0];
    assert_eq!(ev.aggregate_id, "u1");
    assert_eq!(ev.aggregate_type, "User");
    assert_eq!(ev.version, 1);
    assert_eq!(ev.event_type, "UserCreated");
    assert_eq!(ev.payload, br#"{"name":"a"}"#.to_vec());
    assert!(ev.metadata.is_empty());
}

#[test]
fn take_uncommitted_drains_buffer() {
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", b"{}");
    a.raise("UserRenamed", b"{}");
    let events = a.take_uncommitted();
    assert_eq!(events.len(), 2);
    assert!(a.uncommitted().is_empty());
    assert_eq!(a.version, 2, "version survives the drain");
}

// --- Go: TestEventStoreOptimisticConcurrency --------------------------------

#[tokio::test]
async fn event_store_optimistic_concurrency() {
    let store = MemoryEventStore::new();

    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", b"{}");
    store
        .append(&a.id, 0, a.uncommitted().to_vec())
        .await
        .expect("first append");
    a.clear();

    // Stale write — expected version 0 but head is now 1.
    a.raise("UserRenamed", b"{}");
    let err = store
        .append(&a.id, 0, a.uncommitted().to_vec())
        .await
        .expect_err("stale append must fail");
    assert_eq!(err, EventSourcingError::Concurrency);

    // Correct write at head=1.
    store
        .append(&a.id, 1, a.uncommitted().to_vec())
        .await
        .expect("append at head");

    let loaded = store.load(&a.id).await.expect("load");
    assert_eq!(loaded.len(), 2);
}

#[tokio::test]
async fn append_empty_events_is_noop() {
    let store = MemoryEventStore::new();
    // Go returns nil before the version check, even on a mismatched
    // expected version.
    store
        .append("u1", 42, Vec::new())
        .await
        .expect("empty append never errors");
    assert_eq!(
        store.load("u1").await.expect_err("still empty"),
        EventSourcingError::AggregateNotFound
    );
}

#[tokio::test]
async fn load_missing_aggregate_returns_not_found() {
    let store = MemoryEventStore::new();
    assert_eq!(
        store.load("ghost").await.expect_err("missing aggregate"),
        EventSourcingError::AggregateNotFound
    );
}

#[tokio::test]
async fn load_after_filters_by_version() {
    let store = MemoryEventStore::new();
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("E1", b"{}");
    a.raise("E2", b"{}");
    a.raise("E3", b"{}");
    let events = a.take_uncommitted();
    store.append(&a.id, 0, events).await.unwrap();

    let after = store.load_after("u1", 1).await.expect("load_after");
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].version, 2);
    assert_eq!(after[1].version, 3);

    // A missing stream is an empty slice, not an error — Go parity.
    let none = store.load_after("ghost", 0).await.expect("missing stream");
    assert!(none.is_empty());
}

#[tokio::test]
async fn concurrent_appends_only_one_wins() {
    let store = Arc::new(MemoryEventStore::new());
    let mut handles = Vec::new();
    for i in 0..8 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let mut a = AggregateRoot::new("u1", "User");
            a.raise(format!("E{i}"), b"{}");
            store.append("u1", 0, a.take_uncommitted()).await
        }));
    }
    let mut wins = 0;
    for handle in handles {
        match handle.await.expect("task") {
            Ok(()) => wins += 1,
            Err(err) => assert_eq!(err, EventSourcingError::Concurrency),
        }
    }
    assert_eq!(wins, 1, "exactly one racer may append at version 0");
    assert_eq!(store.load("u1").await.unwrap().len(), 1);
}

// --- Go: TestProjectionRunnerReplay ------------------------------------------

#[tokio::test]
async fn projection_runner_replay() {
    let store = MemoryEventStore::new();
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", b"{}");
    a.raise("UserRenamed", b"{}");
    let events = a.take_uncommitted();
    store.append(&a.id, 0, events).await.unwrap();

    let counter = Arc::new(CountingProjection {
        name: "count",
        count: AtomicUsize::new(0),
    });
    let runner = ProjectionRunner::new();
    runner.register(counter.clone());
    runner.replay(&store, "u1").await.expect("replay");
    assert_eq!(counter.count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn projection_error_short_circuits() {
    let store = MemoryEventStore::new();
    let mut a = AggregateRoot::new("u1", "User");
    a.raise("UserCreated", b"{}");
    let events = a.take_uncommitted();
    store.append(&a.id, 0, events).await.unwrap();

    let counter = Arc::new(CountingProjection {
        name: "count",
        count: AtomicUsize::new(0),
    });
    let runner = ProjectionRunner::new();
    runner.register(Arc::new(FailingProjection));
    runner.register(counter.clone());

    let err = runner.replay(&store, "u1").await.expect_err("must fail");
    assert_eq!(err, EventSourcingError::Projection("boom".into()));
    assert_eq!(
        counter.count.load(Ordering::SeqCst),
        0,
        "first error short-circuits later projections"
    );
}

#[tokio::test]
async fn replay_missing_aggregate_propagates_not_found() {
    let runner = ProjectionRunner::new();
    let store = MemoryEventStore::new();
    assert_eq!(
        runner.replay(&store, "ghost").await.expect_err("missing"),
        EventSourcingError::AggregateNotFound
    );
}

// --- Go: TestSnapshotStore ----------------------------------------------------

#[tokio::test]
async fn snapshot_store_soft_miss_then_latest() {
    let store = MemorySnapshotStore::new();
    let miss = store.latest("u1").await.expect("soft miss is not an error");
    assert!(miss.is_none());

    store
        .save(Snapshot {
            aggregate_id: "u1".into(),
            aggregate_type: String::new(),
            version: 5,
            payload: b"x".to_vec(),
        })
        .await
        .expect("save");

    let latest = store.latest("u1").await.expect("latest").expect("present");
    assert_eq!(latest.version, 5);
    assert_eq!(latest.payload, b"x".to_vec());
}

#[tokio::test]
async fn snapshot_save_overwrites_latest() {
    let store = MemorySnapshotStore::new();
    for version in [1, 2, 3] {
        store
            .save(Snapshot {
                aggregate_id: "u1".into(),
                aggregate_type: "User".into(),
                version,
                payload: b"x".to_vec(),
            })
            .await
            .unwrap();
    }
    let latest = store.latest("u1").await.unwrap().expect("present");
    assert_eq!(latest.version, 3, "only the latest capture is kept");
}

// --- Rust-specific: wire format ------------------------------------------------

#[test]
fn domain_event_json_wire_format_matches_go() {
    let ev = DomainEvent {
        aggregate_id: "u1".into(),
        aggregate_type: "User".into(),
        version: 1,
        event_type: "UserCreated".into(),
        time: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        payload: br#"{"name":"alice"}"#.to_vec(),
        metadata: BTreeMap::new(),
        tenant_id: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    // Field names, ordering, RFC 3339 time, base64 payload and the
    // omitted-when-empty metadata all match Go's encoding/json output.
    assert_eq!(
        json,
        r#"{"aggregateId":"u1","aggregateType":"User","version":1,"type":"UserCreated","time":"2026-01-02T03:04:05Z","payload":"eyJuYW1lIjoiYWxpY2UifQ=="}"#
    );
}

#[test]
fn domain_event_json_round_trip_with_metadata() {
    let mut metadata = BTreeMap::new();
    metadata.insert("b".to_string(), serde_json::json!(2));
    metadata.insert("a".to_string(), serde_json::json!("one"));
    let ev = DomainEvent {
        aggregate_id: "u1".into(),
        aggregate_type: "User".into(),
        version: 7,
        event_type: "UserRenamed".into(),
        time: Utc.with_ymd_and_hms(2026, 6, 12, 10, 0, 0).unwrap(),
        payload: b"{}".to_vec(),
        metadata,
        tenant_id: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    // BTreeMap sorts keys — matching Go's sorted map-key encoding.
    assert!(json.ends_with(r#""metadata":{"a":"one","b":2}}"#), "{json}");
    let back: DomainEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ev);
}

#[test]
fn domain_event_deserializes_go_produced_json() {
    // As emitted by the Go port (RFC3339Nano time, base64 payload).
    let json = r#"{"aggregateId":"u1","aggregateType":"User","version":1,"type":"UserCreated","time":"2026-01-02T03:04:05.123456789Z","payload":"e30="}"#;
    let ev: DomainEvent = serde_json::from_str(json).unwrap();
    assert_eq!(ev.payload, b"{}".to_vec());
    assert_eq!(ev.time.timestamp_subsec_nanos(), 123_456_789);
    assert!(ev.metadata.is_empty(), "missing metadata defaults to empty");

    // Go encodes a nil []byte payload as JSON null.
    let json_null = r#"{"aggregateId":"u1","aggregateType":"User","version":1,"type":"UserCreated","time":"2026-01-02T03:04:05Z","payload":null}"#;
    let ev: DomainEvent = serde_json::from_str(json_null).unwrap();
    assert!(ev.payload.is_empty());
}

#[test]
fn snapshot_json_wire_format_matches_go() {
    let snap = Snapshot {
        aggregate_id: "u1".into(),
        aggregate_type: "User".into(),
        version: 5,
        payload: b"x".to_vec(),
    };
    let json = serde_json::to_string(&snap).unwrap();
    assert_eq!(
        json,
        r#"{"aggregateId":"u1","aggregateType":"User","version":5,"payload":"eA=="}"#
    );
    let back: Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
}

// --- Rust-specific: bounds + errors ---------------------------------------------

#[test]
fn ports_and_stores_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MemoryEventStore>();
    assert_send_sync::<MemorySnapshotStore>();
    assert_send_sync::<ProjectionRunner>();
    assert_send_sync::<DomainEvent>();
    assert_send_sync::<Snapshot>();
    assert_send_sync::<EventSourcingError>();
    // The ports stay object-safe.
    fn _event_store_obj(_: &dyn EventStore) {}
    fn _snapshot_store_obj(_: &dyn SnapshotStore) {}
    fn _projection_obj(_: &dyn Projection) {}
}

#[test]
fn error_display_matches_go_sentinels() {
    assert_eq!(
        EventSourcingError::Concurrency.to_string(),
        "firefly/eventsourcing: concurrency conflict"
    );
    assert_eq!(
        EventSourcingError::AggregateNotFound.to_string(),
        "firefly/eventsourcing: aggregate not found"
    );
    assert_eq!(
        EventSourcingError::Projection("boom".into()).to_string(),
        "firefly/eventsourcing: projection error: boom"
    );
}
