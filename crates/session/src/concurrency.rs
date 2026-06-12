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

//! Session concurrency control — the Rust port of pyfly's
//! `session.concurrency`, itself Spring Security's `maximumSessions`.
//!
//! Limits the number of concurrent sessions per authenticated principal.
//! When the cap is exceeded a new login is either rejected
//! ([`Strategy::RejectNew`]) or the oldest session is evicted
//! ([`Strategy::EvictOldest`]). Enforced at the single point where a
//! principal becomes bound to a session (login). With no cap configured
//! ([`ConcurrencyPolicy::max_sessions`] `< 0`) the registry is unused and
//! behavior is unchanged.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::store::SessionStore;

/// The eviction strategy applied when a principal exceeds the session cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Evict the oldest session(s) so the new login fits under the cap
    /// (pyfly `"evict-oldest"`). Default.
    #[default]
    EvictOldest,
    /// Reject the new login when the cap is reached (pyfly `"reject-new"`).
    RejectNew,
}

impl Strategy {
    /// Parses a pyfly-style strategy string (`"evict-oldest"` /
    /// `"reject-new"`); unknown values fall back to [`Strategy::EvictOldest`]
    /// (matching pyfly's "anything not reject-new evicts" behavior).
    #[must_use]
    pub fn from_str_lenient(s: &str) -> Self {
        match s {
            "reject-new" => Strategy::RejectNew,
            _ => Strategy::EvictOldest,
        }
    }
}

/// Concurrency cap configuration — the Rust port of pyfly's
/// `ConcurrencyControlPolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyPolicy {
    /// Maximum concurrent sessions per principal. `< 0` means unlimited
    /// (default; behavior unchanged), matching pyfly's `-1`.
    pub max_sessions: i32,
    /// What to do when the cap is reached.
    pub strategy: Strategy,
}

impl Default for ConcurrencyPolicy {
    fn default() -> Self {
        Self {
            max_sessions: -1,
            strategy: Strategy::EvictOldest,
        }
    }
}

/// Per-principal index of live session ids, kept separate from the
/// [`SessionStore`] — the Rust port of pyfly's `SessionRegistry` protocol.
/// `created_at` is an epoch-millis timestamp; [`Self::list_sessions`]
/// returns entries oldest-first.
#[async_trait]
pub trait SessionRegistry: Send + Sync {
    /// Records `session_id` for `principal` with creation time `created_at`.
    async fn register(&self, principal: &str, session_id: &str, created_at: i64);

    /// Drops `session_id` from `principal`'s set (idempotent).
    async fn deregister(&self, principal: &str, session_id: &str);

    /// `(session_id, created_at)` for `principal`, **oldest first**.
    async fn list_sessions(&self, principal: &str) -> Vec<(String, i64)>;

    /// The number of live sessions for `principal`.
    async fn count(&self, principal: &str) -> usize {
        self.list_sessions(principal).await.len()
    }
}

/// In-process [`SessionRegistry`] — the Rust port of pyfly's
/// `InMemorySessionRegistry`. Prunes a principal's bucket when its last
/// session is deregistered (matching pyfly).
#[derive(Default)]
pub struct MemorySessionRegistry {
    by_principal: Mutex<HashMap<String, HashMap<String, i64>>>,
}

impl MemorySessionRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionRegistry for MemorySessionRegistry {
    async fn register(&self, principal: &str, session_id: &str, created_at: i64) {
        self.by_principal
            .lock()
            .await
            .entry(principal.to_string())
            .or_default()
            .insert(session_id.to_string(), created_at);
    }

    async fn deregister(&self, principal: &str, session_id: &str) {
        let mut map = self.by_principal.lock().await;
        if let Some(sessions) = map.get_mut(principal) {
            sessions.remove(session_id);
            if sessions.is_empty() {
                map.remove(principal);
            }
        }
    }

    async fn list_sessions(&self, principal: &str) -> Vec<(String, i64)> {
        let map = self.by_principal.lock().await;
        let mut sessions: Vec<(String, i64)> = map
            .get(principal)
            .map(|s| s.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .unwrap_or_default();
        // Oldest first; tie-break by id for determinism.
        sessions.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        sessions
    }

    async fn count(&self, principal: &str) -> usize {
        self.by_principal
            .lock()
            .await
            .get(principal)
            .map_or(0, HashMap::len)
    }
}

/// Enforces a per-principal session cap on login and cleans up on logout —
/// the Rust port of pyfly's `SessionConcurrencyController`.
///
/// On [`Strategy::EvictOldest`], evicted sessions are also removed from the
/// optional [`SessionStore`] (`session_store`), the analog of pyfly's
/// `session_deleter` callback.
pub struct SessionConcurrencyController {
    registry: Arc<dyn SessionRegistry>,
    policy: ConcurrencyPolicy,
    session_store: Option<Arc<dyn SessionStore>>,
}

impl SessionConcurrencyController {
    /// Creates a controller over `registry` with `policy` and no store
    /// deleter (evicted sessions are deregistered but their store entries
    /// are left to expire naturally).
    #[must_use]
    pub fn new(registry: Arc<dyn SessionRegistry>, policy: ConcurrencyPolicy) -> Self {
        Self {
            registry,
            policy,
            session_store: None,
        }
    }

    /// Sets the [`SessionStore`] whose entries are deleted when a session is
    /// evicted under [`Strategy::EvictOldest`] — pyfly's `session_deleter`.
    #[must_use]
    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Registers a new session, enforcing the cap. Returns `false` when the
    /// login is rejected (only under [`Strategy::RejectNew`] over the cap).
    ///
    /// A store-delete failure during eviction is logged and otherwise
    /// ignored — the cap is still enforced in the registry (the store entry
    /// will expire by TTL).
    pub async fn on_login(&self, principal: &str, session_id: &str, created_at: i64) -> bool {
        if self.policy.max_sessions < 0 {
            self.registry
                .register(principal, session_id, created_at)
                .await;
            return true;
        }

        let existing: Vec<(String, i64)> = self
            .registry
            .list_sessions(principal)
            .await
            .into_iter()
            .filter(|(sid, _)| sid != session_id)
            .collect();

        let cap = self.policy.max_sessions as usize;
        // pyfly: `len(existing) + 1 <= max_sessions` — i.e. the new session
        // still fits under the cap.
        if existing.len() < cap {
            self.registry
                .register(principal, session_id, created_at)
                .await;
            return true;
        }

        if self.policy.strategy == Strategy::RejectNew {
            tracing::info!(
                principal,
                max_sessions = self.policy.max_sessions,
                "rejected login: max concurrent sessions reached"
            );
            return false;
        }

        // evict-oldest: drop the oldest sessions until the new one fits.
        let to_evict = existing.len() + 1 - cap;
        for (sid, _) in existing.into_iter().take(to_evict) {
            if let Some(store) = &self.session_store {
                if let Err(e) = store.delete(&sid).await {
                    tracing::warn!(session_id = %sid, error = %e, "failed to delete evicted session from store");
                }
            }
            self.registry.deregister(principal, &sid).await;
        }
        self.registry
            .register(principal, session_id, created_at)
            .await;
        true
    }

    /// Deregisters `session_id` from `principal` on logout.
    pub async fn on_logout(&self, principal: &str, session_id: &str) {
        self.registry.deregister(principal, session_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_tracks_sessions_oldest_first() {
        // pyfly: test_registry_tracks_sessions_oldest_first
        let reg = MemorySessionRegistry::new();
        reg.register("alice", "s1", 1).await;
        reg.register("alice", "s2", 2).await;
        reg.register("bob", "s3", 5).await;

        assert_eq!(reg.count("alice").await, 2);
        let ids: Vec<String> = reg
            .list_sessions("alice")
            .await
            .into_iter()
            .map(|(sid, _)| sid)
            .collect();
        assert_eq!(ids, vec!["s1", "s2"]);

        reg.deregister("alice", "s1").await;
        assert_eq!(reg.count("alice").await, 1);
        reg.deregister("bob", "s3").await;
        assert_eq!(reg.count("bob").await, 0); // bucket pruned
    }

    #[tokio::test]
    async fn unlimited_always_allows() {
        // pyfly: test_unlimited_always_allows
        let reg = Arc::new(MemorySessionRegistry::new());
        let ctl = SessionConcurrencyController::new(reg.clone(), ConcurrencyPolicy::default());
        for i in 0..5 {
            assert!(ctl.on_login("alice", &format!("s{i}"), i).await);
        }
        assert_eq!(reg.count("alice").await, 5);
    }

    #[tokio::test]
    async fn reject_new_strategy() {
        // pyfly: test_reject_new_strategy
        let reg = Arc::new(MemorySessionRegistry::new());
        let ctl = SessionConcurrencyController::new(
            reg.clone(),
            ConcurrencyPolicy {
                max_sessions: 2,
                strategy: Strategy::RejectNew,
            },
        );
        assert!(ctl.on_login("alice", "s1", 1).await);
        assert!(ctl.on_login("alice", "s2", 2).await);
        assert!(!ctl.on_login("alice", "s3", 3).await); // over cap -> rejected
        let ids: std::collections::BTreeSet<String> = reg
            .list_sessions("alice")
            .await
            .into_iter()
            .map(|(sid, _)| sid)
            .collect();
        assert_eq!(
            ids,
            ["s1".to_string(), "s2".to_string()].into_iter().collect()
        );
    }

    #[tokio::test]
    async fn evict_oldest_strategy_deletes_evicted_session() {
        // pyfly: test_evict_oldest_strategy_deletes_evicted_session
        use crate::store::MemorySessionStore;
        use std::time::Duration;

        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        store
            .save("s1", &HashMap::new(), Duration::from_secs(60))
            .await
            .unwrap();

        let reg = Arc::new(MemorySessionRegistry::new());
        let ctl = SessionConcurrencyController::new(
            reg.clone(),
            ConcurrencyPolicy {
                max_sessions: 2,
                strategy: Strategy::EvictOldest,
            },
        )
        .with_session_store(store.clone());

        assert!(ctl.on_login("alice", "s1", 1).await);
        assert!(ctl.on_login("alice", "s2", 2).await);
        assert!(ctl.on_login("alice", "s3", 3).await); // evicts oldest (s1)

        // s1 purged from the store.
        assert!(!store.exists("s1").await.unwrap());
        let ids: std::collections::BTreeSet<String> = reg
            .list_sessions("alice")
            .await
            .into_iter()
            .map(|(sid, _)| sid)
            .collect();
        assert_eq!(
            ids,
            ["s2".to_string(), "s3".to_string()].into_iter().collect()
        );
        assert_eq!(reg.count("alice").await, 2); // cap held
    }

    #[tokio::test]
    async fn on_logout_deregisters() {
        // pyfly: test_on_logout_deregisters
        let reg = Arc::new(MemorySessionRegistry::new());
        let ctl = SessionConcurrencyController::new(
            reg.clone(),
            ConcurrencyPolicy {
                max_sessions: 5,
                strategy: Strategy::EvictOldest,
            },
        );
        ctl.on_login("alice", "s1", 1).await;
        ctl.on_logout("alice", "s1").await;
        assert_eq!(reg.count("alice").await, 0);
    }

    #[test]
    fn strategy_parse_lenient() {
        assert_eq!(
            Strategy::from_str_lenient("reject-new"),
            Strategy::RejectNew
        );
        assert_eq!(
            Strategy::from_str_lenient("evict-oldest"),
            Strategy::EvictOldest
        );
        assert_eq!(Strategy::from_str_lenient("garbage"), Strategy::EvictOldest);
    }
}
