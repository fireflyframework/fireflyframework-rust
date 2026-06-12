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

//! Webhook idempotency store — the Rust spelling of pyfly's
//! `webhooks.event_listener.WebhookEventStore` plus its in-memory
//! implementation `InMemoryWebhookEventStore`.
//!
//! The store records which idempotency keys a [`Pipeline`](crate::Pipeline)
//! has already accepted so that a redelivered webhook (same key) is
//! recognised and skipped instead of dispatched a second time. The
//! contract is the two-method `already_processed` / `remember` pair pyfly
//! defines; production deployments supply a distributed implementation
//! (pyfly ships a Redis adapter), while [`MemoryEventStore`] covers tests
//! and single-instance services.

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::WebhookError;

/// The idempotency-store contract — pyfly's `WebhookEventStore`
/// `Protocol`.
///
/// A [`Pipeline`](crate::Pipeline) with a store registered consults it
/// before dispatch: a key reported by [`already_processed`] short-circuits
/// the pipeline (the duplicate is skipped), and a fresh key is recorded
/// with [`remember`] so the next delivery of the same event is recognised.
///
/// Like pyfly's two-step check-then-set, the pair is **not** atomic: for
/// the overwhelming majority of webhook workloads the window between the
/// two calls is negligible and duplicate delivery is rare. Implementations
/// needing once-exactly semantics should serialise the pair behind a
/// distributed lock.
///
/// [`already_processed`]: EventStore::already_processed
/// [`remember`]: EventStore::remember
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Reports whether `idempotency_key` was previously recorded with
    /// [`remember`](EventStore::remember).
    ///
    /// # Errors
    ///
    /// Implementation-specific lookup failures (e.g. a backing-store
    /// outage). The in-memory implementation is infallible.
    async fn already_processed(&self, idempotency_key: &str) -> Result<bool, WebhookError>;

    /// Records `idempotency_key` so a later
    /// [`already_processed`](EventStore::already_processed) call returns
    /// `true`.
    ///
    /// # Errors
    ///
    /// Implementation-specific persistence failures. The in-memory
    /// implementation is infallible.
    async fn remember(&self, idempotency_key: &str) -> Result<(), WebhookError>;
}

/// An in-process [`EventStore`] backed by a mutex-guarded set — the Rust
/// analog of pyfly's `InMemoryWebhookEventStore`.
///
/// Keys accumulate for the lifetime of the process; unlike pyfly's Redis
/// adapter there is no TTL, so a long-lived single-instance service that
/// must bound memory should periodically swap in a fresh store (or use a
/// distributed implementation).
///
/// # Example
///
/// ```
/// # async fn demo() -> Result<(), firefly_webhooks::WebhookError> {
/// use firefly_webhooks::{EventStore, MemoryEventStore};
///
/// let store = MemoryEventStore::new();
/// assert!(!store.already_processed("evt-1").await?);
/// store.remember("evt-1").await?;
/// assert!(store.already_processed("evt-1").await?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct MemoryEventStore {
    seen: Mutex<HashSet<String>>,
}

impl MemoryEventStore {
    /// Returns an empty in-memory idempotency store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of distinct idempotency keys recorded so far.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Reports whether the store has recorded no keys.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl EventStore for MemoryEventStore {
    async fn already_processed(&self, idempotency_key: &str) -> Result<bool, WebhookError> {
        Ok(self.lock().contains(idempotency_key))
    }

    async fn remember(&self, idempotency_key: &str) -> Result<(), WebhookError> {
        self.lock().insert(idempotency_key.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remembers_and_recognises_keys() {
        let store = MemoryEventStore::new();
        assert!(store.is_empty());
        assert!(!store.already_processed("a").await.unwrap());

        store.remember("a").await.unwrap();
        assert!(store.already_processed("a").await.unwrap());
        assert_eq!(store.len(), 1);

        // Idempotent: remembering the same key again does not grow the set.
        store.remember("a").await.unwrap();
        assert_eq!(store.len(), 1);

        // A different key is independent.
        assert!(!store.already_processed("b").await.unwrap());
        store.remember("b").await.unwrap();
        assert_eq!(store.len(), 2);
    }

    #[tokio::test]
    async fn usable_through_the_object_safe_port() {
        let store: std::sync::Arc<dyn EventStore> = std::sync::Arc::new(MemoryEventStore::new());
        assert!(!store.already_processed("x").await.unwrap());
        store.remember("x").await.unwrap();
        assert!(store.already_processed("x").await.unwrap());
    }
}
