//! In-memory [`DocumentService`] composing a [`ContentStore`].

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

use crate::ports::{
    sha256_hex, version_key, zero_time, ContentReader, ContentStore, Document, DocumentService,
    DocumentVersion, EcmError, Folder, FolderRepository,
};

/// Service is the default [`DocumentService`] composing a [`ContentStore`]
/// with an in-memory document index. Production services typically wire
/// in a database-backed index instead.
///
/// Beyond the [`DocumentService`] trait, `Service` adds pyfly-parity
/// conveniences as inherent methods: [`list`](Service::list) (filter the
/// index by folder, capped at a limit), multi-version blob support
/// ([`add_version`](Service::add_version) / [`versions`](Service::versions) /
/// [`read_version`](Service::read_version) /
/// [`delete_version`](Service::delete_version)), and folder management
/// ([`create_folder`](Service::create_folder)) when a [`FolderRepository`] is
/// wired in via [`with_folders`](Service::with_folders).
pub struct Service {
    content: Box<dyn ContentStore>,
    docs: RwLock<HashMap<String, Document>>,
    versions: RwLock<HashMap<String, Vec<DocumentVersion>>>,
    folders: Option<Box<dyn FolderRepository>>,
}

impl Service {
    /// Returns a Service backed by `content`.
    pub fn new(content: impl ContentStore + 'static) -> Self {
        Self {
            content: Box::new(content),
            docs: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            folders: None,
        }
    }

    /// Returns a Service backed by `content` with a [`FolderRepository`]
    /// wired in, enabling [`create_folder`](Service::create_folder). The
    /// analog of pyfly's `DocumentService(storage, metadata, folders=...)`.
    pub fn with_folders(
        content: impl ContentStore + 'static,
        folders: impl FolderRepository + 'static,
    ) -> Self {
        Self {
            content: Box::new(content),
            docs: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            folders: Some(Box::new(folders)),
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

    /// Lists indexed documents, optionally filtered to a `folder_id` (`None`
    /// returns every folder), capped at `limit` records. The pyfly-parity
    /// analog of `DocumentService.list(folder_id=...)` /
    /// `MetadataStoragePort.list(folder_id, *, limit)`.
    pub async fn list(
        &self,
        folder_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Document>, EcmError> {
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

    /// Persists `folder` through the wired [`FolderRepository`], returning the
    /// stored record. The analog of pyfly's `DocumentService.create_folder`;
    /// returns [`EcmError::Provider`] (`"firefly/ecm: no FolderRepository configured"`)
    /// when the Service was built with [`new`](Service::new) instead of
    /// [`with_folders`](Service::with_folders).
    pub async fn create_folder(&self, folder: Folder) -> Result<Folder, EcmError> {
        match &self.folders {
            Some(repo) => repo.save(folder).await,
            None => Err(EcmError::provider(
                "firefly/ecm: no FolderRepository configured",
            )),
        }
    }

    /// Appends a new revision of document `id`'s content and returns the
    /// resulting [`DocumentVersion`]. The content is stored under the
    /// version-aware key `<id>/v<n>` (see [`version_key`]), the document's
    /// `version` counter and `size` are bumped, and the version is recorded
    /// in the per-document version list. Mirrors pyfly's append-on-upload
    /// `DocumentVersion` semantics. [`EcmError::NotFound`] when `id` is
    /// unknown.
    pub async fn add_version(
        &self,
        id: &str,
        content: ContentReader,
    ) -> Result<DocumentVersion, EcmError> {
        let mut doc = self.get(id).await?;
        let next = doc.version + 1;
        let key = version_key(id, next);

        // Buffer the content so we can both store it and hash it.
        let mut buf = Vec::new();
        let mut content = content;
        content.read_to_end(&mut buf).await?;
        let size = buf.len() as i64;
        let content_hash = sha256_hex(&buf);
        self.content
            .put(&key, Box::pin(std::io::Cursor::new(buf)))
            .await?;

        let version = DocumentVersion {
            version: next,
            content_hash,
            size_bytes: size,
            storage_uri: key,
            created_at: Utc::now(),
        };

        doc.version = next;
        doc.size = size;
        doc.updated_at = version.created_at;
        self.docs.write().await.insert(doc.id.clone(), doc);
        self.versions
            .write()
            .await
            .entry(id.to_string())
            .or_default()
            .push(version.clone());
        Ok(version)
    }

    /// Returns the recorded [`DocumentVersion`] list for document `id`
    /// (oldest first), or [`EcmError::NotFound`] when `id` is unknown. A
    /// document created with [`create`](DocumentService::create) but never
    /// extended via [`add_version`](Service::add_version) has an empty list.
    pub async fn versions(&self, id: &str) -> Result<Vec<DocumentVersion>, EcmError> {
        self.get(id).await?;
        Ok(self
            .versions
            .read()
            .await
            .get(id)
            .cloned()
            .unwrap_or_default())
    }

    /// Opens the binary content of a specific `version` of document `id`,
    /// stored under the version-aware key `<id>/v<version>`.
    /// [`EcmError::NotFound`] when the document or that version is absent.
    pub async fn read_version(&self, id: &str, version: i64) -> Result<ContentReader, EcmError> {
        self.require_version(id, version).await?;
        self.content.get(&version_key(id, version)).await
    }

    /// Removes the stored content of a specific `version` of document `id`
    /// and drops it from the recorded version list. [`EcmError::NotFound`]
    /// when the document or that version is absent.
    pub async fn delete_version(&self, id: &str, version: i64) -> Result<(), EcmError> {
        self.require_version(id, version).await?;
        self.content.delete(&version_key(id, version)).await?;
        if let Some(list) = self.versions.write().await.get_mut(id) {
            list.retain(|v| v.version != version);
        }
        Ok(())
    }

    /// Ensures document `id` exists and has recorded `version`.
    async fn require_version(&self, id: &str, version: i64) -> Result<(), EcmError> {
        self.get(id).await?;
        let guard = self.versions.read().await;
        let present = guard
            .get(id)
            .map(|list| list.iter().any(|v| v.version == version))
            .unwrap_or(false);
        if present {
            Ok(())
        } else {
            Err(EcmError::NotFound)
        }
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
    // pyfly parity: list(folder_id, limit).
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn list_filters_by_folder() {
        // Port of pyfly test_list_filters_by_folder.
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        svc.create(
            Document {
                name: "a.txt".into(),
                folder_id: "f-1".into(),
                ..Default::default()
            },
            bytes_reader(b"a".to_vec()),
        )
        .await
        .unwrap();
        svc.create(
            Document {
                name: "b.txt".into(),
                folder_id: "f-2".into(),
                ..Default::default()
            },
            bytes_reader(b"b".to_vec()),
        )
        .await
        .unwrap();

        let in_f1 = svc.list(Some("f-1"), 100).await.unwrap();
        assert_eq!(in_f1.len(), 1);
        assert_eq!(in_f1[0].name, "a.txt");
        // None returns every folder.
        assert_eq!(svc.list(None, 100).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_honors_limit() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        for i in 0..4 {
            svc.create(
                Document {
                    name: format!("d{i}"),
                    ..Default::default()
                },
                bytes_reader(b"x".to_vec()),
            )
            .await
            .unwrap();
        }
        assert_eq!(svc.list(None, 2).await.unwrap().len(), 2);
        assert_eq!(svc.list(None, 100).await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn list_empty_index_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        assert!(svc.list(None, 100).await.unwrap().is_empty());
        assert!(svc.list(Some("f-1"), 100).await.unwrap().is_empty());
    }

    // ---------------------------------------------------------------------
    // pyfly parity: create_folder via a wired FolderRepository.
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_folder_requires_repository() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        // pyfly raises RuntimeError when no FolderRepositoryPort is configured.
        let err = svc.create_folder(Folder::default()).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "firefly/ecm: no FolderRepository configured"
        );
    }

    #[tokio::test]
    async fn create_folder_persists_through_repository() {
        use crate::in_memory::InMemoryFolderRepository;
        let dir = tempfile::tempdir().unwrap();
        let svc =
            Service::with_folders(LocalStore::new(dir.path()), InMemoryFolderRepository::new());
        let folder = svc
            .create_folder(Folder {
                id: "f1".into(),
                name: "contracts".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(folder.id, "f1");
        assert_eq!(folder.name, "contracts");
    }

    // ---------------------------------------------------------------------
    // pyfly parity: multi-version blobs.
    // ---------------------------------------------------------------------

    use crate::ports::version_key;
    use crate::ports::DocumentVersion;

    #[tokio::test]
    async fn add_version_appends_and_stores_under_versioned_key() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"v1-body".to_vec()))
            .await
            .unwrap();
        assert_eq!(doc.version, 1);
        assert!(svc.versions(&doc.id).await.unwrap().is_empty());

        let v2 = svc
            .add_version(&doc.id, bytes_reader(b"v2-body-longer".to_vec()))
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.size_bytes, "v2-body-longer".len() as i64);
        assert_eq!(v2.storage_uri, version_key(&doc.id, 2));
        assert_eq!(v2.content_hash, sha256_hex(b"v2-body-longer"));

        // Document counter and size were bumped.
        let updated = svc.get(&doc.id).await.unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.size, "v2-body-longer".len() as i64);

        // The versioned blob is on disk and readable.
        let body = read_all(svc.read_version(&doc.id, 2).await.unwrap()).await;
        assert_eq!(body, b"v2-body-longer");
        let on_disk = dir.path().join(version_key(&doc.id, 2));
        assert!(on_disk.exists());

        // The version list records the single appended revision.
        let versions = svc.versions(&doc.id).await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0], v2);
    }

    #[tokio::test]
    async fn add_version_increments_monotonically() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"1".to_vec()))
            .await
            .unwrap();
        let a = svc
            .add_version(&doc.id, bytes_reader(b"2".to_vec()))
            .await
            .unwrap();
        let b = svc
            .add_version(&doc.id, bytes_reader(b"3".to_vec()))
            .await
            .unwrap();
        assert_eq!(a.version, 2);
        assert_eq!(b.version, 3);
        let versions: Vec<i64> = svc
            .versions(&doc.id)
            .await
            .unwrap()
            .into_iter()
            .map(|v: DocumentVersion| v.version)
            .collect();
        assert_eq!(versions, vec![2, 3]);
    }

    #[tokio::test]
    async fn delete_version_removes_blob_and_record() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        let doc = svc
            .create(Document::default(), bytes_reader(b"1".to_vec()))
            .await
            .unwrap();
        svc.add_version(&doc.id, bytes_reader(b"2".to_vec()))
            .await
            .unwrap();
        svc.add_version(&doc.id, bytes_reader(b"3".to_vec()))
            .await
            .unwrap();

        svc.delete_version(&doc.id, 2).await.unwrap();
        assert!(!dir.path().join(version_key(&doc.id, 2)).exists());
        let remaining: Vec<i64> = svc
            .versions(&doc.id)
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(remaining, vec![3]);
        // Reading the deleted version is now NotFound.
        assert!(svc
            .read_version(&doc.id, 2)
            .await
            .err()
            .unwrap()
            .is_not_found());
    }

    #[tokio::test]
    async fn version_ops_on_missing_document_or_version_are_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let svc = new_service(&dir);
        assert!(svc
            .add_version("missing", bytes_reader(b"x".to_vec()))
            .await
            .unwrap_err()
            .is_not_found());
        assert!(svc.versions("missing").await.unwrap_err().is_not_found());
        assert!(svc
            .read_version("missing", 1)
            .await
            .err()
            .unwrap()
            .is_not_found());
        assert!(svc
            .delete_version("missing", 1)
            .await
            .unwrap_err()
            .is_not_found());

        let doc = svc
            .create(Document::default(), bytes_reader(b"1".to_vec()))
            .await
            .unwrap();
        // No version 2 has been appended yet.
        assert!(svc
            .read_version(&doc.id, 2)
            .await
            .err()
            .unwrap()
            .is_not_found());
        assert!(svc
            .delete_version(&doc.id, 2)
            .await
            .unwrap_err()
            .is_not_found());
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
