# `firefly-ecm`

> **Tier:** Adapter · **Status:** Stable

## Overview

`firefly-ecm` is the framework's **Enterprise Content Management**
abstraction. It defines four orthogonal ports — `Document`/`Folder`
models, `ContentStore`, `DocumentService`, `ESignatureProvider` — and
ships a default `LocalStore` (filesystem-backed `ContentStore` on
`tokio::fs`) plus an in-memory `Service` composing the two for tests
and single-instance deployments, with SHA-256 checksum support for
content-integrity verification.

Cloud storage adapters ship real REST integrations
(`firefly-ecm-storage-aws` over S3 + a self-contained SigV4 signer;
`firefly-ecm-storage-azure` over Azure Blob + a Shared Key signer);
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

// Envelope metadata returned by `get`, with a per-signer breakdown.
// `signers` is omitted from JSON when empty.
pub struct ESignatureEnvelope {
    pub id: String,
    pub provider: String,                       // omitted when empty
    pub document_id: String,                    // omitted when empty
    pub status: SignatureStatus,
    pub provider_envelope_id: Option<String>,   // "providerEnvelopeId", omitted when None
    pub sent_at: Option<DateTime<Utc>>,         // "sentAt", omitted when None
    pub signed_at: Option<DateTime<Utc>>,       // "signedAt", omitted when None
    pub signers: Vec<SignerState>,              // omitted when empty (additive)
}
pub struct SignerState { pub email: String, pub status: SignatureStatus, pub signed_at: Option<DateTime<Utc>> }

#[async_trait]
pub trait ESignatureProvider: Send + Sync {
    async fn create(&self, req: SignatureRequest) -> Result<String, EcmError>;
    async fn status(&self, id: &str) -> Result<SignatureStatus, EcmError>;
    async fn cancel(&self, id: &str) -> Result<(), EcmError>;
    fn name(&self) -> &str;

    // Returns the full envelope metadata, not just the bare status. The default
    // body bridges to `status` (NotFound → Ok(None); other errors surface) and
    // synthesizes a minimal envelope, so adapters predating it keep compiling
    // and still answer `get`. Richer adapters (NoOpESignature, the DocuSign /
    // Adobe Sign / Logalty crates) override it to populate timestamps + signers.
    async fn get(&self, id: &str) -> Result<Option<ESignatureEnvelope>, EcmError>;
}

pub enum EcmError { NotFound, Io(std::io::Error), Provider(String) }
```

`ContentReader` is an async byte stream
(`Pin<Box<dyn AsyncRead + Send>>`); build one from in-memory bytes
with `bytes_reader`. The `EcmError::NotFound` sentinel renders as the
stable string `firefly/ecm: not found`, and the JSON wire shapes of
`Document`, `Folder`, `SignatureRequest`, and `SignatureStatus` use
stable, documented field names (`omitempty` semantics included) — so
SDKs can transparently swap stores and providers.

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
for `firefly_ecm_storage_aws::S3Store`.

## Versioning, folders, and metadata

On top of the core ports, the crate offers a richer ECM surface — document
versioning, folder hierarchies, and a separable metadata store:

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

// Metadata + folder ports.
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
pub struct InMemoryMetadataStore;     // in-memory metadata storage
pub struct InMemoryFolderRepository;  // in-memory folder repository

// Public no-op signer — signs immediately and stores full ESignatureEnvelope
// metadata, so `get` returns the full status + sent_at/signed_at + per-signer
// breakdown.
pub struct NoOpESignature; // name() == "noop"

// E-signature envelope metadata returned by ESignatureProvider::get, alongside
// the per-SignerState breakdown.
pub struct ESignatureEnvelope { /* id, provider, document_id, status, provider_envelope_id, sent_at, signed_at, signers */ }
pub struct SignerState { /* email, status, signed_at */ }
```

`ESignatureProvider::get` returns the full [`ESignatureEnvelope`]
rather than the bare `SignatureStatus` that
`status` returns — surfacing the envelope's provider, document, provider-side
id, `sent_at` / `signed_at` lifecycle timestamps, and per-signer breakdown.
It is a **default trait method** (bridging to `status`, mapping `NotFound`
to `Ok(None)`), so adapters predating it keep compiling and still answer
`get`; `NoOpESignature` and the DocuSign / Adobe Sign / Logalty crates can
override it to populate the rich shape.

The in-memory `Service` gains inherent methods for folders and versioning:

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

A `from_config` factory selects providers from config strings — a DI-free
autoconfiguration entry point:

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

Covers the create + get + read + delete lifecycle, file-backed read
after create, 404-after-delete, LocalStore put/get/delete with nested
keys, truncation, and idempotent deletes, checksum computation and
verification, JSON wire-shape pins for every model, sentinel-error
stability, and object-safety/Send+Sync guards.
