//! Dead-letter queue: capture executions that fail terminally for
//! offline review — pyfly's `pyfly.transactional.core.dlq`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::persistence::PersistenceError;

/// One captured failed execution / step — pyfly's `DeadLetterEntry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadLetterEntry {
    /// Unique entry id (UUID v4).
    pub id: String,
    /// Name of the saga / workflow / TCC definition that failed.
    pub execution_name: String,
    /// Correlation id of the failed run.
    pub correlation_id: String,
    /// The failing step, when the failure is step-scoped.
    pub step_id: Option<String>,
    /// Error class / variant name (pyfly stores `type(error).__name__`).
    pub error_type: String,
    /// Rendered error message.
    pub error_message: String,
    /// UTC instant the entry was captured.
    pub timestamp: DateTime<Utc>,
    /// How many times an operator retried this entry.
    pub retry_count: u32,
    /// The execution input at failure time, for replay.
    pub input: serde_json::Value,
}

/// Builder-style description of a failure handed to
/// [`DeadLetterService::capture`] — the Rust spelling of pyfly's
/// keyword-argument `capture(...)` call.
#[derive(Debug, Clone)]
pub struct DeadLetterCapture {
    execution_name: String,
    correlation_id: String,
    step_id: Option<String>,
    error_type: String,
    error_message: String,
    input: serde_json::Value,
}

impl DeadLetterCapture {
    /// Starts a capture for one failed execution. `error_type` defaults to
    /// `"Error"`; set it with [`Self::error_type`].
    pub fn new(
        execution_name: impl Into<String>,
        correlation_id: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            execution_name: execution_name.into(),
            correlation_id: correlation_id.into(),
            step_id: None,
            error_type: "Error".to_string(),
            error_message: error_message.into(),
            input: serde_json::Value::Null,
        }
    }

    /// Names the error class / variant (pyfly's `type(error).__name__`).
    #[must_use]
    pub fn error_type(mut self, error_type: impl Into<String>) -> Self {
        self.error_type = error_type.into();
        self
    }

    /// Scopes the failure to one step.
    #[must_use]
    pub fn step_id(mut self, step_id: impl Into<String>) -> Self {
        self.step_id = Some(step_id.into());
        self
    }

    /// Attaches the execution input for later replay.
    #[must_use]
    pub fn input(mut self, input: serde_json::Value) -> Self {
        self.input = input;
        self
    }
}

/// Optional filters for [`DeadLetterStore::list`] — pyfly's keyword
/// arguments `execution_name=` / `correlation_id=`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeadLetterFilter {
    /// Only entries from this execution definition.
    pub execution_name: Option<String>,
    /// Only entries from this run.
    pub correlation_id: Option<String>,
}

impl DeadLetterFilter {
    /// No filtering — every entry.
    pub fn all() -> Self {
        Self::default()
    }

    /// Restricts to one execution definition.
    #[must_use]
    pub fn execution_name(mut self, name: impl Into<String>) -> Self {
        self.execution_name = Some(name.into());
        self
    }

    /// Restricts to one correlation id.
    #[must_use]
    pub fn correlation_id(mut self, cid: impl Into<String>) -> Self {
        self.correlation_id = Some(cid.into());
        self
    }

    fn matches(&self, entry: &DeadLetterEntry) -> bool {
        self.execution_name
            .as_deref()
            .is_none_or(|n| entry.execution_name == n)
            && self
                .correlation_id
                .as_deref()
                .is_none_or(|c| entry.correlation_id == c)
    }
}

/// SPI for persisting dead-letter entries — pyfly's `DeadLetterStore`
/// protocol. Object-safe; async methods box their futures via
/// [`macro@async_trait`].
#[async_trait]
pub trait DeadLetterStore: Send + Sync {
    /// Inserts or replaces an entry keyed by its id.
    async fn add(&self, entry: DeadLetterEntry) -> Result<(), PersistenceError>;
    /// Loads one entry by id.
    async fn get(&self, entry_id: &str) -> Result<Option<DeadLetterEntry>, PersistenceError>;
    /// Lists entries matching `filter`, newest first.
    async fn list(
        &self,
        filter: DeadLetterFilter,
    ) -> Result<Vec<DeadLetterEntry>, PersistenceError>;
    /// Deletes one entry; `Ok(false)` when absent.
    async fn delete(&self, entry_id: &str) -> Result<bool, PersistenceError>;
    /// Deletes every entry, returning how many were removed.
    async fn clear(&self) -> Result<usize, PersistenceError>;
    /// Number of stored entries.
    async fn count(&self) -> Result<usize, PersistenceError>;
}

/// Default DLQ adapter backed by a map — pyfly's
/// `InMemoryDeadLetterStore`.
#[derive(Debug, Default)]
pub struct MemoryDeadLetterStore {
    store: Mutex<HashMap<String, DeadLetterEntry>>,
}

impl MemoryDeadLetterStore {
    /// Returns an empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, DeadLetterEntry>> {
        self.store
            .lock()
            .expect("firefly/orchestration: lock poisoned")
    }
}

#[async_trait]
impl DeadLetterStore for MemoryDeadLetterStore {
    async fn add(&self, entry: DeadLetterEntry) -> Result<(), PersistenceError> {
        self.locked().insert(entry.id.clone(), entry);
        Ok(())
    }

    async fn get(&self, entry_id: &str) -> Result<Option<DeadLetterEntry>, PersistenceError> {
        Ok(self.locked().get(entry_id).cloned())
    }

    async fn list(
        &self,
        filter: DeadLetterFilter,
    ) -> Result<Vec<DeadLetterEntry>, PersistenceError> {
        let mut entries: Vec<DeadLetterEntry> = self
            .locked()
            .values()
            .filter(|e| filter.matches(e))
            .cloned()
            .collect();
        // Newest first, like pyfly's sorted(..., reverse=True).
        entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        Ok(entries)
    }

    async fn delete(&self, entry_id: &str) -> Result<bool, PersistenceError> {
        Ok(self.locked().remove(entry_id).is_some())
    }

    async fn clear(&self) -> Result<usize, PersistenceError> {
        let mut store = self.locked();
        let n = store.len();
        store.clear();
        Ok(n)
    }

    async fn count(&self) -> Result<usize, PersistenceError> {
        Ok(self.locked().len())
    }
}

/// High-level facade orchestration components call into — pyfly's
/// `DeadLetterService`.
#[derive(Clone)]
pub struct DeadLetterService {
    store: Arc<dyn DeadLetterStore>,
}

impl std::fmt::Debug for DeadLetterService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeadLetterService").finish_non_exhaustive()
    }
}

impl Default for DeadLetterService {
    fn default() -> Self {
        Self::new(Arc::new(MemoryDeadLetterStore::new()))
    }
}

impl DeadLetterService {
    /// Wraps a concrete store; [`DeadLetterService::default`] uses the
    /// in-memory adapter, like pyfly's `DeadLetterService()`.
    pub fn new(store: Arc<dyn DeadLetterStore>) -> Self {
        Self { store }
    }

    /// Records one failure and returns the stored entry.
    pub async fn capture(
        &self,
        capture: DeadLetterCapture,
    ) -> Result<DeadLetterEntry, PersistenceError> {
        let entry = DeadLetterEntry {
            id: uuid::Uuid::new_v4().to_string(),
            execution_name: capture.execution_name,
            correlation_id: capture.correlation_id,
            step_id: capture.step_id,
            error_type: capture.error_type,
            error_message: capture.error_message,
            timestamp: Utc::now(),
            retry_count: 0,
            input: capture.input,
        };
        self.store.add(entry.clone()).await?;
        Ok(entry)
    }

    /// Lists entries matching `filter`, newest first.
    pub async fn list(
        &self,
        filter: DeadLetterFilter,
    ) -> Result<Vec<DeadLetterEntry>, PersistenceError> {
        self.store.list(filter).await
    }

    /// Loads one entry by id.
    pub async fn get(&self, entry_id: &str) -> Result<Option<DeadLetterEntry>, PersistenceError> {
        self.store.get(entry_id).await
    }

    /// Number of dead-letter entries currently stored (pyfly audit #167).
    pub async fn count(&self) -> Result<usize, PersistenceError> {
        self.store.count().await
    }

    /// Increments an entry's retry counter; `Ok(false)` when the entry is
    /// missing.
    pub async fn mark_retried(&self, entry_id: &str) -> Result<bool, PersistenceError> {
        let Some(mut entry) = self.store.get(entry_id).await? else {
            return Ok(false);
        };
        entry.retry_count += 1;
        self.store.add(entry).await?;
        Ok(true)
    }

    /// Deletes one entry; `Ok(false)` when absent.
    pub async fn delete(&self, entry_id: &str) -> Result<bool, PersistenceError> {
        self.store.delete(entry_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> DeadLetterService {
        DeadLetterService::new(Arc::new(MemoryDeadLetterStore::new()))
    }

    // Port of pyfly test_capture_and_list.
    #[tokio::test]
    async fn capture_and_list() {
        let dlq = service();
        dlq.capture(
            DeadLetterCapture::new("orderSaga", "cid-1", "payment_declined")
                .error_type("RuntimeError")
                .step_id("charge")
                .input(serde_json::json!({"order": 1})),
        )
        .await
        .expect("capture");
        let entries = dlq.list(DeadLetterFilter::all()).await.expect("list");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.execution_name, "orderSaga");
        assert_eq!(e.error_type, "RuntimeError");
        assert_eq!(e.step_id.as_deref(), Some("charge"));
        assert_eq!(e.input, serde_json::json!({"order": 1}));
        assert_eq!(e.retry_count, 0);
    }

    // Port of pyfly test_filter_by_execution_name.
    #[tokio::test]
    async fn filter_by_execution_name() {
        let dlq = service();
        dlq.capture(DeadLetterCapture::new("a", "1", "x"))
            .await
            .unwrap();
        dlq.capture(DeadLetterCapture::new("b", "2", "y"))
            .await
            .unwrap();
        let only_a = dlq
            .list(DeadLetterFilter::all().execution_name("a"))
            .await
            .unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].execution_name, "a");
        let only_2 = dlq
            .list(DeadLetterFilter::all().correlation_id("2"))
            .await
            .unwrap();
        assert_eq!(only_2.len(), 1);
        assert_eq!(only_2[0].correlation_id, "2");
    }

    // Port of pyfly test_mark_retried_increments_count.
    #[tokio::test]
    async fn mark_retried_increments_count() {
        let dlq = service();
        let entry = dlq
            .capture(DeadLetterCapture::new("x", "1", "e"))
            .await
            .unwrap();
        assert!(dlq.mark_retried(&entry.id).await.unwrap());
        let refreshed = dlq.get(&entry.id).await.unwrap().expect("present");
        assert_eq!(refreshed.retry_count, 1);
        assert!(!dlq.mark_retried("missing").await.unwrap());
    }

    // Port of pyfly test_delete_returns_false_when_missing.
    #[tokio::test]
    async fn delete_returns_false_when_missing() {
        let dlq = service();
        assert!(!dlq.delete("nope").await.unwrap());
    }

    // Port of pyfly TestDeadLetterCount::test_service_count.
    #[tokio::test]
    async fn count_reflects_captures() {
        let dlq = DeadLetterService::default();
        assert_eq!(dlq.count().await.unwrap(), 0);
        dlq.capture(DeadLetterCapture::new("x", "c", "e"))
            .await
            .unwrap();
        assert_eq!(dlq.count().await.unwrap(), 1);
    }

    // Rust-specific: clear empties the store and reports the count.
    #[tokio::test]
    async fn clear_reports_removed() {
        let store = Arc::new(MemoryDeadLetterStore::new());
        let dlq = DeadLetterService::new(store.clone());
        dlq.capture(DeadLetterCapture::new("x", "1", "e"))
            .await
            .unwrap();
        dlq.capture(DeadLetterCapture::new("y", "2", "e"))
            .await
            .unwrap();
        assert_eq!(store.clear().await.unwrap(), 2);
        assert_eq!(dlq.count().await.unwrap(), 0);
    }

    // Rust-specific: list is newest-first, like pyfly's reverse sort.
    #[tokio::test]
    async fn list_is_newest_first() {
        let store = MemoryDeadLetterStore::new();
        let mut older = DeadLetterEntry {
            id: "old".into(),
            execution_name: "x".into(),
            correlation_id: "c".into(),
            step_id: None,
            error_type: "Error".into(),
            error_message: "e".into(),
            timestamp: Utc::now() - chrono::Duration::minutes(5),
            retry_count: 0,
            input: serde_json::Value::Null,
        };
        store.add(older.clone()).await.unwrap();
        older.id = "new".into();
        older.timestamp = Utc::now();
        store.add(older).await.unwrap();
        let entries = store.list(DeadLetterFilter::all()).await.unwrap();
        assert_eq!(entries[0].id, "new");
        assert_eq!(entries[1].id, "old");
    }
}
