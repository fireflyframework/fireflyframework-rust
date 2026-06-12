//! Ports (trait contracts) and domain models of the ECM abstraction.

use std::pin::Pin;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use tokio::io::AsyncRead;

/// Errors produced by the ECM ports and their default implementations.
#[derive(Debug, thiserror::Error)]
pub enum EcmError {
    /// The canonical missing-entity error. Display output matches the Go
    /// port's `ErrNotFound` (`firefly/ecm: not found`).
    #[error("firefly/ecm: not found")]
    NotFound,
    /// An underlying I/O failure from the content store.
    #[error("firefly/ecm: io: {0}")]
    Io(#[from] std::io::Error),
    /// Any other adapter-specific failure (cloud-storage outage, e-signature
    /// provider rejection, unimplemented stub, …); the message is rendered
    /// verbatim, so vendor adapters can keep their sentinel messages
    /// bytes-equal to the Go port.
    #[error("{0}")]
    Provider(String),
}

impl EcmError {
    /// Builds an adapter-specific [`EcmError::Provider`] from any message.
    pub fn provider(message: impl Into<String>) -> Self {
        EcmError::Provider(message.into())
    }

    /// Returns `true` when the error is the canonical [`EcmError::NotFound`]
    /// sentinel — the analog of Go's `errors.Is(err, ecm.ErrNotFound)`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, EcmError::NotFound)
    }
}

/// Boxed async byte stream — the Rust analog of Go's `io.ReadCloser`
/// returned by [`ContentStore::get`] and accepted by [`ContentStore::put`].
pub type ContentReader = Pin<Box<dyn AsyncRead + Send>>;

/// Wraps an in-memory byte buffer as a [`ContentReader`], the analog of
/// Go's `bytes.NewReader`.
pub fn bytes_reader(bytes: impl Into<Vec<u8>>) -> ContentReader {
    Box::pin(std::io::Cursor::new(bytes.into()))
}

/// Computes the lowercase hexadecimal SHA-256 digest of `data`. Used for
/// content-integrity verification (see [`crate::Service::checksum`]).
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// The Go/Java zero-time sentinel (`0001-01-01T00:00:00Z`) used as the
/// default for unset timestamps so the JSON wire format matches the Go port.
pub(crate) fn zero_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap()
}

/// Maps a JSON `null` to the type's default value. The Go port serializes a
/// nil `Signers` slice (no `omitempty`) as `"signers":null`; this keeps such
/// payloads deserializable on the Rust side.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Document is the document-record metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Document {
    /// Unique document identifier; generated on create when empty.
    pub id: String,
    /// Containing folder identifier; omitted from JSON when empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub folder_id: String,
    /// Human-readable document name (e.g. `spec.pdf`).
    pub name: String,
    /// MIME type of the binary content (e.g. `application/pdf`).
    pub mime_type: String,
    /// Content size in bytes, set from the stored content on create.
    pub size: i64,
    /// Free-form labels; omitted from JSON when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Arbitrary key/value metadata; omitted from JSON when empty.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Creation timestamp (UTC); filled on create when left at the zero time.
    pub created_at: DateTime<Utc>,
    /// Last-update timestamp (UTC); always refreshed on create.
    pub updated_at: DateTime<Utc>,
    /// Monotonic version counter, starting at 1.
    pub version: i64,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            id: String::new(),
            folder_id: String::new(),
            name: String::new(),
            mime_type: String::new(),
            size: 0,
            tags: Vec::new(),
            metadata: serde_json::Map::new(),
            created_at: zero_time(),
            updated_at: zero_time(),
            version: 0,
        }
    }
}

/// DocumentVersion is a single immutable revision of a [`Document`]'s binary
/// content. Each upload appends a new version; the version number is a
/// monotonic 1-based counter. Faithful port of pyfly's `ecm.DocumentVersion`
/// dataclass (`version`, `content_hash`, `size_bytes`, `storage_uri`,
/// `created_at`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DocumentVersion {
    /// Monotonic 1-based revision number.
    pub version: i64,
    /// Lowercase hexadecimal SHA-256 digest of the stored content.
    pub content_hash: String,
    /// Content size in bytes.
    pub size_bytes: i64,
    /// Backing-store key (or URI) under which the content lives — the
    /// version-aware key produced by [`version_key`] and used by
    /// [`crate::Service::add_version`].
    pub storage_uri: String,
    /// Creation timestamp (UTC); filled on append when left at the zero time.
    pub created_at: DateTime<Utc>,
}

impl Default for DocumentVersion {
    fn default() -> Self {
        Self {
            version: 0,
            content_hash: String::new(),
            size_bytes: 0,
            storage_uri: String::new(),
            created_at: zero_time(),
        }
    }
}

/// Builds the version-aware content key for a multi-version blob, used by
/// [`crate::Service::add_version`] and friends.
///
/// The scheme is `<doc-id>__v<n>` — a flat key that mirrors pyfly's
/// per-version `v<n>` convention while deliberately *not* nesting under the
/// bare `<doc-id>` key. The Go-parity [`crate::Service::create`] stores its
/// primary blob at the bare `<doc-id>` key (a file on [`crate::LocalStore`]),
/// so a nested `<doc-id>/v<n>` key would clash file-vs-directory; the flat
/// key lets the two coexist on every [`ContentStore`].
pub fn version_key(document_id: &str, version: i64) -> String {
    format!("{document_id}__v{version}")
}

/// Folder is a container of documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Folder {
    /// Unique folder identifier.
    pub id: String,
    /// Human-readable folder name.
    pub name: String,
    /// Parent folder identifier; omitted from JSON when empty (root folder).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    /// Creation timestamp (UTC).
    pub created_at: DateTime<Utc>,
}

impl Default for Folder {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            parent_id: String::new(),
            created_at: zero_time(),
        }
    }
}

/// ContentStore is the binary-content port — abstracted so the same
/// document service can swap between local-disk, S3, or Azure Blob.
#[async_trait]
pub trait ContentStore: Send + Sync {
    /// Stores the content under `key`, returning the number of bytes written.
    async fn put(&self, key: &str, content: ContentReader) -> Result<i64, EcmError>;
    /// Opens the content stored under `key`; [`EcmError::NotFound`] when absent.
    async fn get(&self, key: &str) -> Result<ContentReader, EcmError>;
    /// Removes the content stored under `key`; deleting a missing key is not
    /// an error.
    async fn delete(&self, key: &str) -> Result<(), EcmError>;
    /// Human-readable store identifier (e.g. `local-fs`, `aws-s3`).
    fn name(&self) -> &str;
}

/// DocumentService is the document-record CRUD port.
#[async_trait]
pub trait DocumentService: Send + Sync {
    /// Persists `content` and registers `doc`, returning the stored record
    /// with identifier, size, timestamps, and version filled in.
    async fn create(&self, doc: Document, content: ContentReader) -> Result<Document, EcmError>;
    /// Returns the document record for `id`.
    async fn get(&self, id: &str) -> Result<Document, EcmError>;
    /// Opens the binary content of the document `id`.
    async fn read(&self, id: &str) -> Result<ContentReader, EcmError>;
    /// Removes both the record and the stored content of document `id`.
    async fn delete(&self, id: &str) -> Result<(), EcmError>;
}

/// MetadataStore is the document-record index port — the Rust analog of
/// pyfly's `MetadataStoragePort`. It persists [`Document`] records separately
/// from their binary content (which lives behind a [`ContentStore`]), so the
/// same [`crate::Service`] can pair an in-memory index with any blob backend.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Persists `doc` (insert or replace by id), returning the stored record.
    async fn save(&self, doc: Document) -> Result<Document, EcmError>;
    /// Returns the document record for `id`, or [`EcmError::NotFound`].
    async fn get(&self, id: &str) -> Result<Document, EcmError>;
    /// Lists stored documents, optionally filtered to a `folder_id`
    /// (`None` returns every folder), capped at `limit` records. Mirrors
    /// pyfly's `MetadataStoragePort.list(folder_id, *, limit=100)`.
    async fn list(&self, folder_id: Option<&str>, limit: usize) -> Result<Vec<Document>, EcmError>;
    /// Removes the record `id`; returns `true` when a record was removed,
    /// `false` when it was already absent (pyfly's bool-returning delete).
    async fn delete(&self, id: &str) -> Result<bool, EcmError>;
}

/// FolderRepository is the folder-record port — the Rust analog of pyfly's
/// `FolderRepositoryPort`. Folders are containers of documents; this port
/// manages their lifecycle independently of document content.
#[async_trait]
pub trait FolderRepository: Send + Sync {
    /// Persists `folder` (insert or replace by id), returning the stored record.
    async fn save(&self, folder: Folder) -> Result<Folder, EcmError>;
    /// Returns the folder record for `id`, or [`EcmError::NotFound`].
    async fn get(&self, id: &str) -> Result<Folder, EcmError>;
    /// Lists folders whose `parent_id` equals `parent_id` (`None` returns the
    /// root folders, whose parent is empty). Mirrors pyfly's
    /// `FolderRepositoryPort.list(parent_id)`.
    async fn list(&self, parent_id: Option<&str>) -> Result<Vec<Folder>, EcmError>;
    /// Removes the folder `id`; returns `true` when a record was removed,
    /// `false` when it was already absent (pyfly's bool-returning delete).
    async fn delete(&self, id: &str) -> Result<bool, EcmError>;
}

/// SignatureRequest is the universal e-signature creation envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignatureRequest {
    /// Identifier of the document to be signed.
    pub document_id: String,
    /// Signer email addresses. A JSON `null` (Go's nil-slice encoding)
    /// deserializes as the empty list.
    #[serde(deserialize_with = "null_to_default")]
    pub signers: Vec<String>,
    /// Human-readable envelope title.
    pub title: String,
    /// Target provider: `docusign` | `adobesign` | `logalty`.
    pub provider: String,
}

/// SignatureStatus enumerates the canonical states a signature flow
/// transitions through. Wire-compatible with the Java/.NET/Go/Python ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignatureStatus {
    /// Awaiting one or more signers.
    Pending,
    /// All signers have signed.
    Signed,
    /// A signer declined the request.
    Declined,
    /// The request expired before completion.
    Expired,
}

impl SignatureStatus {
    /// Returns the canonical wire string (`pending`, `signed`, `declined`,
    /// `expired`).
    pub fn as_str(&self) -> &'static str {
        match self {
            SignatureStatus::Pending => "pending",
            SignatureStatus::Signed => "signed",
            SignatureStatus::Declined => "declined",
            SignatureStatus::Expired => "expired",
        }
    }
}

impl std::fmt::Display for SignatureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// ESignatureProvider is the e-signature port.
#[async_trait]
pub trait ESignatureProvider: Send + Sync {
    /// Creates a signature flow and returns its provider-side identifier.
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError>;
    /// Returns the current status of the signature flow `id`.
    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError>;
    /// Cancels the signature flow `id`.
    async fn cancel(&self, id: &str) -> Result<(), EcmError>;
    /// Human-readable provider identifier (e.g. `docusign`).
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;

    // ---------------------------------------------------------------------
    // Sentinel parity: ecm.ErrNotFound renders bytes-equal across runtimes.
    // ---------------------------------------------------------------------

    #[test]
    fn not_found_message_matches_go() {
        assert_eq!(EcmError::NotFound.to_string(), "firefly/ecm: not found");
        assert!(EcmError::NotFound.is_not_found());
    }

    #[test]
    fn provider_error_renders_message_verbatim() {
        let e = EcmError::provider("firefly/ecmstorageaws: not yet implemented");
        assert_eq!(e.to_string(), "firefly/ecmstorageaws: not yet implemented");
        assert!(!e.is_not_found());
    }

    #[test]
    fn io_error_converts_and_is_not_the_sentinel() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let e = EcmError::from(io);
        assert!(matches!(e, EcmError::Io(_)));
        assert_eq!(e.to_string(), "firefly/ecm: io: denied");
        assert!(!e.is_not_found());
    }

    // ---------------------------------------------------------------------
    // Wire-shape parity with the Go port's encoding/json tags.
    // ---------------------------------------------------------------------

    fn sample_document() -> Document {
        let mut metadata = serde_json::Map::new();
        metadata.insert("dept".to_string(), serde_json::json!("eng"));
        Document {
            id: "d1".into(),
            folder_id: "f1".into(),
            name: "spec.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 1024,
            tags: vec!["legal".into(), "draft".into()],
            metadata,
            created_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
            version: 3,
        }
    }

    #[test]
    fn document_json_wire_shape_matches_go() {
        let got = serde_json::to_string(&sample_document()).unwrap();
        let want = r#"{"id":"d1","folderId":"f1","name":"spec.pdf","mimeType":"application/pdf","size":1024,"tags":["legal","draft"],"metadata":{"dept":"eng"},"createdAt":"2025-01-02T03:04:05Z","updatedAt":"2025-01-02T03:04:05Z","version":3}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn document_json_omits_empty_optionals() {
        // folderId, tags, and metadata carry omitempty in the Go port; the
        // remaining fields always serialize, zero values included.
        let got = serde_json::to_string(&Document::default()).unwrap();
        let want = r#"{"id":"","name":"","mimeType":"","size":0,"createdAt":"0001-01-01T00:00:00Z","updatedAt":"0001-01-01T00:00:00Z","version":0}"#;
        assert_eq!(got, want);
    }

    #[test]
    fn document_round_trip() {
        let d = sample_document();
        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn document_deserialize_tolerates_missing_fields() {
        // Go's encoding/json leaves missing fields at their zero values.
        let d: Document = serde_json::from_str("{}").unwrap();
        assert_eq!(d, Document::default());
        assert_eq!(d.created_at, zero_time());
    }

    #[test]
    fn folder_json_wire_shape_matches_go() {
        let f = Folder {
            id: "f1".into(),
            name: "contracts".into(),
            parent_id: "root".into(),
            created_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
        };
        let got = serde_json::to_string(&f).unwrap();
        let want = r#"{"id":"f1","name":"contracts","parentId":"root","createdAt":"2025-01-02T03:04:05Z"}"#;
        assert_eq!(got, want);

        let back: Folder = serde_json::from_str(&got).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn folder_json_omits_empty_parent() {
        let got = serde_json::to_string(&Folder::default()).unwrap();
        assert_eq!(
            got,
            r#"{"id":"","name":"","createdAt":"0001-01-01T00:00:00Z"}"#
        );

        let back: Folder = serde_json::from_str("{}").unwrap();
        assert_eq!(back, Folder::default());
    }

    // ---------------------------------------------------------------------
    // DocumentVersion (pyfly parity) wire shape and version-key scheme.
    // ---------------------------------------------------------------------

    #[test]
    fn document_version_json_wire_shape() {
        let v = DocumentVersion {
            version: 2,
            content_hash: "abc123".into(),
            size_bytes: 64,
            storage_uri: "d1/v2".into(),
            created_at: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
        };
        let got = serde_json::to_string(&v).unwrap();
        let want = r#"{"version":2,"contentHash":"abc123","sizeBytes":64,"storageUri":"d1/v2","createdAt":"2025-01-02T03:04:05Z"}"#;
        assert_eq!(got, want);
        let back: DocumentVersion = serde_json::from_str(&got).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn document_version_default_and_missing_fields() {
        let got = serde_json::to_string(&DocumentVersion::default()).unwrap();
        assert_eq!(
            got,
            r#"{"version":0,"contentHash":"","sizeBytes":0,"storageUri":"","createdAt":"0001-01-01T00:00:00Z"}"#
        );
        let back: DocumentVersion = serde_json::from_str("{}").unwrap();
        assert_eq!(back, DocumentVersion::default());
    }

    #[test]
    fn version_key_is_flat_and_collision_free() {
        // Flat `<id>__v<n>` so it never clashes with the bare `<id>` primary
        // blob key on a directory-backed store.
        assert_eq!(version_key("d1", 1), "d1__v1");
        assert_eq!(version_key("doc-42", 7), "doc-42__v7");
        // The version key shares no path prefix with the bare document key.
        assert!(!version_key("d1", 1).starts_with("d1/"));
    }

    #[test]
    fn signature_request_json_wire_shape_matches_go() {
        let req = SignatureRequest {
            document_id: "d1".into(),
            signers: vec!["a@example.com".into(), "b@example.com".into()],
            title: "NDA".into(),
            provider: "docusign".into(),
        };
        let got = serde_json::to_string(&req).unwrap();
        let want = r#"{"documentId":"d1","signers":["a@example.com","b@example.com"],"title":"NDA","provider":"docusign"}"#;
        assert_eq!(got, want);

        let back: SignatureRequest = serde_json::from_str(&got).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn signature_request_always_emits_all_fields() {
        // No omitempty in the Go struct: empty fields stay on the wire.
        let got = serde_json::to_string(&SignatureRequest::default()).unwrap();
        assert_eq!(
            got,
            r#"{"documentId":"","signers":[],"title":"","provider":""}"#
        );
    }

    #[test]
    fn signature_request_tolerates_null_signers() {
        // A Go nil slice without omitempty serializes as JSON null.
        let req: SignatureRequest =
            serde_json::from_str(r#"{"documentId":"d1","signers":null,"title":"","provider":""}"#)
                .unwrap();
        assert_eq!(req.document_id, "d1");
        assert!(req.signers.is_empty());
    }

    #[test]
    fn signature_status_wire_values_match_go() {
        let cases = [
            (SignatureStatus::Pending, "pending"),
            (SignatureStatus::Signed, "signed"),
            (SignatureStatus::Declined, "declined"),
            (SignatureStatus::Expired, "expired"),
        ];
        for (status, want) in cases {
            assert_eq!(status.as_str(), want);
            assert_eq!(status.to_string(), want);
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{want}\"")
            );
            let back: SignatureStatus = serde_json::from_str(&format!("\"{want}\"")).unwrap();
            assert_eq!(back, status);
        }
    }

    // ---------------------------------------------------------------------
    // Checksum and reader helpers.
    // ---------------------------------------------------------------------

    #[test]
    fn sha256_hex_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"hello firefly"),
            "d4977b6f6f5bf0a0efcf2e979bd11e936ee0bc60f6c58613b7d47e24dc5b0ab2"
        );
    }

    #[tokio::test]
    async fn bytes_reader_round_trips() {
        let mut r = bytes_reader(b"hello firefly".to_vec());
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello firefly");
    }

    // ---------------------------------------------------------------------
    // Rust-specific: the e-signature port is object-safe behind Arc/Box,
    // standing in for the Go compile-time port assertions.
    // ---------------------------------------------------------------------

    /// Minimal in-memory provider standing in for a vendor adapter.
    struct StaticSigner;

    #[async_trait]
    impl ESignatureProvider for StaticSigner {
        async fn create(&self, req: SignatureRequest) -> Result<String, EcmError> {
            if req.document_id.is_empty() {
                return Err(EcmError::provider("static: missing documentId"));
            }
            Ok("env-1".to_string())
        }

        async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError> {
            if id == "env-1" {
                Ok(SignatureStatus::Pending)
            } else {
                Err(EcmError::NotFound)
            }
        }

        async fn cancel(&self, id: &str) -> Result<(), EcmError> {
            if id == "env-1" {
                Ok(())
            } else {
                Err(EcmError::NotFound)
            }
        }

        fn name(&self) -> &str {
            "static"
        }
    }

    #[tokio::test]
    async fn esignature_provider_usable_as_trait_object() {
        let signer: Arc<dyn ESignatureProvider> = Arc::new(StaticSigner);
        assert_eq!(signer.name(), "static");

        let id = signer
            .create(SignatureRequest {
                document_id: "d1".into(),
                signers: vec!["a@example.com".into()],
                title: "NDA".into(),
                provider: "static".into(),
            })
            .await
            .unwrap();
        assert_eq!(id, "env-1");
        assert_eq!(signer.status(&id).await.unwrap(), SignatureStatus::Pending);
        signer.cancel(&id).await.unwrap();
        assert!(signer.status("nope").await.unwrap_err().is_not_found());

        let err = signer
            .create(SignatureRequest::default())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "static: missing documentId");
    }

    #[test]
    fn port_types_are_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Document>();
        assert_send_sync::<DocumentVersion>();
        assert_send_sync::<Folder>();
        assert_send_sync::<SignatureRequest>();
        assert_send_sync::<SignatureStatus>();
        assert_send_sync::<EcmError>();
        // Content streams only need Send: they cross await points but are
        // owned by a single reader at a time, like Go's io.ReadCloser.
        assert_send::<ContentReader>();
        assert_send_sync::<Box<dyn ContentStore>>();
        assert_send_sync::<Arc<dyn DocumentService>>();
        assert_send_sync::<Box<dyn ESignatureProvider>>();
        assert_send_sync::<Arc<dyn MetadataStore>>();
        assert_send_sync::<Arc<dyn FolderRepository>>();
    }
}
