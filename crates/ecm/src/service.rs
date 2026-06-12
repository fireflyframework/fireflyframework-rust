//! In-memory [`DocumentService`] composing a [`ContentStore`].

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

use crate::ports::{
    sha256_hex, zero_time, ContentReader, ContentStore, Document, DocumentService, EcmError,
};

/// Service is the default [`DocumentService`] composing a [`ContentStore`]
/// with an in-memory document index. Production services typically wire
/// in a database-backed index instead.
pub struct Service {
    content: Box<dyn ContentStore>,
    docs: RwLock<HashMap<String, Document>>,
}

impl Service {
    /// Returns a Service backed by `content`.
    pub fn new(content: impl ContentStore + 'static) -> Self {
        Self {
            content: Box::new(content),
            docs: RwLock::new(HashMap::new()),
        }
    }

    /// Computes the lowercase hexadecimal SHA-256 checksum of the stored
    /// content of document `id`, for integrity verification.
    pub async fn checksum(&self, id: &str) -> Result<String, EcmError> {
        let mut reader = self.read(id).await?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await?;
        Ok(sha256_hex(&buf))
    }

    /// Verifies the content integrity of document `id` by comparing its
    /// SHA-256 checksum against `expected` (case-insensitive), returning
    /// `true` on match.
    pub async fn verify_checksum(&self, id: &str, expected: &str) -> Result<bool, EcmError> {
        Ok(self.checksum(id).await?.eq_ignore_ascii_case(expected))
    }
}

#[async_trait]
impl DocumentService for Service {
    async fn create(
        &self,
        mut doc: Document,
        content: ContentReader,
    ) -> Result<Document, EcmError> {
        if doc.id.is_empty() {
            doc.id = new_id();
        }
        let now = Utc::now();
        if doc.created_at == zero_time() {
            doc.created_at = now;
        }
        doc.updated_at = now;
        doc.version = 1;
        doc.size = self.content.put(&doc.id, content).await?;
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

    async fn read(&self, id: &str) -> Result<ContentReader, EcmError> {
        self.get(id).await?;
        self.content.get(id).await
    }

    async fn delete(&self, id: &str) -> Result<(), EcmError> {
        self.get(id).await?;
        self.content.delete(id).await?;
        self.docs.write().await.remove(id);
        Ok(())
    }
}

/// Generates a 32-character lowercase hexadecimal identifier (122 bits of
/// randomness), matching the shape of the Go port's `newID`.
fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalStore;
    use crate::ports::bytes_reader;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    fn new_service(dir: &tempfile::TempDir) -> Service {
        Service::new(LocalStore::new(dir.path()))
    }

    async fn read_all(mut r: ContentReader) -> Vec<u8> {
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        buf
    }

    // ---------------------------------------------------------------------
    // Port of Go TestServiceCRUD: create + get + read + delete lifecycle,
    // file-backed Read after Create, and 404-after-delete.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn service_crud() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);

        let doc = svc
            .create(
                Document {
                    name: "spec.txt".into(),
                    mime_type: "text/plain".into(),
                    ..Default::default()
                },
                bytes_reader(b"hello firefly".to_vec()),
            )
            .await
            .unwrap();
        assert!(!doc.id.is_empty(), "doc: {doc:?}");
        assert_eq!(doc.size, "hello firefly".len() as i64, "doc: {doc:?}");

        let got = svc.get(&doc.id).await.unwrap();
        assert_eq!(got.name, "spec.txt");

        let body = read_all(svc.read(&doc.id).await.unwrap()).await;
        assert_eq!(body, b"hello firefly");

        svc.delete(&doc.id).await.unwrap();
        assert!(
            svc.get(&doc.id).await.unwrap_err().is_not_found(),
            "expected not found after delete"
        );
        // File should also be gone.
        assert!(
            !dir.path().join(&doc.id).exists(),
            "file remained after delete"
        );
    }

    // ---------------------------------------------------------------------
    // Field-assignment semantics of Create, mirroring the Go service.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_assigns_32_hex_char_id() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"x".to_vec()))
            .await
            .unwrap();
        assert_eq!(doc.id.len(), 32, "id: {}", doc.id);
        assert!(doc
            .id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[tokio::test]
    async fn create_preserves_caller_supplied_id_and_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let created_at = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        let doc = svc
            .create(
                Document {
                    id: "doc-42".into(),
                    name: "spec.txt".into(),
                    created_at,
                    ..Default::default()
                },
                bytes_reader(b"x".to_vec()),
            )
            .await
            .unwrap();
        assert_eq!(doc.id, "doc-42");
        assert_eq!(doc.created_at, created_at);
        assert!(doc.updated_at > created_at);
        assert_eq!(svc.get("doc-42").await.unwrap().name, "spec.txt");
    }

    #[tokio::test]
    async fn create_sets_timestamps_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let before = Utc::now();
        let doc = svc
            .create(Document::default(), bytes_reader(b"x".to_vec()))
            .await
            .unwrap();
        // Zero CreatedAt is replaced; both stamps come from the same instant.
        assert_eq!(doc.created_at, doc.updated_at);
        assert!(doc.created_at >= before && doc.created_at <= Utc::now());
        assert_eq!(doc.version, 1);
        assert_eq!(doc.size, 1);
    }

    #[tokio::test]
    async fn get_read_delete_missing_are_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        assert!(svc.get("missing").await.unwrap_err().is_not_found());
        assert!(svc.read("missing").await.err().unwrap().is_not_found());
        assert!(svc.delete("missing").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn create_writes_content_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"on disk".to_vec()))
            .await
            .unwrap();
        let on_disk = tokio::fs::read(dir.path().join(&doc.id)).await.unwrap();
        assert_eq!(on_disk, b"on disk");
    }

    // ---------------------------------------------------------------------
    // Checksum support.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn checksum_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"hello firefly".to_vec()))
            .await
            .unwrap();

        let sum = svc.checksum(&doc.id).await.unwrap();
        assert_eq!(
            sum,
            "d4977b6f6f5bf0a0efcf2e979bd11e936ee0bc60f6c58613b7d47e24dc5b0ab2"
        );
        assert_eq!(sum, sha256_hex(b"hello firefly"));

        assert!(svc.verify_checksum(&doc.id, &sum).await.unwrap());
        // Case-insensitive comparison.
        assert!(svc
            .verify_checksum(&doc.id, &sum.to_ascii_uppercase())
            .await
            .unwrap());
        assert!(!svc
            .verify_checksum(&doc.id, &sha256_hex(b"tampered"))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn checksum_missing_document_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        assert!(svc.checksum("missing").await.unwrap_err().is_not_found());
    }

    // ---------------------------------------------------------------------
    // Rust-specific: object safety and shared concurrent use.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn service_usable_as_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        let svc: Arc<dyn DocumentService> = Arc::new(new_service(&dir));
        let doc = svc
            .create(
                Document {
                    name: "spec.txt".into(),
                    ..Default::default()
                },
                bytes_reader(b"x".to_vec()),
            )
            .await
            .unwrap();
        assert_eq!(svc.get(&doc.id).await.unwrap().name, "spec.txt");
        svc.delete(&doc.id).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_creates_yield_distinct_documents() {
        let dir = tempfile::tempdir().unwrap();
        let svc = Arc::new(new_service(&dir));

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let svc = Arc::clone(&svc);
                tokio::spawn(async move {
                    svc.create(
                        Document {
                            name: format!("doc-{i}.txt"),
                            ..Default::default()
                        },
                        bytes_reader(format!("body-{i}").into_bytes()),
                    )
                    .await
                    .unwrap()
                })
            })
            .collect();

        let mut ids = std::collections::HashSet::new();
        for handle in handles {
            let doc = handle.await.unwrap();
            assert!(ids.insert(doc.id.clone()), "duplicate id {}", doc.id);
            let body = read_all(svc.read(&doc.id).await.unwrap()).await;
            assert_eq!(
                body,
                format!(
                    "body-{}",
                    doc.name.trim_start_matches("doc-").trim_end_matches(".txt")
                )
                .into_bytes()
            );
        }
        assert_eq!(ids.len(), 8);
    }
}
