//! Default in-memory [`Store`] implementation — the Rust spelling of
//! the Go `callbacks/models` sub-package.

use std::collections::BTreeMap;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::interfaces::{Attempt, CallbackError, Store, Target};

/// MemoryStore is the default [`Store`] implementation —
/// concurrent-safe, suitable for tests and single-instance deployments.
///
/// Targets are keyed by [`Target::id`]; attempts are appended per
/// [`Attempt::event_id`] in arrival order. Unlike Go's map iteration,
/// [`MemoryStore::list_targets`] returns targets in deterministic
/// (ascending id) order — the contract never specified an order, and
/// determinism is friendlier to tests.
#[derive(Debug, Default)]
pub struct MemoryStore {
    targets: RwLock<BTreeMap<String, Target>>,
    attempts: RwLock<BTreeMap<String, Vec<Attempt>>>,
}

impl MemoryStore {
    /// Returns an empty MemoryStore.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn upsert_target(&self, target: Target) -> Result<Target, CallbackError> {
        let mut targets = self.targets.write().expect("targets lock poisoned");
        targets.insert(target.id.clone(), target.clone());
        Ok(target)
    }

    async fn get_target(&self, id: &str) -> Result<Target, CallbackError> {
        let targets = self.targets.read().expect("targets lock poisoned");
        targets.get(id).cloned().ok_or(CallbackError::NotFound)
    }

    async fn list_targets(&self) -> Result<Vec<Target>, CallbackError> {
        let targets = self.targets.read().expect("targets lock poisoned");
        Ok(targets.values().cloned().collect())
    }

    async fn delete_target(&self, id: &str) -> Result<(), CallbackError> {
        let mut targets = self.targets.write().expect("targets lock poisoned");
        targets
            .remove(id)
            .map(|_| ())
            .ok_or(CallbackError::NotFound)
    }

    async fn record_attempt(&self, attempt: Attempt) -> Result<(), CallbackError> {
        let mut attempts = self.attempts.write().expect("attempts lock poisoned");
        attempts
            .entry(attempt.event_id.clone())
            .or_default()
            .push(attempt);
        Ok(())
    }

    async fn list_attempts(&self, event_id: &str) -> Result<Vec<Attempt>, CallbackError> {
        let attempts = self.attempts.read().expect("attempts lock poisoned");
        Ok(attempts.get(event_id).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str) -> Target {
        Target {
            id: id.into(),
            url: format!("https://example.com/{id}"),
            active: true,
            ..Target::default()
        }
    }

    #[tokio::test]
    async fn upsert_get_list_delete_roundtrip() {
        let store = MemoryStore::new();
        let saved = store.upsert_target(target("t1")).await.unwrap();
        assert_eq!(saved.id, "t1");

        let got = store.get_target("t1").await.unwrap();
        assert_eq!(got, saved);

        store.upsert_target(target("t2")).await.unwrap();
        let list = store.list_targets().await.unwrap();
        assert_eq!(list.len(), 2);
        // Deterministic ascending-id order.
        assert_eq!(list[0].id, "t1");
        assert_eq!(list[1].id, "t2");

        store.delete_target("t1").await.unwrap();
        assert_eq!(store.list_targets().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn upsert_replaces_existing_target() {
        let store = MemoryStore::new();
        store.upsert_target(target("t1")).await.unwrap();
        let mut updated = target("t1");
        updated.active = false;
        store.upsert_target(updated).await.unwrap();
        let got = store.get_target("t1").await.unwrap();
        assert!(!got.active);
        assert_eq!(store.list_targets().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_target_is_not_found() {
        let store = MemoryStore::new();
        let err = store.get_target("nope").await.unwrap_err();
        assert_eq!(err, CallbackError::NotFound);
        let err = store.delete_target("nope").await.unwrap_err();
        assert_eq!(err, CallbackError::NotFound);
    }

    #[tokio::test]
    async fn attempts_append_in_order_per_event() {
        let store = MemoryStore::new();
        for n in 1..=3u32 {
            store
                .record_attempt(Attempt {
                    id: format!("a{n}"),
                    event_id: "ev1".into(),
                    target_id: "t1".into(),
                    attempt: n,
                    ..Attempt::default()
                })
                .await
                .unwrap();
        }
        store
            .record_attempt(Attempt {
                id: "b1".into(),
                event_id: "ev2".into(),
                ..Attempt::default()
            })
            .await
            .unwrap();

        let atts = store.list_attempts("ev1").await.unwrap();
        assert_eq!(atts.len(), 3);
        assert_eq!(
            atts.iter().map(|a| a.attempt).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(store.list_attempts("ev2").await.unwrap().len(), 1);
        assert!(store.list_attempts("unknown").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_attempts_returns_a_copy() {
        let store = MemoryStore::new();
        store
            .record_attempt(Attempt {
                id: "a1".into(),
                event_id: "ev1".into(),
                ..Attempt::default()
            })
            .await
            .unwrap();
        let mut copy = store.list_attempts("ev1").await.unwrap();
        copy.clear();
        assert_eq!(store.list_attempts("ev1").await.unwrap().len(), 1);
    }
}
