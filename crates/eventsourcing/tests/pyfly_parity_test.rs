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

//! Ports pyfly's `tests/eventsourcing` cases for the surfaces added at
//! pyfly parity: [`EventUpcaster`] applied on read, [`TransactionalOutbox`]
//! delivery + dead-letters, and the SQL-backed [`SqlEventStore`] (exercised
//! against `rusqlite`, the in-crate stand-in for the `firefly-transactional`
//! `Database` port).
//!
//! Mirrors `test_eventsourcing.py::TestOutbox`,
//! `test_eventsourcing_fixes.py::{TestUpcastersAppliedOnRead,
//! TestOutboxDeadLetters, TestSqlAlchemyConcurrency}`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use firefly_eventsourcing::{
    AggregateRoot, DomainEvent, EventSourcedAggregate, EventSourcedRepository, EventSourcingError,
    EventStore, EventUpcaster, FunctionProjection, MemoryEventStore, MemorySnapshotStore,
    NoOpUpcaster, OutboxSink, Projection, ProjectionRunner, SnapshotStore, SqlEventStore,
    TransactionalOutbox,
};
use firefly_transactional::{Database, Executor, Row, SqlValue, Transaction, TxError};
use rusqlite::Connection;
use std::sync::atomic::AtomicI64;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Builds an event already stamped with its aggregate id / type, mirroring
/// pyfly's `_env(...)` helper.
fn env(aggregate_id: &str, event_type: &str, payload: &[u8]) -> DomainEvent {
    let mut agg = AggregateRoot::new(aggregate_id, "Account");
    agg.raise(event_type, payload.to_vec());
    agg.take_uncommitted().remove(0)
}

/// pyfly's `_RenameUpcaster`: upcasts the legacy event name to the current
/// one and stamps a marker into the JSON payload.
struct RenameUpcaster;
impl EventUpcaster for RenameUpcaster {
    fn applies_to(&self, event: &DomainEvent) -> bool {
        event.event_type == "legacy.opened"
    }
    fn upcast(&self, mut event: DomainEvent) -> DomainEvent {
        event.event_type = "account.opened".into();
        event.payload = br#"{"upcast":true}"#.to_vec();
        event
    }
}

// ---------------------------------------------------------------------------
// Upcasters applied on read — test_eventsourcing_fixes.TestUpcastersAppliedOnRead
// ---------------------------------------------------------------------------

#[tokio::test]
async fn load_and_load_after_apply_upcasters() {
    let store = MemoryEventStore::with_upcasters(vec![Arc::new(RenameUpcaster)]);
    store
        .append("acc-1", 0, vec![env("acc-1", "legacy.opened", b"{}")])
        .await
        .expect("append");

    let loaded = store.load("acc-1").await.expect("load");
    assert_eq!(
        loaded
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>(),
        ["account.opened"]
    );
    assert_eq!(loaded[0].payload, br#"{"upcast":true}"#.to_vec());

    // load_after is the Rust analog of pyfly's stream_all read path.
    let after = store.load_after("acc-1", 0).await.expect("load_after");
    assert_eq!(
        after
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>(),
        ["account.opened"]
    );
}

#[tokio::test]
async fn no_upcasters_is_identity() {
    let store = MemoryEventStore::new();
    store
        .append("acc-1", 0, vec![env("acc-1", "legacy.opened", b"{}")])
        .await
        .expect("append");
    assert_eq!(
        store.load("acc-1").await.unwrap()[0].event_type,
        "legacy.opened"
    );

    // A NoOpUpcaster chain is also the identity.
    let store = MemoryEventStore::with_upcasters(vec![Arc::new(NoOpUpcaster)]);
    store
        .append("acc-2", 0, vec![env("acc-2", "legacy.opened", b"{}")])
        .await
        .expect("append");
    assert_eq!(
        store.load("acc-2").await.unwrap()[0].event_type,
        "legacy.opened"
    );
}

// ---------------------------------------------------------------------------
// Outbox — test_eventsourcing.TestOutbox + test_eventsourcing_fixes.TestOutboxDeadLetters
// ---------------------------------------------------------------------------

/// A sink that records every published event — pyfly's `publish` collector.
#[derive(Default)]
struct Collecting {
    published: Mutex<Vec<DomainEvent>>,
}
#[async_trait]
impl OutboxSink for Collecting {
    async fn publish(&self, event: &DomainEvent) -> Result<(), String> {
        self.published.lock().unwrap().push(event.clone());
        Ok(())
    }
}

/// A sink that always fails — pyfly's `always_fail`.
struct AlwaysFail {
    calls: AtomicUsize,
}
#[async_trait]
impl OutboxSink for AlwaysFail {
    async fn publish(&self, _event: &DomainEvent) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err("upstream down".into())
    }
}

#[tokio::test]
async fn outbox_publishes() {
    let sink = Arc::new(Collecting::default());
    let outbox =
        TransactionalOutbox::new(sink.clone()).with_poll_interval(Duration::from_millis(5));
    let record = outbox.enqueue(env("acc-1", "account.opened", b"{}")).await;

    outbox.start().await;
    // Bounded poll loop — never sleeps more than ~200ms total.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if record.delivered() {
            break;
        }
    }
    outbox.stop().await;

    assert!(record.delivered(), "record must be delivered");
    assert_eq!(sink.published.lock().unwrap().len(), 1);
    assert!(outbox.pending().await.is_empty());
}

#[tokio::test]
async fn outbox_exhausted_records_are_surfaced_as_dead_letters() {
    let sink = Arc::new(AlwaysFail {
        calls: AtomicUsize::new(0),
    });
    let outbox = TransactionalOutbox::new(sink.clone())
        .with_max_attempts(2)
        .with_poll_interval(Duration::from_millis(5));
    let record = outbox.enqueue(env("acc-1", "account.opened", b"{}")).await;

    outbox.start().await;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if record.attempts() >= 2 {
            break;
        }
    }
    outbox.stop().await;

    assert!(record.attempts() >= 2, "attempts={}", record.attempts());
    assert!(!record.delivered());
    assert_eq!(record.last_error().as_deref(), Some("upstream down"));
    // Excluded from the publish loop once exhausted...
    assert!(outbox.pending().await.is_empty());
    // ...but surfaced for inspection.
    let dead = outbox.dead_letters().await;
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].id(), record.id());
    // The relay stopped re-attempting after exhaustion (2 attempts only).
    assert_eq!(sink.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn eda_sink_bridges_stored_events_onto_the_broker() {
    // pyfly's EventSourcingPublisher parity: the EdaSink forwards a stored
    // event onto a firefly-eda broker, tagged with routing headers.
    use firefly_eda::{handler, Event, InMemoryBroker, Publisher};

    let broker = Arc::new(InMemoryBroker::new());
    let received: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_received = Arc::clone(&received);
    broker
        .subscribe(
            "pyfly.events",
            handler(move |ev: Event| {
                let received = Arc::clone(&sink_received);
                async move {
                    received.lock().unwrap().push(ev);
                    Ok(())
                }
            }),
        )
        .expect("subscribe");

    let publisher: Arc<dyn Publisher> = broker.clone();
    let sink = Arc::new(firefly_eventsourcing::EdaSink::new(
        publisher,
        "pyfly.events",
        "account-svc",
    ));
    let outbox = TransactionalOutbox::new(sink).with_poll_interval(Duration::from_millis(5));
    let mut agg = AggregateRoot::new("acc-1", "Account");
    agg.raise("AccountOpened", br#"{"owner":"Ada"}"#);
    let record = outbox.enqueue(agg.take_uncommitted().remove(0)).await;

    outbox.start().await;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if record.delivered() {
            break;
        }
    }
    outbox.stop().await;
    broker.close().expect("close");

    let events = received.lock().unwrap();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.event_type, "AccountOpened");
    assert_eq!(ev.source, "account-svc");
    assert_eq!(ev.topic, "pyfly.events");
    assert_eq!(ev.payload.as_deref(), Some(br#"{"owner":"Ada"}"#.as_ref()));
    assert_eq!(
        ev.headers.get("aggregate_id").map(String::as_str),
        Some("acc-1")
    );
    assert_eq!(
        ev.headers.get("aggregate_type").map(String::as_str),
        Some("Account")
    );
    assert_eq!(ev.headers.get("version").map(String::as_str), Some("1"));
}

// ---------------------------------------------------------------------------
// SQL EventStore over the firefly-transactional Database port (rusqlite)
// ---------------------------------------------------------------------------

/// A `Database` port backed by a file-based SQLite database, opening a
/// fresh connection per operation / transaction. This connection-per-tx
/// model matches a real pooled database (and pyfly's file-based test setup):
/// concurrent writers genuinely contend on the `UNIQUE(aggregate_id,
/// version)` constraint, so the loser sees a constraint error the
/// `SqlEventStore` translates into `ConcurrencyError` — never a raw DB
/// error.
struct SqliteDatabase {
    path: String,
    // Keeps the temp dir alive for the lifetime of the database.
    _dir: tempfile::TempDir,
}
struct SqliteTransaction {
    conn: Mutex<Option<Connection>>,
}

impl SqliteDatabase {
    fn temp() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("es.db").to_string_lossy().into_owned();
        SqliteDatabase { path, _dir: dir }
    }

    fn open(&self) -> Result<Connection, TxError> {
        let conn = Connection::open(&self.path).map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_err)?;
        Ok(conn)
    }
}

fn db_err(err: rusqlite::Error) -> TxError {
    TxError::database(err.to_string())
}

fn bind(params: &[SqlValue]) -> impl Iterator<Item = rusqlite::types::Value> + '_ {
    params.iter().map(|p| match p {
        SqlValue::Null => rusqlite::types::Value::Null,
        SqlValue::Integer(i) => rusqlite::types::Value::Integer(*i),
        SqlValue::Real(f) => rusqlite::types::Value::Real(*f),
        SqlValue::Text(s) => rusqlite::types::Value::Text(s.clone()),
        SqlValue::Blob(b) => rusqlite::types::Value::Blob(b.clone()),
    })
}

fn run_execute(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
    conn.execute(sql, rusqlite::params_from_iter(bind(params)))
        .map(|n| n as u64)
        .map_err(db_err)
}

fn run_query(conn: &Connection, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
    let mut stmt = conn.prepare(sql).map_err(db_err)?;
    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let mut rows = stmt
        .query(rusqlite::params_from_iter(bind(params)))
        .map_err(db_err)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(db_err)? {
        let mut values = Vec::with_capacity(columns.len());
        for idx in 0..columns.len() {
            let value: rusqlite::types::Value = row.get(idx).map_err(db_err)?;
            values.push(match value {
                rusqlite::types::Value::Null => SqlValue::Null,
                rusqlite::types::Value::Integer(i) => SqlValue::Integer(i),
                rusqlite::types::Value::Real(f) => SqlValue::Real(f),
                rusqlite::types::Value::Text(s) => SqlValue::Text(s),
                rusqlite::types::Value::Blob(b) => SqlValue::Blob(b),
            });
        }
        out.push(Row::new(columns.clone(), values));
    }
    Ok(out)
}

impl Executor for SqliteDatabase {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
        run_execute(&self.open()?, sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
        run_query(&self.open()?, sql, params)
    }
}

impl Database for SqliteDatabase {
    fn begin(&self) -> Result<Box<dyn Transaction + '_>, TxError> {
        let conn = self.open()?;
        // IMMEDIATE takes the write lock at BEGIN so two concurrent writers
        // serialize and the loser fails fast (with busy_timeout) rather than
        // deadlocking — the realistic pooled-DB behaviour.
        conn.execute_batch("BEGIN IMMEDIATE").map_err(db_err)?;
        Ok(Box::new(SqliteTransaction {
            conn: Mutex::new(Some(conn)),
        }))
    }
}

impl Executor for SqliteTransaction {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<u64, TxError> {
        let guard = self.conn.lock().unwrap();
        run_execute(
            guard.as_ref().expect("transaction already finished"),
            sql,
            params,
        )
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Row>, TxError> {
        let guard = self.conn.lock().unwrap();
        run_query(
            guard.as_ref().expect("transaction already finished"),
            sql,
            params,
        )
    }
}

impl Transaction for SqliteTransaction {
    fn commit(self: Box<Self>) -> Result<(), TxError> {
        let conn = self.conn.lock().unwrap().take().expect("finished");
        conn.execute_batch("COMMIT").map_err(db_err)
    }
    fn rollback(self: Box<Self>) -> Result<(), TxError> {
        let conn = self.conn.lock().unwrap().take().expect("finished");
        conn.execute_batch("ROLLBACK").map_err(db_err)
    }
}

#[tokio::test]
async fn sql_store_append_load_round_trip() {
    let store = SqlEventStore::new(Arc::new(SqliteDatabase::temp()));
    store.initialize().expect("ddl");

    store
        .append(
            "o-1",
            0,
            vec![env("o-1", "OrderPlaced", br#"{"amount":42}"#)],
        )
        .await
        .expect("first append");
    store
        .append(
            "o-1",
            1,
            vec![env("o-1", "OrderShipped", br#"{"carrier":"ups"}"#)],
        )
        .await
        .expect("second append at head 1");

    let loaded = store.load("o-1").await.expect("load");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].event_type, "OrderPlaced");
    assert_eq!(loaded[0].version, 1);
    assert_eq!(loaded[1].event_type, "OrderShipped");
    assert_eq!(loaded[1].version, 2);
    assert_eq!(loaded[0].payload, br#"{"amount":42}"#.to_vec());

    assert_eq!(store.latest_version("o-1").await.unwrap(), 2);

    let after = store.load_after("o-1", 1).await.expect("load_after");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].version, 2);
}

#[tokio::test]
async fn sql_store_missing_aggregate_is_not_found() {
    let store = SqlEventStore::new(Arc::new(SqliteDatabase::temp()));
    store.initialize().expect("ddl");
    assert_eq!(
        store.load("ghost").await.expect_err("missing"),
        EventSourcingError::AggregateNotFound
    );
    assert_eq!(store.latest_version("ghost").await.unwrap(), 0);
    assert!(store.load_after("ghost", 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn sql_store_stale_expected_version_is_concurrency_error() {
    let store = SqlEventStore::new(Arc::new(SqliteDatabase::temp()));
    store.initialize().expect("ddl");
    store
        .append("acc-1", 0, vec![env("acc-1", "account.opened", b"{}")])
        .await
        .expect("first append");

    // Stale write — expected version 0 but head is now 1. pyfly translates
    // this (and the UNIQUE backstop) into a ConcurrencyError.
    let err = store
        .append("acc-1", 0, vec![env("acc-1", "a.deposited", b"{}")])
        .await
        .expect_err("stale append must fail");
    assert_eq!(err, EventSourcingError::Concurrency);

    // The losing write left no partial state behind.
    assert_eq!(store.latest_version("acc-1").await.unwrap(), 1);
    assert_eq!(store.load("acc-1").await.unwrap().len(), 1);
}

#[tokio::test]
async fn sql_store_concurrent_append_one_wins_other_sees_concurrency() {
    // Shared DB, two writers both targeting version 1 — mirrors pyfly's
    // TestSqlAlchemyConcurrency (exactly one wins, the loser sees a
    // ConcurrencyError, never a raw DB error).
    let db = Arc::new(SqliteDatabase::temp());
    let store = Arc::new(SqlEventStore::new(db));
    store.initialize().expect("ddl");
    store
        .append("acc-1", 0, vec![env("acc-1", "account.opened", b"{}")])
        .await
        .expect("seed");

    let mut handles = Vec::new();
    for ty in ["a.deposited", "b.deposited"] {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .append("acc-1", 1, vec![env("acc-1", ty, b"{}")])
                .await
        }));
    }

    let mut wins = 0;
    for handle in handles {
        match handle.await.expect("join") {
            Ok(()) => wins += 1,
            Err(err) => assert_eq!(
                err,
                EventSourcingError::Concurrency,
                "loser must see ConcurrencyError, not a raw DB error"
            ),
        }
    }
    assert_eq!(wins, 1, "exactly one writer wins at version 1");
    assert_eq!(store.latest_version("acc-1").await.unwrap(), 2);
}

#[tokio::test]
async fn sql_store_applies_upcasters_on_load() {
    let store = SqlEventStore::with_upcasters(
        Arc::new(SqliteDatabase::temp()),
        vec![Arc::new(RenameUpcaster)],
    );
    store.initialize().expect("ddl");
    store
        .append("acc-1", 0, vec![env("acc-1", "legacy.opened", b"{}")])
        .await
        .expect("append");

    let loaded = store.load("acc-1").await.expect("load");
    assert_eq!(loaded[0].event_type, "account.opened");
    assert_eq!(loaded[0].payload, br#"{"upcast":true}"#.to_vec());
}

// ---------------------------------------------------------------------------
// Global cross-aggregate stream — test_eventsourcing.TestInMemoryEventStore::test_stream_all
// + test_eventsourcing.TestProjection::test_projection_consumes_events
// ---------------------------------------------------------------------------

/// Stamps a tenant id onto a freshly-raised event.
fn env_tenant(aggregate_id: &str, event_type: &str, tenant: &str) -> DomainEvent {
    let mut agg = AggregateRoot::new(aggregate_id, "Account").with_tenant(tenant);
    agg.raise(event_type, b"{}".to_vec());
    agg.take_uncommitted().remove(0)
}

#[tokio::test]
async fn memory_stream_all_returns_global_log_in_append_order() {
    // pyfly TestInMemoryEventStore::test_stream_all: one event per aggregate,
    // stream_all sees all three.
    let store = MemoryEventStore::new();
    for i in 0..3 {
        store
            .append(
                &format!("o-{i}"),
                0,
                vec![env(&format!("o-{i}"), "OrderPlaced", b"{}")],
            )
            .await
            .expect("append");
    }
    let all = store.stream_all(None, 100, None).await.expect("stream_all");
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].event.aggregate_id, "o-0");
    assert_eq!(all[2].event.aggregate_id, "o-2");
    // Cursor keys are distinct and monotonic.
    assert!(all[0].event_id < all[1].event_id);
    assert!(all[1].event_id < all[2].event_id);
}

#[tokio::test]
async fn memory_stream_all_cursor_resumes_and_limits() {
    let store = MemoryEventStore::new();
    for i in 0..5 {
        store
            .append(
                &format!("o-{i}"),
                0,
                vec![env(&format!("o-{i}"), "E", b"{}")],
            )
            .await
            .expect("append");
    }
    // First page of 2.
    let page1 = store.stream_all(None, 2, None).await.unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].event.aggregate_id, "o-0");
    // Resume after the last event of page1.
    let cursor = page1.last().unwrap().event_id.clone();
    let page2 = store.stream_all(Some(&cursor), 2, None).await.unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].event.aggregate_id, "o-2");
    let cursor = page2.last().unwrap().event_id.clone();
    let page3 = store.stream_all(Some(&cursor), 2, None).await.unwrap();
    assert_eq!(page3.len(), 1);
    assert_eq!(page3[0].event.aggregate_id, "o-4");
    // Drained.
    let cursor = page3.last().unwrap().event_id.clone();
    assert!(store
        .stream_all(Some(&cursor), 2, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn memory_stream_all_unknown_cursor_yields_empty_page() {
    let store = MemoryEventStore::new();
    store
        .append("o-1", 0, vec![env("o-1", "E", b"{}")])
        .await
        .unwrap();
    assert!(store
        .stream_all(Some("does-not-exist"), 100, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn memory_stream_all_filters_by_tenant() {
    // Multi-tenancy: tenant_id persisted on the event, filterable in stream_all.
    let store = MemoryEventStore::new();
    store
        .append("a-1", 0, vec![env_tenant("a-1", "E", "acme")])
        .await
        .unwrap();
    store
        .append("g-1", 0, vec![env_tenant("g-1", "E", "globex")])
        .await
        .unwrap();
    store
        .append("a-2", 0, vec![env_tenant("a-2", "E", "acme")])
        .await
        .unwrap();

    let acme = store.stream_all(None, 100, Some("acme")).await.unwrap();
    assert_eq!(acme.len(), 2);
    assert!(acme.iter().all(|s| s.tenant_id() == Some("acme")));
    assert_eq!(acme[0].event.aggregate_id, "a-1");
    assert_eq!(acme[1].event.aggregate_id, "a-2");

    let globex = store.stream_all(None, 100, Some("globex")).await.unwrap();
    assert_eq!(globex.len(), 1);
    assert_eq!(globex[0].event.aggregate_id, "g-1");

    // Unfiltered sees all three.
    assert_eq!(store.stream_all(None, 100, None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn tenant_id_round_trips_through_event_json() {
    // The tenant_id is part of the persisted envelope (round-trips through
    // serialize/deserialize) but a None tenant is omitted from the wire form.
    let ev = env_tenant("a-1", "E", "acme");
    assert_eq!(ev.tenant_id.as_deref(), Some("acme"));
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""tenantId":"acme""#), "{json}");
    let back: DomainEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tenant_id.as_deref(), Some("acme"));

    let no_tenant = env("a-1", "E", b"{}");
    assert_eq!(no_tenant.tenant_id, None);
    assert!(!serde_json::to_string(&no_tenant)
        .unwrap()
        .contains("tenantId"));
}

/// A projection that records every event id it sees, and optionally fails on
/// a designated event id (to prove the cursor does not advance past it).
struct Recording {
    name: &'static str,
    seen: Mutex<Vec<String>>,
    fail_on_type: Option<&'static str>,
}
#[async_trait]
impl Projection for Recording {
    fn name(&self) -> &str {
        self.name
    }
    async fn apply(&self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        if Some(event.event_type.as_str()) == self.fail_on_type {
            return Err(EventSourcingError::Projection("boom".into()));
        }
        self.seen.lock().unwrap().push(event.aggregate_id.clone());
        Ok(())
    }
}

#[tokio::test]
async fn projection_runner_replay_all_drains_global_stream() {
    // pyfly TestProjection::test_projection_consumes_events, but deterministic
    // (no background poll / sleeps): replay_all drains the whole global log.
    let store = MemoryEventStore::new();
    for i in 0..3 {
        store
            .append(
                &format!("o-{i}"),
                0,
                vec![env(&format!("o-{i}"), "OrderPlaced", b"{}")],
            )
            .await
            .unwrap();
    }
    let projection = Arc::new(Recording {
        name: "collect",
        seen: Mutex::new(Vec::new()),
        fail_on_type: None,
    });
    let runner = ProjectionRunner::new();
    runner.register(projection.clone());

    // Batch size 2 to exercise the paging loop.
    let cursor = runner.replay_all(&store, None, 2, None).await.unwrap();
    assert_eq!(projection.seen.lock().unwrap().len(), 3);
    assert!(cursor.is_some());

    // A second replay_all from the cursor is a no-op (idempotent resume).
    let cursor2 = runner
        .replay_all(&store, cursor.clone(), 2, None)
        .await
        .unwrap();
    assert_eq!(cursor2, cursor);
    assert_eq!(projection.seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn projection_drive_once_does_not_advance_past_a_failing_event() {
    // At-least-once, in-order: a failing event halts the batch and the cursor
    // stays on the last good event so the failure is retried (pyfly _loop).
    let store = MemoryEventStore::new();
    store
        .append("o-0", 0, vec![env("o-0", "Good", b"{}")])
        .await
        .unwrap();
    store
        .append("o-1", 0, vec![env("o-1", "Bad", b"{}")])
        .await
        .unwrap();
    store
        .append("o-2", 0, vec![env("o-2", "Good", b"{}")])
        .await
        .unwrap();

    let projection = Arc::new(Recording {
        name: "collect",
        seen: Mutex::new(Vec::new()),
        fail_on_type: Some("Bad"),
    });
    let runner = ProjectionRunner::new();
    runner.register(projection.clone());

    let (cursor, err) = runner.drive_once(&store, None, 100, None).await.unwrap();
    // Applied the first Good, then halted on Bad — cursor sits on o-0.
    assert!(err.is_some());
    assert_eq!(projection.seen.lock().unwrap().as_slice(), &["o-0"]);
    let first = store.stream_all(None, 1, None).await.unwrap()[0]
        .event_id
        .clone();
    assert_eq!(cursor.as_deref(), Some(first.as_str()));
}

#[tokio::test]
async fn function_projection_consumes_via_runner() {
    let store = MemoryEventStore::new();
    store
        .append("o-1", 0, vec![env("o-1", "E", b"{}")])
        .await
        .unwrap();
    let count = Arc::new(AtomicI64::new(0));
    let sink = Arc::clone(&count);
    let projection = Arc::new(FunctionProjection::new("fn", move |_: &DomainEvent| {
        let sink = Arc::clone(&sink);
        async move {
            sink.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }));
    let runner = ProjectionRunner::new();
    runner.register(projection);
    runner.replay_all(&store, None, 10, None).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sql_store_stream_all_global_order_cursor_and_tenant() {
    let store = SqlEventStore::new(Arc::new(SqliteDatabase::temp()));
    store.initialize().expect("ddl");
    store
        .append("a-1", 0, vec![env_tenant("a-1", "E", "acme")])
        .await
        .unwrap();
    store
        .append("g-1", 0, vec![env_tenant("g-1", "E", "globex")])
        .await
        .unwrap();
    store
        .append("a-2", 0, vec![env_tenant("a-2", "E", "acme")])
        .await
        .unwrap();

    // Global order across aggregates.
    let all = store.stream_all(None, 100, None).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|s| s.event.aggregate_id.as_str())
            .collect::<Vec<_>>(),
        ["a-1", "g-1", "a-2"]
    );

    // Cursor resume.
    let cursor = all[0].event_id.clone();
    let after = store.stream_all(Some(&cursor), 100, None).await.unwrap();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].event.aggregate_id, "g-1");

    // Tenant filter (persisted column).
    let acme = store.stream_all(None, 100, Some("acme")).await.unwrap();
    assert_eq!(acme.len(), 2);
    assert!(acme.iter().all(|s| s.tenant_id() == Some("acme")));
}

// ---------------------------------------------------------------------------
// Generic EventSourcedRepository — test_eventsourcing.TestRepository
// ---------------------------------------------------------------------------

/// pyfly's `Order` aggregate: OrderPlaced sets amount, OrderShipped flips a flag.
#[derive(Default)]
struct Order {
    root: AggregateRoot,
    amount: i64,
    shipped: bool,
}

impl Order {
    fn place(&mut self, amount: i64) {
        self.amount = amount;
        self.root.raise(
            "OrderPlaced",
            format!(r#"{{"amount":{amount}}}"#).into_bytes(),
        );
    }
    fn ship(&mut self) {
        self.shipped = true;
        self.root
            .raise("OrderShipped", br#"{"carrier":"ups"}"#.to_vec());
    }
}

impl EventSourcedAggregate for Order {
    const AGGREGATE_TYPE: &'static str = "Order";
    fn root(&self) -> &AggregateRoot {
        &self.root
    }
    fn root_mut(&mut self) -> &mut AggregateRoot {
        &mut self.root
    }
    fn apply_event(&mut self, event: &DomainEvent) -> Result<(), EventSourcingError> {
        match event.event_type.as_str() {
            "OrderPlaced" => {
                let v: serde_json::Value = serde_json::from_slice(&event.payload)
                    .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
                self.amount = v["amount"].as_i64().unwrap_or(0);
            }
            "OrderShipped" => self.shipped = true,
            other => {
                return Err(EventSourcingError::Projection(format!(
                    "no handler for {other}"
                )))
            }
        }
        Ok(())
    }
    fn snapshot_payload(&self) -> Result<Vec<u8>, EventSourcingError> {
        serde_json::to_vec(&serde_json::json!({"amount": self.amount, "shipped": self.shipped}))
            .map_err(|e| EventSourcingError::Projection(e.to_string()))
    }
    fn restore_snapshot(&mut self, payload: &[u8]) -> Result<(), EventSourcingError> {
        let v: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|e| EventSourcingError::Projection(e.to_string()))?;
        self.amount = v["amount"].as_i64().unwrap_or(0);
        self.shipped = v["shipped"].as_bool().unwrap_or(false);
        Ok(())
    }
}

#[tokio::test]
async fn repository_save_and_load_round_trip() {
    // pyfly TestRepository::test_save_and_load_round_trip.
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let repo = EventSourcedRepository::<Order>::new(Arc::clone(&store));

    let mut order = Order::default();
    order.root.id = "o-1".into();
    order.root.aggregate_type = "Order".into();
    order.place(99);
    order.ship();
    assert_eq!(order.root.version, 2);
    repo.save(&mut order).await.expect("save");
    // Uncommitted drained after save.
    assert!(order.root.uncommitted().is_empty());

    let reloaded = repo.load("o-1").await.expect("load").expect("present");
    assert_eq!(reloaded.amount, 99);
    assert!(reloaded.shipped);
    assert_eq!(reloaded.root.version, 2);
}

#[tokio::test]
async fn repository_load_missing_returns_none() {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let repo = EventSourcedRepository::<Order>::new(store);
    assert!(repo.load("ghost").await.unwrap().is_none());
}

#[tokio::test]
async fn repository_save_is_noop_without_pending_events() {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let repo = EventSourcedRepository::<Order>::new(store);
    let mut order = Order::default();
    order.root.id = "o-1".into();
    // No events raised.
    repo.save(&mut order).await.expect("no-op save");
    assert!(repo.load("o-1").await.unwrap().is_none());
}

#[tokio::test]
async fn repository_snapshots_when_batch_crosses_interval() {
    // Snapshot policy: interval 2. A two-event batch crosses the boundary
    // (0 -> 2) and triggers a snapshot; load then restores it + replays the
    // (empty) tail.
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let snapshots: Arc<dyn SnapshotStore> = Arc::new(MemorySnapshotStore::new());
    let repo = EventSourcedRepository::<Order>::with_snapshots(
        Arc::clone(&store),
        Arc::clone(&snapshots),
        2,
    );

    let mut order = Order::default();
    order.root.id = "o-1".into();
    order.root.aggregate_type = "Order".into();
    order.place(50);
    order.ship();
    repo.save(&mut order).await.expect("save");

    // A snapshot at version 2 was written.
    let snap = snapshots.latest("o-1").await.unwrap().expect("snapshot");
    assert_eq!(snap.version, 2);
    assert_eq!(snap.aggregate_type, "Order");

    let reloaded = repo.load("o-1").await.unwrap().expect("present");
    assert_eq!(reloaded.amount, 50);
    assert!(reloaded.shipped);
    assert_eq!(reloaded.root.version, 2);
}

#[tokio::test]
async fn repository_does_not_snapshot_below_interval() {
    let store: Arc<dyn EventStore> = Arc::new(MemoryEventStore::new());
    let snapshots: Arc<dyn SnapshotStore> = Arc::new(MemorySnapshotStore::new());
    let repo = EventSourcedRepository::<Order>::with_snapshots(
        Arc::clone(&store),
        Arc::clone(&snapshots),
        100,
    );
    let mut order = Order::default();
    order.root.id = "o-1".into();
    order.root.aggregate_type = "Order".into();
    order.place(50);
    repo.save(&mut order).await.expect("save");
    assert!(snapshots.latest("o-1").await.unwrap().is_none());
}

#[tokio::test]
async fn repository_round_trips_over_sql_store_with_tenant() {
    // The repository works over any EventStore; here the SQL adapter, and the
    // tenant id threads through append -> stored column -> load -> replay.
    let sql = SqlEventStore::new(Arc::new(SqliteDatabase::temp()));
    sql.initialize().expect("ddl");
    let store: Arc<dyn EventStore> = Arc::new(sql);
    let repo = EventSourcedRepository::<Order>::new(Arc::clone(&store));

    let mut order = Order {
        root: AggregateRoot::new("o-9", "Order").with_tenant("acme"),
        ..Default::default()
    };
    order.place(7);
    order.ship();
    repo.save(&mut order).await.expect("save");

    let reloaded = repo.load("o-9").await.unwrap().expect("present");
    assert_eq!(reloaded.amount, 7);
    assert!(reloaded.shipped);

    // The persisted events carry the tenant id, filterable via stream_all.
    let acme = store.stream_all(None, 100, Some("acme")).await.unwrap();
    assert_eq!(acme.len(), 2);
    assert!(acme.iter().all(|s| s.tenant_id() == Some("acme")));
}
