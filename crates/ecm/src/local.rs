//! Filesystem-backed [`ContentStore`] — the default content adapter.

use std::io::ErrorKind;
use std::path::PathBuf;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::ports::{ContentReader, ContentStore, EcmError};

/// LocalStore is the default [`ContentStore`] backed by a directory on
/// the local filesystem. Suitable for development and single-instance
/// deployments.
pub struct LocalStore {
    root: PathBuf,
    mu: Mutex<()>,
}

impl LocalStore {
    /// Returns a LocalStore rooted at `root`. The directory is created on
    /// first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mu: Mutex::new(()),
        }
    }
}

#[async_trait]
impl ContentStore for LocalStore {
    async fn put(&self, key: &str, mut content: ContentReader) -> Result<i64, EcmError> {
        let _guard = self.mu.lock().await;
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut file = tokio::fs::File::create(&path).await?;
        let size = tokio::io::copy(&mut content, &mut file).await?;
        file.flush().await?;
        Ok(size as i64)
    }

    async fn get(&self, key: &str) -> Result<ContentReader, EcmError> {
        match tokio::fs::File::open(self.root.join(key)).await {
            Ok(file) => Ok(Box::pin(file) as ContentReader),
            Err(err) if err.kind() == ErrorKind::NotFound => Err(EcmError::NotFound),
            Err(err) => Err(err.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), EcmError> {
        match tokio::fs::remove_file(self.root.join(key)).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn name(&self) -> &str {
        "local-fs"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::bytes_reader;
    use tokio::io::AsyncReadExt;

    async fn read_all(mut r: ContentReader) -> Vec<u8> {
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn put_get_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        assert_eq!(store.name(), "local-fs");

        let size = store
            .put("k1", bytes_reader(b"hello firefly".to_vec()))
            .await
            .unwrap();
        assert_eq!(size, 13);

        let body = read_all(store.get("k1").await.unwrap()).await;
        assert_eq!(body, b"hello firefly");

        store.delete("k1").await.unwrap();
        assert!(store.get("k1").await.err().unwrap().is_not_found());
        assert!(!dir.path().join("k1").exists());
    }

    #[tokio::test]
    async fn get_missing_key_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let err = store.get("missing").await.err().unwrap();
        assert!(err.is_not_found());
        assert_eq!(err.to_string(), "firefly/ecm: not found");
    }

    #[tokio::test]
    async fn delete_missing_key_is_idempotent() {
        // Mirrors the Go port: os.Remove's IsNotExist is swallowed.
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        store.delete("missing").await.unwrap();
    }

    #[tokio::test]
    async fn put_creates_root_and_nested_directories() {
        // The Go port MkdirAlls filepath.Dir(root/key): the root directory
        // itself and any nested key segments materialize on first write.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("docs").join("store");
        let store = LocalStore::new(root.clone());
        assert!(!root.exists());

        let size = store
            .put("a/b/c.txt", bytes_reader(b"nested".to_vec()))
            .await
            .unwrap();
        assert_eq!(size, 6);
        assert_eq!(
            read_all(store.get("a/b/c.txt").await.unwrap()).await,
            b"nested"
        );
        assert!(root.join("a").join("b").join("c.txt").exists());
    }

    #[tokio::test]
    async fn put_truncates_existing_content() {
        // os.Create / File::create truncate: re-putting a key replaces its
        // content entirely, even when the new content is shorter.
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        store
            .put("k", bytes_reader(b"a longer first version".to_vec()))
            .await
            .unwrap();
        let size = store.put("k", bytes_reader(b"v2".to_vec())).await.unwrap();
        assert_eq!(size, 2);
        assert_eq!(read_all(store.get("k").await.unwrap()).await, b"v2");
    }

    #[tokio::test]
    async fn put_empty_content_writes_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let size = store.put("empty", bytes_reader(Vec::new())).await.unwrap();
        assert_eq!(size, 0);
        assert_eq!(read_all(store.get("empty").await.unwrap()).await, b"");
    }
}
