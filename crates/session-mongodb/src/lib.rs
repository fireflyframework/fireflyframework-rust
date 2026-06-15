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

//! # firefly-session-mongodb
//!
//! A **MongoDB-backed distributed [`SessionRegistry`]** — the document-store
//! sibling of [`firefly-session-postgres`] and [`firefly-session-redis`], for
//! enforcing a maximum-concurrent-sessions policy across every instance of a
//! horizontally-scaled service.
//!
//! Each `(principal, session_id, created_at)` triple is one document in a
//! sessions collection (keyed uniquely by `session_id`). [`register`] upserts,
//! [`deregister`] deletes, [`list_sessions`] returns the principal's sessions
//! **oldest-first**, and [`count`] counts them — exactly the contract the
//! [`SessionConcurrencyController`](firefly_session::SessionConcurrencyController)
//! consults to evict the oldest session when a login would exceed the cap.
//!
//! The [`SessionRegistry`] trait is **infallible by contract** (a backend
//! hiccup must not fail a login), so every method here logs and swallows a
//! MongoDB error rather than propagating it — the concurrency cap simply is
//! not enforced for that one operation. Constructors return [`RegistryError`].
//!
//! ```no_run
//! use firefly_session_mongodb::MongoSessionRegistry;
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = MongoSessionRegistry::connect("mongodb://localhost:27017").await?;
//! registry.init().await?; // create the indexes (optional but recommended)
//! # let _ = registry;
//! # Ok(())
//! # }
//! ```
//!
//! [`register`]: SessionRegistry::register
//! [`deregister`]: SessionRegistry::deregister
//! [`list_sessions`]: SessionRegistry::list_sessions
//! [`count`]: SessionRegistry::count
//! [`firefly-session-postgres`]: https://docs.rs/firefly-session-postgres
//! [`firefly-session-redis`]: https://docs.rs/firefly-session-redis

#![warn(missing_docs)]

use async_trait::async_trait;
use futures::TryStreamExt;
use mongodb::bson::{doc, Document};
use mongodb::options::{FindOptions, IndexOptions, ReplaceOptions};
use mongodb::{Client, Collection, IndexModel};

use firefly_session::SessionRegistry;

/// The default database used when the connection string carries none.
const DEFAULT_DB: &str = "firefly";
/// The default collection holding the session-registry documents.
const DEFAULT_COLLECTION: &str = "firefly_sessions";

const FIELD_SESSION_ID: &str = "session_id";
const FIELD_PRINCIPAL: &str = "principal";
const FIELD_CREATED_AT: &str = "created_at";

/// Framework version stamp.
pub const VERSION: &str = "26.6.9";

/// A MongoDB-backed [`SessionRegistry`].
///
/// Build it from a connection string with [`connect`](MongoSessionRegistry::connect)
/// / [`connect_with`](MongoSessionRegistry::connect_with), or from an
/// already-resolved [`Collection`] with
/// [`from_collection`](MongoSessionRegistry::from_collection) (the DI entry point).
#[derive(Clone)]
pub struct MongoSessionRegistry {
    collection: Collection<Document>,
}

impl std::fmt::Debug for MongoSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MongoSessionRegistry")
            .field("collection", &self.collection.name())
            .finish()
    }
}

impl MongoSessionRegistry {
    /// Connects to `uri` and uses the connection string's default database (or
    /// `firefly` when none is given) and the `firefly_sessions` collection.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Connection`] when the URI is malformed or the
    /// client cannot be constructed.
    pub async fn connect(uri: &str) -> Result<Self, RegistryError> {
        let client = Client::with_uri_str(uri)
            .await
            .map_err(|e| RegistryError::Connection(e.to_string()))?;
        let db = client
            .default_database()
            .unwrap_or_else(|| client.database(DEFAULT_DB));
        Ok(Self::from_collection(
            db.collection::<Document>(DEFAULT_COLLECTION),
        ))
    }

    /// Like [`connect`](MongoSessionRegistry::connect) but targets an explicit
    /// `database` and `collection`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Connection`] when the URI is malformed or the
    /// client cannot be constructed.
    pub async fn connect_with(
        uri: &str,
        database: &str,
        collection: &str,
    ) -> Result<Self, RegistryError> {
        let client = Client::with_uri_str(uri)
            .await
            .map_err(|e| RegistryError::Connection(e.to_string()))?;
        Ok(Self::from_collection(
            client.database(database).collection::<Document>(collection),
        ))
    }

    /// Builds the registry over an already-resolved
    /// [`Collection`] — the DI entry point when the application already owns a
    /// `mongodb::Client`.
    pub fn from_collection(collection: Collection<Document>) -> Self {
        Self { collection }
    }

    /// The backing collection.
    pub fn collection(&self) -> &Collection<Document> {
        &self.collection
    }

    /// Creates the supporting indexes: a unique index on `session_id` (so an
    /// upsert is keyed) and a non-unique index on `principal` (so the
    /// per-principal lookups are efficient). Idempotent; safe to call at
    /// startup.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Backend`] when an index cannot be created.
    pub async fn init(&self) -> Result<(), RegistryError> {
        let unique_session = IndexModel::builder()
            .keys(doc! { FIELD_SESSION_ID: 1 })
            .options(IndexOptions::builder().unique(true).build())
            .build();
        let by_principal = IndexModel::builder()
            .keys(doc! { FIELD_PRINCIPAL: 1 })
            .build();
        self.collection
            .create_index(unique_session)
            .await
            .map_err(|e| RegistryError::Backend(e.to_string()))?;
        self.collection
            .create_index(by_principal)
            .await
            .map_err(|e| RegistryError::Backend(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl SessionRegistry for MongoSessionRegistry {
    /// Upserts the `(principal, session_id, created_at)` document keyed by
    /// `session_id` — the document-store analogue of an `INSERT … ON CONFLICT
    /// (session_id) DO UPDATE`. Infallible by contract: a backend failure is
    /// logged and swallowed (the cap simply isn't enforced for this login).
    async fn register(&self, principal: &str, session_id: &str, created_at: i64) {
        let replacement = doc! {
            FIELD_SESSION_ID: session_id,
            FIELD_PRINCIPAL: principal,
            FIELD_CREATED_AT: created_at,
        };
        if let Err(e) = self
            .collection
            .replace_one(doc! { FIELD_SESSION_ID: session_id }, replacement)
            .with_options(ReplaceOptions::builder().upsert(true).build())
            .await
        {
            tracing::warn!(principal, session_id, error = %e, "session-mongodb: register upsert failed; concurrency cap not enforced for this login");
        }
    }

    /// Deletes the `session_id` document for `principal` (idempotent). A
    /// backend failure is logged and swallowed.
    async fn deregister(&self, principal: &str, session_id: &str) {
        if let Err(e) = self
            .collection
            .delete_one(doc! { FIELD_PRINCIPAL: principal, FIELD_SESSION_ID: session_id })
            .await
        {
            tracing::warn!(principal, session_id, error = %e, "session-mongodb: deregister failed");
        }
    }

    /// Returns `(session_id, created_at)` for `principal`, **oldest first**
    /// (`created_at` ascending). A backend failure is logged and yields an
    /// empty list.
    async fn list_sessions(&self, principal: &str) -> Vec<(String, i64)> {
        let options = FindOptions::builder()
            .sort(doc! { FIELD_CREATED_AT: 1 })
            .build();
        let cursor = match self
            .collection
            .find(doc! { FIELD_PRINCIPAL: principal })
            .with_options(options)
            .await
        {
            Ok(cursor) => cursor,
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-mongodb: list_sessions query failed");
                return Vec::new();
            }
        };
        let docs: Vec<Document> = match cursor.try_collect().await {
            Ok(docs) => docs,
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-mongodb: list_sessions cursor failed");
                return Vec::new();
            }
        };
        docs.into_iter()
            .filter_map(|d| {
                let id = d.get_str(FIELD_SESSION_ID).ok()?.to_string();
                let created = d.get_i64(FIELD_CREATED_AT).unwrap_or(0);
                Some((id, created))
            })
            .collect()
    }

    /// The number of live sessions for `principal`. A backend failure is
    /// logged and yields `0`.
    async fn count(&self, principal: &str) -> usize {
        match self
            .collection
            .count_documents(doc! { FIELD_PRINCIPAL: principal })
            .await
        {
            Ok(n) => n as usize,
            Err(e) => {
                tracing::warn!(principal, error = %e, "session-mongodb: count failed");
                0
            }
        }
    }
}

/// The error type surfaced by [`MongoSessionRegistry`]'s constructors and
/// [`init`](MongoSessionRegistry::init). The [`SessionRegistry`] trait methods
/// themselves are infallible and never return this.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The connection string was malformed or the client could not connect.
    #[error("session-mongodb connection error: {0}")]
    Connection(String),
    /// A backend operation (e.g. index creation) failed.
    #[error("session-mongodb backend error: {0}")]
    Backend(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full round-trip against a live MongoDB — **env-gated**: set
    /// `FIREFLY_TEST_MONGODB_URL` (fallback `MONGODB_URL`) to run. Skips cleanly
    /// otherwise so the default `cargo test` stays infra-free.
    #[tokio::test]
    async fn registry_round_trips_against_live_mongo() {
        let Ok(url) =
            std::env::var("FIREFLY_TEST_MONGODB_URL").or_else(|_| std::env::var("MONGODB_URL"))
        else {
            eprintln!("skipping registry_round_trips_against_live_mongo: set FIREFLY_TEST_MONGODB_URL to run");
            return;
        };
        // A unique collection per run keeps concurrent test runs isolated.
        let coll = format!("firefly_sessions_test_{}", std::process::id());
        let registry = MongoSessionRegistry::connect_with(&url, "firefly_test", &coll)
            .await
            .expect("connect mongo");
        registry.init().await.expect("init indexes");

        // Clean slate.
        let _ = registry.collection().delete_many(doc! {}).await;

        registry.register("alice", "s1", 100).await;
        registry.register("alice", "s2", 200).await;
        registry.register("bob", "s3", 150).await;

        assert_eq!(registry.count("alice").await, 2);
        assert_eq!(registry.count("bob").await, 1);

        // Oldest-first ordering.
        let sessions = registry.list_sessions("alice").await;
        assert_eq!(sessions, vec![("s1".into(), 100), ("s2".into(), 200)]);

        // Re-register is an upsert, not a duplicate.
        registry.register("alice", "s1", 100).await;
        assert_eq!(registry.count("alice").await, 2);

        registry.deregister("alice", "s1").await;
        assert_eq!(registry.count("alice").await, 1);
        assert_eq!(
            registry.list_sessions("alice").await,
            vec![("s2".into(), 200)]
        );

        // Cleanup.
        let _ = registry.collection().drop().await;
    }
}
