# `firefly-ecm`

> **Tier:** Adapter · **Status:** Full (port + LocalStore + pyfly parity) · **Java original:** `firefly-ecm` · **Go module:** `ecm`

## Overview

`firefly-ecm` is the framework's **Enterprise Content Management**
abstraction. It defines four orthogonal ports — `Document`/`Folder`
models, `ContentStore`, `DocumentService`, `ESignatureProvider` — and
ships a default `LocalStore` (filesystem-backed `ContentStore` on
`tokio::fs`) plus an in-memory `Service` composing the two for tests
and single-instance deployments, with SHA-256 checksum support for
content-integrity verification.

Cloud storage adapters (`firefly-ecm-storage-aws`,
`firefly-ecm-storage-azure`) live in dedicated crates as port-asserting stubs;
e-signature provider adapters (`firefly-ecm-esignature-docusign`,
`firefly-ecm-esignature-adobe-sign`, `firefly-ecm-esignature-logalty`) ship
real REST integrations.

## Public surface

```rust
pub struct Document {
    pub id: String,
    pub folder_id: String,        // "folderId", omitted when empty
    pub name: String,
    pub mime_type: String,        // "mimeType"
    pub size: i64,
    pub tags: Vec<String>,        // omitted when empty
    pub metadata: serde_json::Map<String, serde_json::Value>, // omitted when empty
    pub created_at: DateTime<Utc>, // "createdAt"
    pub updated_at: DateTime<Utc>, // "updatedAt"
    pub version: i64,
}

pub struct Folder { pub id: String, pub name: String, pub parent_id: String, pub created_at: DateTime<Utc> }

#[async_trait]
pub trait ContentStore: Send + Sync {
    async fn put(&self, key: &str, content: ContentReader) -> Result<i64, EcmError>;
    async fn get(&self, key: &str) -> Result<ContentReader, EcmError>;
    async fn delete(&self, key: &str) -> Result<(), EcmError>;
    fn name(&self) -> &str;
}

#[async_trait]
pub trait DocumentService: Send + Sync {
    async fn create(&self, doc: Document, content: ContentReader) -> Result<Document, EcmError>;
    async fn get(&self, id: &str) -> Result<Document, EcmError>;
    async fn read(&self, id: &str) -> Result<ContentReader, EcmError>;
    async fn delete(&self, id: &str) -> Result<(), EcmError>;
}

pub struct SignatureRequest { pub document_id: String, pub signers: Vec<String>, pub title: String, pub provider: String }
pub enum SignatureStatus { Pending, Signed, Declined, Expired } // "pending" | "signed" | "declined" | "expired"

#[async_trait]
pub trait ESignatureProvider: Send + Sync {
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError>;
    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError>;
    async fn cancel(&self, id: &str) -> Result<(), EcmError>;
    fn name(&self) -> &str;
}

pub enum EcmError { NotFound, Io(std::io::Error), Provider(String) }
```

`ContentReader` is the async analog of Go's `io.ReadCloser`
(`Pin<Box<dyn AsyncRead + Send>>`); build one from in-memory bytes
with `bytes_reader`. The `EcmError::NotFound` sentinel renders
bytes-equal to the Go port's `ecm.ErrNotFound`
(`firefly/ecm: not found`), and the JSON wire shapes of `Document`,
`Folder`, `SignatureRequest`, and `SignatureStatus` match the
Go/Java/.NET/Python ports exactly (`omitempty` semantics included) —
SDKs can transparently swap stores, providers, *and* runtimes.

### Default implementations

```rust
pub struct LocalStore { /* … */ }
impl LocalStore { pub fn new(root: impl Into<PathBuf>) -> Self } // filesystem-backed ContentStore ("local-fs")

pub struct Service { /* … */ }
impl Service {
    pub fn new(content: impl ContentStore + 'static) -> Self;    // in-memory document index
    pub async fn checksum(&self, id: &str) -> Result<String, EcmError>;            // SHA-256 hex
    pub async fn verify_checksum(&self, id: &str, expected: &str) -> Result<bool, EcmError>;
}
```

## Quick start

```rust
use firefly_ecm::{bytes_reader, Document, DocumentService, LocalStore, Service};

#[tokio::main]
async fn main() -> Result<(), firefly_ecm::EcmError> {
    let svc = Service::new(LocalStore::new("/var/firefly/docs"));

    let doc = svc
        .create(
            Document {
                name: "spec.pdf".into(),
                mime_type: "application/pdf".into(),
                ..Default::default()
            },
            bytes_reader(b"%PDF-1.7".to_vec()),
        )
        .await?;
    println!("{} {}", doc.id, doc.size);

    // Content-integrity verification.
    let checksum = svc.checksum(&doc.id).await?;
    assert!(svc.verify_checksum(&doc.id, &checksum).await?);

    svc.delete(&doc.id).await?;
    Ok(())
}
```

For S3-backed content storage in production, swap the `LocalStore`
for `firefly_ecm_storage_aws::Store` (once that adapter is wired).

## pyfly parity

On top of the Go-parity core, this crate ports pyfly's ECM surface:

```rust
// DocumentVersion model + version-aware blob keys.
pub struct DocumentVersion {
    pub version: i64,
    pub content_hash: String,   // "contentHash"
    pub size_bytes: i64,        // "sizeBytes"
    pub storage_uri: String,    // "storageUri"
    pub created_at: DateTime<Utc>, // "createdAt"
}
pub fn version_key(document_id: &str, version: i64) -> String; // "<id>__v<n>"

// Metadata + folder ports (pyfly MetadataStoragePort / FolderRepositoryPort).
#[async_trait]
pub trait MetadataStore: Send + Sync {
    async fn save(&self, doc: Document) -> Result<Document, EcmError>;
    async fn get(&self, id: &str) -> Result<Document, EcmError>;
    async fn list(&self, folder_id: Option<&str>, limit: usize) -> Result<Vec<Document>, EcmError>;
    async fn delete(&self, id: &str) -> Result<bool, EcmError>;
}
#[async_trait]
pub trait FolderRepository: Send + Sync {
    async fn save(&self, folder: Folder) -> Result<Folder, EcmError>;
    async fn get(&self, id: &str) -> Result<Folder, EcmError>;
    async fn list(&self, parent_id: Option<&str>) -> Result<Vec<Folder>, EcmError>;
    async fn delete(&self, id: &str) -> Result<bool, EcmError>;
}
pub struct InMemoryMetadataStore;     // pyfly InMemoryMetadataStorage
pub struct InMemoryFolderRepository;  // pyfly InMemoryFolderRepository

// Public no-op signer (pyfly NoOpESignatureAdapter) — signs immediately.
pub struct NoOpESignature; // name() == "noop"
```

The in-memory `Service` gains pyfly-parity inherent methods:

```rust
impl Service {
    pub fn with_folders(content: impl ContentStore + 'static, folders: impl FolderRepository + 'static) -> Self;
    pub async fn list(&self, folder_id: Option<&str>, limit: usize) -> Result<Vec<Document>, EcmError>;
    pub async fn create_folder(&self, folder: Folder) -> Result<Folder, EcmError>;
    // Multi-version blobs (stored at version_key(id, n)):
    pub async fn add_version(&self, id: &str, content: ContentReader) -> Result<DocumentVersion, EcmError>;
    pub async fn versions(&self, id: &str) -> Result<Vec<DocumentVersion>, EcmError>;
    pub async fn read_version(&self, id: &str, version: i64) -> Result<ContentReader, EcmError>;
    pub async fn delete_version(&self, id: &str, version: i64) -> Result<(), EcmError>;
}
```

A `from_config` factory selects providers from config strings — the DI-free
analog of pyfly's `EcmAutoConfiguration`:

```rust
pub struct EcmConfig { pub storage: StorageConfig, pub esignature: EsignatureConfig } // serde Deserialize
pub fn from_config(cfg: &EcmConfig) -> Result<(Box<dyn ContentStore>, Box<dyn ESignatureProvider>), EcmError>;
```

`storage.provider` accepts `local` (default) / `s3`/`aws` / `azure`;
`esignature.provider` accepts `noop` (default) / `docusign` / `adobe` /
`logalty`. Self-contained providers (`local`, `noop`) are built directly;
cloud/vendor providers return an `EcmError::Provider` pointing the caller to the
dedicated adapter crate (which implements the same ports and drops straight into
`Service`). `EcmConfig` is `serde`-deserializable, so it binds straight from
`firefly-config`.

## Testing

```bash
cargo test -p firefly-ecm
```

Covers the create + get + read + delete lifecycle (the Go
`TestServiceCRUD` contract), file-backed read after create,
404-after-delete, LocalStore put/get/delete with nested keys,
truncation, and idempotent deletes, checksum computation and
verification, JSON wire-shape pins for every model, sentinel-error
parity, and object-safety/Send+Sync guards.
