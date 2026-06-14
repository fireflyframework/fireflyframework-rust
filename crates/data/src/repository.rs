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

//! The typed CRUD contract and its in-memory implementation.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::RwLock;

use async_trait::async_trait;
use thiserror::Error;

use crate::filter::Filter;
use crate::page::Page;
use crate::pageable::Pageable;

/// The error type shared by every [`Repository`] implementation.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DataError {
    /// The canonical "no row" error returned by every Repository
    /// implementation. Message matches the Go port's `ErrNotFound`.
    #[error("firefly/data: not found")]
    NotFound,
    /// A store-specific failure surfaced by a backing implementation
    /// (SQL driver error, connection loss, …).
    #[error("firefly/data: {0}")]
    Backend(String),
    /// An optimistic-locking conflict: the row was modified by another writer
    /// since it was loaded (the `@Version` guard found a stale version) —
    /// Spring Data's `OptimisticLockingFailureException`.
    #[error("firefly/data: optimistic lock conflict (stale version)")]
    OptimisticLock,
}

/// Repository is the generic typed CRUD contract. Implementations may
/// back onto a SQL driver, an in-memory map, or any other store —
/// service authors program against this trait.
///
/// The Go port's `context.Context` parameter is implicit in async Rust;
/// cancellation rides on the future itself.
#[async_trait]
pub trait Repository<T, K>: Send + Sync
where
    T: Send,
    K: Send + Sync,
{
    /// Looks up a single entity by primary key. Returns
    /// [`DataError::NotFound`] when no row matches.
    async fn find_by_id(&self, id: &K) -> Result<T, DataError>;

    /// Returns the page of entities selected by `filter`.
    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError>;

    /// Returns the page of entities for a Spring-style
    /// [`Pageable`](crate::Pageable) request.
    ///
    /// The default implementation lowers the pageable to a
    /// [`Filter`](crate::Filter) via [`Pageable::to_filter`] (translating
    /// the 1-based page number to the filter's 0-based page index and
    /// projecting the sort orders) and delegates to [`Repository::find`].
    /// Implementations may override it to push the pageable's sort/limit
    /// into the backing store directly.
    async fn find_page(&self, pageable: &Pageable) -> Result<Page<T>, DataError> {
        self.find(&pageable.to_filter()).await
    }

    /// Inserts or updates the entity (upsert by id) and returns the
    /// persisted value.
    async fn save(&self, entity: T) -> Result<T, DataError>;

    /// Removes the entity with the given id. Deleting a missing id is
    /// not an error.
    async fn delete(&self, id: &K) -> Result<(), DataError>;
}

/// MemoryRepository is the in-process [`Repository`] backed by a map.
/// ID extraction is delegated to a user-supplied keyer function.
///
/// `find` honours paging but not filter predicates — use a SQL-backed
/// Repository when you need real filtering. Unlike the Go original, the
/// store is guarded by an `RwLock`, so the repository is `Send + Sync`
/// and safe to share across tasks.
pub struct MemoryRepository<T, K> {
    keyer: Box<dyn Fn(&T) -> K + Send + Sync>,
    store: RwLock<HashMap<K, T>>,
}

impl<T, K> MemoryRepository<T, K>
where
    K: Eq + Hash,
{
    /// Returns an empty MemoryRepository whose ids are derived by
    /// `keyer`.
    pub fn new(keyer: impl Fn(&T) -> K + Send + Sync + 'static) -> Self {
        MemoryRepository {
            keyer: Box::new(keyer),
            store: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl<T, K> Repository<T, K> for MemoryRepository<T, K>
where
    T: Clone + Send + Sync,
    K: Eq + Hash + Clone + Send + Sync,
{
    async fn find_by_id(&self, id: &K) -> Result<T, DataError> {
        let store = self.store.read().expect("data: store lock poisoned");
        store.get(id).cloned().ok_or(DataError::NotFound)
    }

    async fn find(&self, filter: &Filter) -> Result<Page<T>, DataError> {
        let all: Vec<T> = {
            let store = self.store.read().expect("data: store lock poisoned");
            store.values().cloned().collect()
        };
        let total = all.len() as u64;
        let page = filter.page;
        let mut size = filter.size;
        if size == 0 {
            size = all.len().max(1);
        }
        let from = page * size;
        if from > all.len() {
            return Ok(Page::new(Vec::new(), page, size, total));
        }
        let to = (from + size).min(all.len());
        Ok(Page::new(all[from..to].to_vec(), page, size, total))
    }

    async fn save(&self, entity: T) -> Result<T, DataError> {
        let key = (self.keyer)(&entity);
        let mut store = self.store.write().expect("data: store lock poisoned");
        store.insert(key, entity.clone());
        Ok(entity)
    }

    async fn delete(&self, id: &K) -> Result<(), DataError> {
        let mut store = self.store.write().expect("data: store lock poisoned");
        store.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct User {
        id: String,
        name: String,
    }

    fn new_repo() -> MemoryRepository<User, String> {
        MemoryRepository::new(|u: &User| u.id.clone())
    }

    /// Port of Go `TestMemoryRepository`.
    #[tokio::test]
    async fn test_memory_repository() {
        let r = new_repo();
        assert_eq!(
            r.find_by_id(&"x".to_string()).await,
            Err(DataError::NotFound)
        );
        r.save(User {
            id: "u1".into(),
            name: "alice".into(),
        })
        .await
        .unwrap();
        let v = r.find_by_id(&"u1".to_string()).await.unwrap();
        assert_eq!(v.name, "alice", "got {v:?}");
        for c in 'a'..='e' {
            r.save(User {
                id: format!("u{c}"),
                name: "x".into(),
            })
            .await
            .unwrap();
        }
        let page = r.find(&Filter::new().paged(0, 3)).await.unwrap();
        assert_eq!(page.content.len(), 3, "page: {page:?}");
        assert_eq!(page.total_elements, 6, "page: {page:?}");
        r.delete(&"u1".to_string()).await.unwrap();
        assert_eq!(
            r.find_by_id(&"u1".to_string()).await,
            Err(DataError::NotFound)
        );
    }

    #[tokio::test]
    async fn test_save_is_upsert() {
        let r = new_repo();
        r.save(User {
            id: "u1".into(),
            name: "alice".into(),
        })
        .await
        .unwrap();
        r.save(User {
            id: "u1".into(),
            name: "bob".into(),
        })
        .await
        .unwrap();
        let v = r.find_by_id(&"u1".to_string()).await.unwrap();
        assert_eq!(v.name, "bob");
        let page = r.find(&Filter::new()).await.unwrap();
        assert_eq!(page.total_elements, 1);
    }

    #[tokio::test]
    async fn test_find_without_size_returns_everything() {
        let r = new_repo();
        for i in 0..4 {
            r.save(User {
                id: format!("u{i}"),
                name: "x".into(),
            })
            .await
            .unwrap();
        }
        let page = r.find(&Filter::new()).await.unwrap();
        assert_eq!(page.content.len(), 4);
        assert_eq!(page.total_elements, 4);
        assert_eq!(page.total_pages, 1);
    }

    #[tokio::test]
    async fn test_find_past_the_end_returns_empty_page() {
        let r = new_repo();
        r.save(User {
            id: "u1".into(),
            name: "alice".into(),
        })
        .await
        .unwrap();
        let page = r.find(&Filter::new().paged(5, 10)).await.unwrap();
        assert!(page.content.is_empty());
        assert_eq!(page.total_elements, 1);
        assert_eq!(page.number, 5);
    }

    #[tokio::test]
    async fn test_find_on_empty_store() {
        let r = new_repo();
        let page = r.find(&Filter::new()).await.unwrap();
        assert!(page.content.is_empty());
        assert_eq!(page.total_elements, 0);
        assert_eq!(page.size, 1, "size clamps to 1 like the Go port");
    }

    #[tokio::test]
    async fn test_delete_missing_id_is_not_an_error() {
        let r = new_repo();
        assert!(r.delete(&"ghost".to_string()).await.is_ok());
    }

    /// The error message must match the Go port's `ErrNotFound` exactly.
    #[test]
    fn test_not_found_message_matches_go() {
        assert_eq!(DataError::NotFound.to_string(), "firefly/data: not found");
    }

    /// Rust-specific: the trait is object-safe and the repository can be
    /// used through `dyn Repository`.
    #[tokio::test]
    async fn test_repository_is_object_safe() {
        let r: Box<dyn Repository<User, String>> = Box::new(new_repo());
        r.save(User {
            id: "u1".into(),
            name: "alice".into(),
        })
        .await
        .unwrap();
        let v = r.find_by_id(&"u1".to_string()).await.unwrap();
        assert_eq!(v.name, "alice");
    }

    /// `find_page` lowers a Spring-style `Pageable` (1-based page) to the
    /// filter's 0-based page index and returns the right window.
    #[tokio::test]
    async fn test_find_page_with_pageable() {
        use crate::pageable::Pageable;

        let r = new_repo();
        for c in 'a'..='f' {
            r.save(User {
                id: format!("u{c}"),
                name: "x".into(),
            })
            .await
            .unwrap();
        }
        // 1-based page 2, size 3 -> 0-based filter page 1 -> rows 3..6.
        let pageable = Pageable::paged(2, 3).unwrap();
        let page = r.find_page(&pageable).await.unwrap();
        assert_eq!(page.content.len(), 3, "page: {page:?}");
        assert_eq!(page.number, 1, "0-based page index");
        assert_eq!(page.total_elements, 6);
    }

    /// An unpaged `Pageable` fetches everything via `find_page`.
    #[tokio::test]
    async fn test_find_page_unpaged_returns_everything() {
        use crate::pageable::Pageable;

        let r = new_repo();
        for i in 0..4 {
            r.save(User {
                id: format!("u{i}"),
                name: "x".into(),
            })
            .await
            .unwrap();
        }
        let page = r.find_page(&Pageable::unpaged()).await.unwrap();
        assert_eq!(page.content.len(), 4);
        assert_eq!(page.total_elements, 4);
    }

    /// Rust-specific: the in-memory repository is Send + Sync so it can
    /// be shared across tokio tasks.
    #[test]
    fn test_memory_repository_is_send_sync() {
        fn assert_send_sync<X: Send + Sync>() {}
        assert_send_sync::<MemoryRepository<User, String>>();
        assert_send_sync::<DataError>();
    }
}
