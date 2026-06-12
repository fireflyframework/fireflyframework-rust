//! Persistence port + adapters for orchestration executions.
//!
//! The Rust spelling of pyfly's `ExecutionPersistenceProvider` SPI
//! (`pyfly.transactional.core.persistence`): [`PersistenceProvider`] is the
//! port, [`MemoryPersistence`] is the default in-process adapter, and
//! [`SqlitePersistence`] is the durable dev-grade adapter over `rusqlite`
//! (the same embedded-database pattern the migrations crate uses).

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::model::{ExecutionPattern, ExecutionState, ExecutionStatus};

/// Error raised by a [`PersistenceProvider`] adapter.
#[derive(Debug, thiserror::Error)]
#[error("firefly/orchestration: persistence: {0}")]
pub struct PersistenceError(
    /// Backend-reported failure message.
    pub String,
);

/// Optional filters for [`PersistenceProvider::list`] — pyfly's keyword
/// arguments `status=` / `pattern=` on `find_all`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExecutionFilter {
    /// Only executions in this status.
    pub status: Option<ExecutionStatus>,
    /// Only executions of this pattern.
    pub pattern: Option<ExecutionPattern>,
}

impl ExecutionFilter {
    /// No filtering — every execution.
    pub fn all() -> Self {
        Self::default()
    }

    /// Restricts to one [`ExecutionStatus`].
    #[must_use]
    pub fn status(mut self, status: ExecutionStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Restricts to one [`ExecutionPattern`].
    #[must_use]
    pub fn pattern(mut self, pattern: ExecutionPattern) -> Self {
        self.pattern = Some(pattern);
        self
    }

    fn matches(&self, state: &ExecutionState) -> bool {
        self.status.is_none_or(|s| state.status == s)
            && self.pattern.is_none_or(|p| state.pattern == p)
    }
}

/// SPI implemented by every execution-persistence backend — pyfly's
/// `ExecutionPersistenceProvider` protocol. Object-safe; the async
/// methods box their futures via [`macro@async_trait`].
#[async_trait]
pub trait PersistenceProvider: Send + Sync {
    /// Inserts or replaces the state keyed by its correlation id.
    async fn save(&self, state: ExecutionState) -> Result<(), PersistenceError>;

    /// Loads one execution by correlation id — pyfly's `find`.
    async fn load(&self, correlation_id: &str) -> Result<Option<ExecutionState>, PersistenceError>;

    /// Lists executions matching `filter` — pyfly's `find_all`.
    async fn list(&self, filter: ExecutionFilter) -> Result<Vec<ExecutionState>, PersistenceError>;

    /// Non-terminal executions whose `updated_at` is older than `before` —
    /// pyfly's `find_stale`.
    async fn list_stale(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<ExecutionState>, PersistenceError>;

    /// Atomically claims a single stale execution for recovery.
    ///
    /// Transitions the execution `correlation_id` to `claimed_status`
    /// (bumping `updated_at`) **only if** it is still non-terminal and its
    /// `updated_at` is older than `before` — i.e. it still matches the
    /// [`Self::list_stale`] predicate. Returns the freshly-claimed
    /// [`ExecutionState`] when this caller won the claim, or `Ok(None)` when
    /// the row no longer exists, is already terminal, has been refreshed, or
    /// was claimed by a concurrent recovery pass.
    ///
    /// This is the compare-and-swap guard that makes recovery safe under
    /// overlapping scans: two passes that both observe the same stale row via
    /// [`Self::list_stale`] cannot both run a side-effecting Resume/Compensate
    /// handler, because only one `claim_stale` succeeds — the loser sees
    /// `None` because the winner already bumped `updated_at` past `before`.
    ///
    /// The default implementation is a best-effort load-check-save and is
    /// **not** atomic; durable adapters override it with a conditional update.
    async fn claim_stale(
        &self,
        correlation_id: &str,
        before: DateTime<Utc>,
        claimed_status: ExecutionStatus,
    ) -> Result<Option<ExecutionState>, PersistenceError> {
        let Some(mut state) = self.load(correlation_id).await? else {
            return Ok(None);
        };
        if state.is_terminal() || state.updated_at >= before {
            return Ok(None);
        }
        state.transition(claimed_status);
        self.save(state.clone()).await?;
        Ok(Some(state))
    }

    /// Deletes one execution; `Ok(false)` when absent.
    async fn delete(&self, correlation_id: &str) -> Result<bool, PersistenceError>;

    /// Deletes terminal executions whose completion (falling back to
    /// `updated_at`) is older than `older_than`; returns how many were
    /// removed — pyfly's `cleanup`.
    async fn cleanup(&self, older_than: Duration) -> Result<usize, PersistenceError>;

    /// Liveness probe — pyfly's `is_healthy`.
    async fn is_healthy(&self) -> bool {
        true
    }
}

/// Thread-safe map-backed adapter — pyfly's `InMemoryPersistenceProvider`,
/// the default when nothing else is configured.
#[derive(Debug, Default)]
pub struct MemoryPersistence {
    store: Mutex<HashMap<String, ExecutionState>>,
}

impl MemoryPersistence {
    /// Returns an empty in-memory provider.
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, ExecutionState>> {
        self.store
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }
}

#[async_trait]
impl PersistenceProvider for MemoryPersistence {
    async fn save(&self, state: ExecutionState) -> Result<(), PersistenceError> {
        self.locked().insert(state.correlation_id.clone(), state);
        Ok(())
    }

    async fn load(&self, correlation_id: &str) -> Result<Option<ExecutionState>, PersistenceError> {
        Ok(self.locked().get(correlation_id).cloned())
    }

    async fn list(&self, filter: ExecutionFilter) -> Result<Vec<ExecutionState>, PersistenceError> {
        let mut out: Vec<ExecutionState> = self
            .locked()
            .values()
            .filter(|s| filter.matches(s))
            .cloned()
            .collect();
        out.sort_by_key(|s| s.started_at);
        Ok(out)
    }

    async fn list_stale(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<ExecutionState>, PersistenceError> {
        Ok(self
            .locked()
            .values()
            .filter(|s| !s.is_terminal() && s.updated_at < before)
            .cloned()
            .collect())
    }

    async fn claim_stale(
        &self,
        correlation_id: &str,
        before: DateTime<Utc>,
        claimed_status: ExecutionStatus,
    ) -> Result<Option<ExecutionState>, PersistenceError> {
        // The whole compare-and-swap runs under the store mutex so two
        // overlapping recovery passes cannot both claim the same row.
        let mut store = self.locked();
        let Some(state) = store.get_mut(correlation_id) else {
            return Ok(None);
        };
        if state.is_terminal() || state.updated_at >= before {
            return Ok(None);
        }
        state.transition(claimed_status);
        Ok(Some(state.clone()))
    }

    async fn delete(&self, correlation_id: &str) -> Result<bool, PersistenceError> {
        Ok(self.locked().remove(correlation_id).is_some())
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize, PersistenceError> {
        let cutoff = Utc::now() - older_than;
        let mut store = self.locked();
        let doomed: Vec<String> = store
            .values()
            .filter(|s| s.is_terminal() && s.completed_at.unwrap_or(s.updated_at) < cutoff)
            .map(|s| s.correlation_id.clone())
            .collect();
        for cid in &doomed {
            store.remove(cid);
        }
        Ok(doomed.len())
    }
}

/// Fixed-width UTC timestamp format so lexicographic comparison in SQL
/// equals chronological comparison.
const SQLITE_TS: &str = "%Y-%m-%dT%H:%M:%S%.9f+00:00";

fn encode_ts(ts: DateTime<Utc>) -> String {
    ts.format(SQLITE_TS).to_string()
}

fn decode_ts(raw: &str) -> Result<DateTime<Utc>, PersistenceError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| PersistenceError(format!("bad timestamp {raw:?}: {e}")))
}

/// Durable adapter over an embedded SQLite database — the Rust analogue of
/// pyfly's `SqlAlchemyPersistenceProvider` running on `aiosqlite`, reusing
/// the dev-database pattern established by the migrations crate.
///
/// The schema is created on construction:
///
/// ```sql
/// CREATE TABLE IF NOT EXISTS orchestration_executions (
///     correlation_id TEXT PRIMARY KEY,
///     name           TEXT NOT NULL,
///     pattern        TEXT NOT NULL,
///     status         TEXT NOT NULL,
///     terminal       INTEGER NOT NULL,
///     started_at     TEXT NOT NULL,
///     updated_at     TEXT NOT NULL,
///     completed_at   TEXT,
///     payload        TEXT NOT NULL
/// )
/// ```
///
/// `rusqlite` is synchronous; calls are short and run inline on the
/// caller's task, guarded by a mutex — adequate for the dev/test tier this
/// adapter targets (production deployments plug a server-grade
/// [`PersistenceProvider`] instead).
pub struct SqlitePersistence {
    conn: Mutex<rusqlite::Connection>,
}

impl std::fmt::Debug for SqlitePersistence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlitePersistence").finish_non_exhaustive()
    }
}

impl SqlitePersistence {
    /// Opens (creating if needed) a database file and ensures the schema.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, PersistenceError> {
        let conn = rusqlite::Connection::open(path).map_err(|e| PersistenceError(e.to_string()))?;
        Self::with_connection(conn)
    }

    /// Opens a private in-memory database — handy for tests.
    pub fn open_in_memory() -> Result<Self, PersistenceError> {
        let conn =
            rusqlite::Connection::open_in_memory().map_err(|e| PersistenceError(e.to_string()))?;
        Self::with_connection(conn)
    }

    /// Wraps an existing connection and ensures the schema.
    pub fn with_connection(conn: rusqlite::Connection) -> Result<Self, PersistenceError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS orchestration_executions (
                correlation_id TEXT PRIMARY KEY,
                name           TEXT NOT NULL,
                pattern        TEXT NOT NULL,
                status         TEXT NOT NULL,
                terminal       INTEGER NOT NULL,
                started_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL,
                completed_at   TEXT,
                payload        TEXT NOT NULL
            )",
        )
        .map_err(|e| PersistenceError(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }
}

fn row_to_state(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<ExecutionState, PersistenceError>> {
    let correlation_id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let pattern: String = row.get(2)?;
    let status: String = row.get(3)?;
    let started_at: String = row.get(4)?;
    let updated_at: String = row.get(5)?;
    let completed_at: Option<String> = row.get(6)?;
    let payload: String = row.get(7)?;
    Ok((|| {
        Ok(ExecutionState {
            correlation_id,
            name,
            pattern: match pattern.as_str() {
                "SAGA" => ExecutionPattern::Saga,
                "WORKFLOW" => ExecutionPattern::Workflow,
                "TCC" => ExecutionPattern::Tcc,
                other => return Err(PersistenceError(format!("bad pattern {other:?}"))),
            },
            status: ExecutionStatus::parse(&status)
                .ok_or_else(|| PersistenceError(format!("bad status {status:?}")))?,
            started_at: decode_ts(&started_at)?,
            updated_at: decode_ts(&updated_at)?,
            completed_at: completed_at.as_deref().map(decode_ts).transpose()?,
            payload: serde_json::from_str(&payload)
                .map_err(|e| PersistenceError(format!("bad payload: {e}")))?,
        })
    })())
}

const STATE_COLUMNS: &str =
    "correlation_id, name, pattern, status, started_at, updated_at, completed_at, payload";

fn collect_states(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<ExecutionState>, PersistenceError> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| PersistenceError(e.to_string()))?;
    let rows = stmt
        .query_map(params, row_to_state)
        .map_err(|e| PersistenceError(e.to_string()))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| PersistenceError(e.to_string()))??);
    }
    Ok(out)
}

#[async_trait]
impl PersistenceProvider for SqlitePersistence {
    async fn save(&self, state: ExecutionState) -> Result<(), PersistenceError> {
        let payload = serde_json::to_string(&state.payload)
            .map_err(|e| PersistenceError(format!("encode payload: {e}")))?;
        self.locked()
            .execute(
                "INSERT OR REPLACE INTO orchestration_executions
                 (correlation_id, name, pattern, status, terminal,
                  started_at, updated_at, completed_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    state.correlation_id,
                    state.name,
                    state.pattern.as_str(),
                    state.status.as_str(),
                    state.status.is_terminal() as i64,
                    encode_ts(state.started_at),
                    encode_ts(state.updated_at),
                    state.completed_at.map(encode_ts),
                    payload,
                ],
            )
            .map_err(|e| PersistenceError(e.to_string()))?;
        Ok(())
    }

    async fn load(&self, correlation_id: &str) -> Result<Option<ExecutionState>, PersistenceError> {
        let conn = self.locked();
        let mut found = collect_states(
            &conn,
            &format!(
                "SELECT {STATE_COLUMNS} FROM orchestration_executions WHERE correlation_id = ?1"
            ),
            &[&correlation_id],
        )?;
        Ok(found.pop())
    }

    async fn list(&self, filter: ExecutionFilter) -> Result<Vec<ExecutionState>, PersistenceError> {
        let mut sql = format!("SELECT {STATE_COLUMNS} FROM orchestration_executions WHERE 1 = 1");
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(status) = filter.status {
            sql.push_str(&format!(" AND status = ?{}", params.len() + 1));
            params.push(Box::new(status.as_str().to_string()));
        }
        if let Some(pattern) = filter.pattern {
            sql.push_str(&format!(" AND pattern = ?{}", params.len() + 1));
            params.push(Box::new(pattern.as_str().to_string()));
        }
        sql.push_str(" ORDER BY started_at");
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(AsRef::as_ref).collect();
        collect_states(&self.locked(), &sql, &refs)
    }

    async fn list_stale(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<ExecutionState>, PersistenceError> {
        collect_states(
            &self.locked(),
            &format!(
                "SELECT {STATE_COLUMNS} FROM orchestration_executions
                 WHERE terminal = 0 AND updated_at < ?1"
            ),
            &[&encode_ts(before)],
        )
    }

    async fn claim_stale(
        &self,
        correlation_id: &str,
        before: DateTime<Utc>,
        claimed_status: ExecutionStatus,
    ) -> Result<Option<ExecutionState>, PersistenceError> {
        let now = Utc::now();
        // A terminal claim (e.g. mark-failed) stamps `completed_at`, matching
        // `ExecutionState::transition`; a non-terminal marker leaves it as-is.
        let completed_at = claimed_status.is_terminal().then(|| encode_ts(now));
        let conn = self.locked();
        // Conditional update guarded by the same predicate `list_stale` uses;
        // the held mutex makes the update-then-read atomic for this adapter,
        // so only one overlapping recovery pass can claim a given row.
        let claimed = conn
            .execute(
                "UPDATE orchestration_executions
                 SET status = ?1,
                     terminal = ?2,
                     updated_at = ?3,
                     completed_at = COALESCE(?4, completed_at)
                 WHERE correlation_id = ?5 AND terminal = 0 AND updated_at < ?6",
                rusqlite::params![
                    claimed_status.as_str(),
                    claimed_status.is_terminal() as i64,
                    encode_ts(now),
                    completed_at,
                    correlation_id,
                    encode_ts(before),
                ],
            )
            .map_err(|e| PersistenceError(e.to_string()))?;
        if claimed == 0 {
            return Ok(None);
        }
        let mut found = collect_states(
            &conn,
            &format!(
                "SELECT {STATE_COLUMNS} FROM orchestration_executions WHERE correlation_id = ?1"
            ),
            &[&correlation_id],
        )?;
        Ok(found.pop())
    }

    async fn delete(&self, correlation_id: &str) -> Result<bool, PersistenceError> {
        let n = self
            .locked()
            .execute(
                "DELETE FROM orchestration_executions WHERE correlation_id = ?1",
                rusqlite::params![correlation_id],
            )
            .map_err(|e| PersistenceError(e.to_string()))?;
        Ok(n > 0)
    }

    async fn cleanup(&self, older_than: Duration) -> Result<usize, PersistenceError> {
        let cutoff = encode_ts(Utc::now() - older_than);
        let n = self
            .locked()
            .execute(
                "DELETE FROM orchestration_executions
                 WHERE terminal = 1 AND COALESCE(completed_at, updated_at) < ?1",
                rusqlite::params![cutoff],
            )
            .map_err(|e| PersistenceError(e.to_string()))?;
        Ok(n)
    }

    async fn is_healthy(&self) -> bool {
        self.locked()
            .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn state(name: &str, status: ExecutionStatus) -> ExecutionState {
        let mut s = ExecutionState::new(
            format!("cid-{}", uuid::Uuid::new_v4()),
            name,
            ExecutionPattern::Saga,
        );
        s.transition(status);
        s
    }

    fn providers() -> Vec<Arc<dyn PersistenceProvider>> {
        vec![
            Arc::new(MemoryPersistence::new()),
            Arc::new(SqlitePersistence::open_in_memory().expect("sqlite opens")),
        ]
    }

    // Port of pyfly TestInMemoryProvider::test_save_and_find — run against
    // both the memory and the sqlite adapters.
    #[tokio::test]
    async fn save_and_load_round_trip() {
        for provider in providers() {
            let s = state("t", ExecutionStatus::Running);
            provider.save(s.clone()).await.expect("save");
            let found = provider
                .load(&s.correlation_id)
                .await
                .expect("load")
                .expect("present");
            assert_eq!(found.correlation_id, s.correlation_id);
            assert_eq!(found.status, ExecutionStatus::Running);
            assert_eq!(found.pattern, ExecutionPattern::Saga);
            assert_eq!(found.name, "t");
        }
    }

    // Port of pyfly test_find_all_filtered_by_status.
    #[tokio::test]
    async fn list_filtered_by_status() {
        for provider in providers() {
            provider
                .save(state("t", ExecutionStatus::Running))
                .await
                .unwrap();
            provider
                .save(state("t", ExecutionStatus::Completed))
                .await
                .unwrap();
            let completed = provider
                .list(ExecutionFilter::all().status(ExecutionStatus::Completed))
                .await
                .expect("list");
            assert_eq!(completed.len(), 1);
            assert_eq!(completed[0].status, ExecutionStatus::Completed);
            let everything = provider.list(ExecutionFilter::all()).await.expect("list");
            assert_eq!(everything.len(), 2);
        }
    }

    #[tokio::test]
    async fn list_filtered_by_pattern() {
        for provider in providers() {
            provider
                .save(state("t", ExecutionStatus::Running))
                .await
                .unwrap();
            let mut wf = ExecutionState::new("wf-1", "w", ExecutionPattern::Workflow);
            wf.transition(ExecutionStatus::Running);
            provider.save(wf).await.unwrap();
            let sagas = provider
                .list(ExecutionFilter::all().pattern(ExecutionPattern::Saga))
                .await
                .expect("list");
            assert_eq!(sagas.len(), 1);
            assert_eq!(sagas[0].pattern, ExecutionPattern::Saga);
        }
    }

    // Port of pyfly test_find_stale.
    #[tokio::test]
    async fn list_stale_finds_old_non_terminal() {
        for provider in providers() {
            let mut s = state("t", ExecutionStatus::Running);
            s.updated_at = Utc::now() - Duration::hours(2);
            provider.save(s).await.unwrap();
            // Recent running execution is not stale.
            provider
                .save(state("t", ExecutionStatus::Running))
                .await
                .unwrap();
            // Old but terminal execution is not stale.
            let mut done = state("t", ExecutionStatus::Completed);
            done.updated_at = Utc::now() - Duration::hours(2);
            provider.save(done).await.unwrap();

            let cutoff = Utc::now() - Duration::hours(1);
            let stale = provider.list_stale(cutoff).await.expect("stale");
            assert_eq!(stale.len(), 1);
        }
    }

    // Port of pyfly test_cleanup_removes_terminal_old_records.
    #[tokio::test]
    async fn cleanup_removes_old_terminal_records() {
        for provider in providers() {
            let mut s = state("t", ExecutionStatus::Completed);
            let old = Utc::now() - Duration::days(10);
            s.updated_at = old;
            s.completed_at = Some(old);
            let cid = s.correlation_id.clone();
            provider.save(s).await.unwrap();
            let cleaned = provider.cleanup(Duration::days(7)).await.expect("cleanup");
            assert_eq!(cleaned, 1);
            assert!(provider.load(&cid).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn cleanup_keeps_recent_and_in_flight() {
        for provider in providers() {
            provider
                .save(state("t", ExecutionStatus::Completed))
                .await
                .unwrap();
            let mut running = state("t", ExecutionStatus::Running);
            running.updated_at = Utc::now() - Duration::days(30);
            provider.save(running).await.unwrap();
            let cleaned = provider.cleanup(Duration::days(7)).await.expect("cleanup");
            assert_eq!(cleaned, 0);
        }
    }

    // Port of pyfly test_delete.
    #[tokio::test]
    async fn delete_reports_presence() {
        for provider in providers() {
            let s = state("t", ExecutionStatus::Running);
            let cid = s.correlation_id.clone();
            provider.save(s).await.unwrap();
            assert!(provider.delete(&cid).await.unwrap());
            assert!(!provider.delete("missing").await.unwrap());
        }
    }

    // Port of pyfly test_health.
    #[tokio::test]
    async fn providers_are_healthy() {
        for provider in providers() {
            assert!(provider.is_healthy().await);
        }
    }

    // Rust-specific: save is an upsert keyed by correlation id.
    #[tokio::test]
    async fn save_overwrites_existing_state() {
        for provider in providers() {
            let mut s = state("t", ExecutionStatus::Running);
            let cid = s.correlation_id.clone();
            provider.save(s.clone()).await.unwrap();
            s.transition(ExecutionStatus::Completed);
            provider.save(s).await.unwrap();
            let found = provider.load(&cid).await.unwrap().expect("present");
            assert_eq!(found.status, ExecutionStatus::Completed);
            assert_eq!(
                provider.list(ExecutionFilter::all()).await.unwrap().len(),
                1
            );
        }
    }

    // Regression for Bug 2: claim_stale is an atomic compare-and-swap — only
    // the first claim of a stale row succeeds; a second claim with the same
    // cutoff (the loser of an overlapping recovery pass) sees None because the
    // winner already bumped updated_at past the cutoff. Verified on both the
    // memory and the sqlite adapters.
    #[tokio::test]
    async fn claim_stale_is_a_single_winner_cas() {
        for provider in providers() {
            let mut s = state("t", ExecutionStatus::Running);
            s.updated_at = Utc::now() - Duration::hours(2);
            let cid = s.correlation_id.clone();
            provider.save(s).await.unwrap();

            let cutoff = Utc::now() - Duration::hours(1);
            // First claim wins and transitions to the in-recovery marker.
            let first = provider
                .claim_stale(&cid, cutoff, ExecutionStatus::Compensating)
                .await
                .expect("claim ok");
            let claimed = first.expect("first claim wins");
            assert_eq!(claimed.status, ExecutionStatus::Compensating);

            // Second claim with the same cutoff loses: the row is no longer
            // older than the cutoff (its updated_at was bumped to now).
            let second = provider
                .claim_stale(&cid, cutoff, ExecutionStatus::Compensating)
                .await
                .expect("claim ok");
            assert!(second.is_none(), "second claim must lose the CAS");
        }
    }

    // Regression for Bug 2: a terminal claim (mark-failed) stamps completed_at
    // on both adapters, matching ExecutionState::transition.
    #[tokio::test]
    async fn claim_stale_terminal_marks_completed_at() {
        for provider in providers() {
            let mut s = state("t", ExecutionStatus::Running);
            s.updated_at = Utc::now() - Duration::hours(2);
            let cid = s.correlation_id.clone();
            provider.save(s).await.unwrap();
            let cutoff = Utc::now() - Duration::hours(1);
            let claimed = provider
                .claim_stale(&cid, cutoff, ExecutionStatus::Failed)
                .await
                .expect("claim ok")
                .expect("claim wins");
            assert_eq!(claimed.status, ExecutionStatus::Failed);
            assert!(claimed.completed_at.is_some());
            let reloaded = provider.load(&cid).await.unwrap().expect("present");
            assert!(reloaded.completed_at.is_some());
        }
    }

    // Regression for Bug 2: a row that is no longer stale (refreshed, or
    // already terminal) cannot be claimed.
    #[tokio::test]
    async fn claim_stale_rejects_fresh_and_terminal() {
        for provider in providers() {
            // Fresh (recently updated) running row.
            let fresh = state("t", ExecutionStatus::Running);
            let fresh_cid = fresh.correlation_id.clone();
            provider.save(fresh).await.unwrap();
            // Old but terminal row.
            let mut done = state("t", ExecutionStatus::Completed);
            done.updated_at = Utc::now() - Duration::hours(2);
            done.completed_at = Some(done.updated_at);
            let done_cid = done.correlation_id.clone();
            provider.save(done).await.unwrap();

            let cutoff = Utc::now() - Duration::hours(1);
            assert!(provider
                .claim_stale(&fresh_cid, cutoff, ExecutionStatus::Failed)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .claim_stale(&done_cid, cutoff, ExecutionStatus::Failed)
                .await
                .unwrap()
                .is_none());
            assert!(provider
                .claim_stale("missing", cutoff, ExecutionStatus::Failed)
                .await
                .unwrap()
                .is_none());
        }
    }

    // Rust-specific: the sqlite adapter survives reopening the same file —
    // the durability property pyfly checks on its SQLAlchemy adapter.
    #[tokio::test]
    async fn sqlite_state_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("orchestration.db");
        let s = state("durable", ExecutionStatus::Waiting);
        let cid = s.correlation_id.clone();
        {
            let provider = SqlitePersistence::open(&path).expect("open");
            provider.save(s).await.unwrap();
        }
        let provider = SqlitePersistence::open(&path).expect("reopen");
        let found = provider.load(&cid).await.unwrap().expect("present");
        assert_eq!(found.name, "durable");
        assert_eq!(found.status, ExecutionStatus::Waiting);
    }
}
