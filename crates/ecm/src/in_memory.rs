//! In-memory [`MetadataStore`] and [`FolderRepository`] adapters — the Rust
//! analogs of pyfly's `InMemoryMetadataStorage` and `InMemoryFolderRepository`.
//!
//! Both keep a `HashMap` behind a [`tokio::sync::RwLock`] (the analog of
//! pyfly's `asyncio.Lock`), so they are safe to share across tasks and are
//! ideal for tests and single-instance deployments.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::ports::{Document, EcmError, Folder, FolderRepository, MetadataStore};

/// InMemoryMetadataStore is the default [`MetadataStore`] — a thread-safe
/// in-memory document index keyed by document id. Mirrors pyfly's
/// `InMemoryMetadataStorage`.
#[derive(Default)]
pub struct InMemoryMetadataStore {
    docs: RwLock<HashMap<String, Document>>,
}

impl InMemoryMetadataStore {
    /// Returns an empty in-memory metadata store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MetadataStore for InMemoryMetadataStore {
    async fn save(&self, doc: Document) -> Result<Document, EcmError> {
        self.docs.write().await.insert(doc.id.clone(), doc.clone());
        Ok(doc)
    }

    async fn get(&self, id: &str) -> Result<Document, EcmError> {
        self.docs
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or(EcmError::NotFound)
    }

    async fn list(&self, folder_id: Option<&str>, limit: usize) -> Result<Vec<Document>, EcmError> {
        let guard = self.docs.read().await;
        let results = guard
            .values()
            .filter(|d| match folder_id {
                Some(fid) => d.folder_id == fid,
                None => true,
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(results)
    }

    async fn delete(&self, id: &str) -> Result<bool, EcmError> {
        Ok(self.docs.write().await.remove(id).is_some())
    }
}

/// InMemoryFolderRepository is the default [`FolderRepository`] — a
/// thread-safe in-memory folder index keyed by folder id. Mirrors pyfly's
/// `InMemoryFolderRepository`.
#[derive(Default)]
pub struct InMemoryFolderRepository {
    folders: RwLock<HashMap<String, Folder>>,
}

impl InMemoryFolderRepository {
    /// Returns an empty in-memory folder repository.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl FolderRepository for InMemoryFolderRepository {
    async fn save(&self, folder: Folder) -> Result<Folder, EcmError> {
        self.folders
            .write()
            .await
            .insert(folder.id.clone(), folder.clone());
        Ok(folder)
    }

    async fn get(&self, id: &str) -> Result<Folder, EcmError> {
        self.folders
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or(EcmError::NotFound)
    }

    async fn list(&self, parent_id: Option<&str>) -> Result<Vec<Folder>, EcmError> {
        // pyfly compares `f.parent_id == parent_id`; the Rust `Folder` models
        // the root parent as an empty string, so `None` matches the empty
        // parent and `Some(p)` matches exactly `p`.
        let want = parent_id.unwrap_or("");
        let guard = self.folders.read().await;
        Ok(guard
            .values()
            .filter(|f| f.parent_id == want)
            .cloned()
            .collect())
    }

    async fn delete(&self, id: &str) -> Result<bool, EcmError> {
        Ok(self.folders.write().await.remove(id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn doc(id: &str, folder: &str) -> Document {
        Document {
            id: id.into(),
            folder_id: folder.into(),
            name: format!("{id}.txt"),
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // MetadataStore — pyfly InMemoryMetadataStorage parity.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn metadata_save_get_round_trip() {
        let store = InMemoryMetadataStore::new();
        let saved = store.save(doc("d1", "")).await.unwrap();
        assert_eq!(saved.id, "d1");
        assert_eq!(store.get("d1").await.unwrap().name, "d1.txt");
        assert!(store.get("missing").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn metadata_list_filters_by_folder() {
        let store = InMemoryMetadataStore::new();
        store.save(doc("a", "f-1")).await.unwrap();
        store.save(doc("b", "f-2")).await.unwrap();
        // pyfly: test_list_filters_by_folder — exactly one doc lives in f-1.
        let in_f1 = store.list(Some("f-1"), 100).await.unwrap();
        assert_eq!(in_f1.len(), 1);
        assert_eq!(in_f1[0].id, "a");
        // None returns every folder.
        assert_eq!(store.list(None, 100).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn metadata_list_honors_limit() {
        let store = InMemoryMetadataStore::new();
        for i in 0..5 {
            store.save(doc(&format!("d{i}"), "")).await.unwrap();
        }
        assert_eq!(store.list(None, 2).await.unwrap().len(), 2);
        assert_eq!(store.list(None, 100).await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn metadata_delete_returns_whether_present() {
        let store = InMemoryMetadataStore::new();
        store.save(doc("d1", "")).await.unwrap();
        assert!(store.delete("d1").await.unwrap());
        assert!(!store.delete("d1").await.unwrap());
        assert!(store.get("d1").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn metadata_save_replaces_existing() {
        let store = InMemoryMetadataStore::new();
        store.save(doc("d1", "f-1")).await.unwrap();
        let mut updated = doc("d1", "f-2");
        updated.name = "renamed.txt".into();
        store.save(updated).await.unwrap();
        let got = store.get("d1").await.unwrap();
        assert_eq!(got.folder_id, "f-2");
        assert_eq!(got.name, "renamed.txt");
    }

    // -----------------------------------------------------------------------
    // FolderRepository — pyfly InMemoryFolderRepository parity.
    // -----------------------------------------------------------------------

    fn folder(id: &str, parent: &str) -> Folder {
        Folder {
            id: id.into(),
            name: format!("{id}-folder"),
            parent_id: parent.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn folder_save_get_round_trip() {
        let repo = InMemoryFolderRepository::new();
        repo.save(folder("f1", "")).await.unwrap();
        assert_eq!(repo.get("f1").await.unwrap().name, "f1-folder");
        assert!(repo.get("missing").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn folder_list_filters_by_parent() {
        let repo = InMemoryFolderRepository::new();
        repo.save(folder("root1", "")).await.unwrap();
        repo.save(folder("child", "root1")).await.unwrap();
        // None lists root folders (empty parent).
        let roots = repo.list(None).await.unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "root1");
        // Some(parent) lists that parent's children.
        let children = repo.list(Some("root1")).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, "child");
    }

    #[tokio::test]
    async fn folder_delete_returns_whether_present() {
        let repo = InMemoryFolderRepository::new();
        repo.save(folder("f1", "")).await.unwrap();
        assert!(repo.delete("f1").await.unwrap());
        assert!(!repo.delete("f1").await.unwrap());
    }

    #[tokio::test]
    async fn repos_usable_as_trait_objects() {
        let meta: Arc<dyn MetadataStore> = Arc::new(InMemoryMetadataStore::new());
        let folders: Arc<dyn FolderRepository> = Arc::new(InMemoryFolderRepository::new());
        meta.save(doc("d1", "")).await.unwrap();
        folders.save(folder("f1", "")).await.unwrap();
        assert_eq!(meta.get("d1").await.unwrap().id, "d1");
        assert_eq!(folders.get("f1").await.unwrap().id, "f1");
    }
}
