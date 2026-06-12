//! SQL-backed [`EventStore`] over the `firefly-transactional` [`Database`]
//! port.
//!
//! Ports pyfly's `SqlAlchemyEventStore`. Events are persisted to a single
//! `firefly_event_store` table; the per-aggregate `version` column carries a
//! `UNIQUE(aggregate_id, version)` constraint that backstops optimistic
//! concurrency. [`append`](SqlEventStore::append) reads the current head
//! version *inside* the write transaction (no check-then-write TOCTOU race)
//! and translates a concurrent unique-constraint collision into
//! [`EventSourcingError::Concurrency`] rather than leaking a raw driver
//! error — matching pyfly's TOCTOU fix.
//!
//! The [`Database`] port is synchronous, so this adapter is portable across
//! any backend that implements the port (it is exercised in-crate against
//! `rusqlite`). An optional [`EventUpcaster`] chain runs on the read paths.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firefly_transactional::{exec, with_tx, Database, Executor, Row, SqlValue, TxContext, TxError};

use crate::aggregate::{DomainEvent, EventStore};
use crate::error::EventSourcingError;
use crate::upcaster::{apply_upcasters, EventUpcaster};

/// `CREATE TABLE IF NOT EXISTS` for the event store. Portable DDL — the
/// `version` column plus `UNIQUE(aggregate_id, version)` enforce optimistic
/// concurrency at the storage layer.
pub const DDL: &str = "CREATE TABLE IF NOT EXISTS firefly_event_store (\
    event_id        TEXT NOT NULL,\
    aggregate_id    TEXT NOT NULL,\
    aggregate_type  TEXT NOT NULL,\
    version         INTEGER NOT NULL,\
    event_type      TEXT NOT NULL,\
    occurred_at     TEXT NOT NULL,\
    payload         TEXT NOT NULL,\
    UNIQUE (aggregate_id, version)\
)";

/// A SQL-backed event store over a [`Database`] port.
///
/// Construct one with [`SqlEventStore::new`], then call
/// [`initialize`](SqlEventStore::initialize) once to create the table. The
/// store owns its `Database` behind an `Arc` so it can be shared across
/// tasks.
///
/// # Example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use firefly_eventsourcing::{EventStore, SqlEventStore};
/// # use firefly_transactional::Database;
/// # async fn demo(db: Arc<dyn Database>) -> Result<(), Box<dyn std::error::Error>> {
/// let store = SqlEventStore::new(db);
/// store.initialize()?;
/// let loaded = store.load("acc-1").await;
/// # let _ = loaded;
/// # Ok(())
/// # }
/// ```
pub struct SqlEventStore {
    db: Arc<dyn Database>,
    upcasters: Vec<Arc<dyn EventUpcaster>>,
}

impl std::fmt::Debug for SqlEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlEventStore")
            .field("upcasters", &self.upcasters.len())
            .finish_non_exhaustive()
    }
}

impl SqlEventStore {
    /// Builds a store over `db` with no upcasters.
    pub fn new(db: Arc<dyn Database>) -> Self {
        SqlEventStore {
            db,
            upcasters: Vec::new(),
        }
    }

    /// Builds a store over `db` whose read paths apply `upcasters` in order.
    pub fn with_upcasters(db: Arc<dyn Database>, upcasters: Vec<Arc<dyn EventUpcaster>>) -> Self {
        SqlEventStore { db, upcasters }
    }

    /// Creates the `firefly_event_store` table if it does not yet exist.
    /// Idempotent; call once before first use.
    pub fn initialize(&self) -> Result<(), EventSourcingError> {
        self.db.execute(DDL, &[]).map(|_| ()).map_err(map_tx_err)
    }

    /// The current head version for `aggregate_id` (0 when absent) — the
    /// number of events stored for the stream, like pyfly's
    /// `latest_version`.
    pub async fn latest_version(&self, aggregate_id: &str) -> Result<i64, EventSourcingError> {
        let row = self
            .db
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM firefly_event_store WHERE aggregate_id = ?1",
                &[SqlValue::Text(aggregate_id.to_string())],
            )
            .map_err(map_tx_err)?;
        Ok(row
            .and_then(|r| match r.get_index(0) {
                Some(SqlValue::Integer(n)) => Some(*n),
                _ => None,
            })
            .unwrap_or(0))
    }
}

#[async_trait]
impl EventStore for SqlEventStore {
    async fn append(
        &self,
        aggregate_id: &str,
        expected_version: i64,
        events: Vec<DomainEvent>,
    ) -> Result<(), EventSourcingError> {
        if events.is_empty() {
            return Ok(());
        }
        let aggregate_id = aggregate_id.to_string();
        // Read the head version and insert inside one transaction so the
        // check-then-write is atomic; the UNIQUE(aggregate_id, version)
        // constraint backstops a concurrent racer.
        let result = with_tx(&TxContext::root(), self.db.as_ref(), |ctx| {
            let conn = exec(ctx, self.db.as_ref());
            let head = conn
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM firefly_event_store WHERE aggregate_id = ?1",
                    &[SqlValue::Text(aggregate_id.clone())],
                )?
                .and_then(|r| match r.get_index(0) {
                    Some(SqlValue::Integer(n)) => Some(*n),
                    _ => None,
                })
                .unwrap_or(0);
            if head != expected_version {
                return Err(concurrency_marker());
            }
            for (i, event) in events.iter().enumerate() {
                let version = expected_version + (i as i64) + 1;
                // Stamp the authoritative aggregate id + store-assigned
                // version onto the event before persisting, so the stored
                // payload round-trips with the version the store chose
                // (mirrors pyfly setting `evt.sequence = expected_version + i`).
                let mut stamped = event.clone();
                stamped.aggregate_id = aggregate_id.clone();
                stamped.version = version;
                conn.execute(
                    "INSERT INTO firefly_event_store \
                     (event_id, aggregate_id, aggregate_type, version, event_type, occurred_at, payload) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    &[
                        SqlValue::Text(uuid::Uuid::new_v4().to_string()),
                        SqlValue::Text(aggregate_id.clone()),
                        SqlValue::Text(stamped.aggregate_type.clone()),
                        SqlValue::Integer(version),
                        SqlValue::Text(stamped.event_type.clone()),
                        SqlValue::Text(stamped.time.to_rfc3339()),
                        SqlValue::Text(
                            serde_json::to_string(&stamped)
                                .map_err(|e| TxError::database(e.to_string()))?,
                        ),
                    ],
                )?;
            }
            Ok(())
        });

        match result {
            Ok(()) => Ok(()),
            Err(err) if is_concurrency(&err) => Err(EventSourcingError::Concurrency),
            Err(err) => Err(map_tx_err(err)),
        }
    }

    async fn load(&self, aggregate_id: &str) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let rows = self
            .db
            .query(
                "SELECT payload FROM firefly_event_store \
                 WHERE aggregate_id = ?1 ORDER BY version",
                &[SqlValue::Text(aggregate_id.to_string())],
            )
            .map_err(map_tx_err)?;
        if rows.is_empty() {
            return Err(EventSourcingError::AggregateNotFound);
        }
        rows.into_iter()
            .map(|r| self.decode(&r))
            .collect::<Result<Vec<_>, _>>()
    }

    async fn load_after(
        &self,
        aggregate_id: &str,
        since_version: i64,
    ) -> Result<Vec<DomainEvent>, EventSourcingError> {
        let rows = self
            .db
            .query(
                "SELECT payload FROM firefly_event_store \
                 WHERE aggregate_id = ?1 AND version > ?2 ORDER BY version",
                &[
                    SqlValue::Text(aggregate_id.to_string()),
                    SqlValue::Integer(since_version),
                ],
            )
            .map_err(map_tx_err)?;
        rows.into_iter()
            .map(|r| self.decode(&r))
            .collect::<Result<Vec<_>, _>>()
    }
}

impl SqlEventStore {
    /// Decodes one `payload` JSON column into a [`DomainEvent`], applying
    /// the upcaster chain.
    fn decode(&self, row: &Row) -> Result<DomainEvent, EventSourcingError> {
        let payload = match row.get("payload").or_else(|| row.get_index(0)) {
            Some(SqlValue::Text(s)) => s.clone(),
            other => {
                return Err(EventSourcingError::Projection(format!(
                    "firefly/eventsourcing: unexpected payload column: {other:?}"
                )))
            }
        };
        let event: DomainEvent = serde_json::from_str(&payload).map_err(|e| {
            EventSourcingError::Projection(format!(
                "firefly/eventsourcing: corrupt stored event: {e}"
            ))
        })?;
        Ok(apply_upcasters(event, &self.upcasters))
    }
}

/// A sentinel `TxError` carrying the concurrency marker so the helper can
/// roll back and we can recognise it after `with_tx` returns.
const CONCURRENCY_MARKER: &str = "firefly/eventsourcing::sql::concurrency";

fn concurrency_marker() -> TxError {
    TxError::application(CONCURRENCY_MARKER)
}

/// Whether `err` is the optimistic-concurrency signal — either our in-tx
/// version-check marker, or a UNIQUE-constraint collision a concurrent
/// writer triggered (the storage-layer backstop). The latter is matched on
/// the driver message, mirroring pyfly translating `IntegrityError` into
/// `ConcurrencyError`.
fn is_concurrency(err: &TxError) -> bool {
    let text = err.to_string();
    text.contains(CONCURRENCY_MARKER)
        || text.to_ascii_lowercase().contains("unique")
        || text.to_ascii_lowercase().contains("constraint")
}

/// Maps a [`TxError`] to the event-sourcing error taxonomy. Non-concurrency
/// driver failures surface as [`EventSourcingError::Projection`] carrying
/// the driver message (the crate has no dedicated storage variant; this
/// keeps the public error enum backward-compatible).
fn map_tx_err(err: TxError) -> EventSourcingError {
    EventSourcingError::Projection(format!("firefly/eventsourcing: store error: {err}"))
}

/// Parses an RFC 3339 timestamp the SQL store wrote, returning UTC.
/// Exposed for adapters that read the `occurred_at` column directly.
pub fn parse_occurred_at(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}
