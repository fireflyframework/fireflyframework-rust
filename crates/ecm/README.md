# `firefly-ecm`

> **Tier:** Adapter · **Status:** Full (port + LocalStore) · **Java original:** `firefly-ecm` · **Go module:** `ecm`

## Overview

`firefly-ecm` is the framework's **Enterprise Content Management**
abstraction. It defines four orthogonal ports — `Document`/`Folder`
models, `ContentStore`, `DocumentService`, `ESignatureProvider` — and
ships a default `LocalStore` (filesystem-backed `ContentStore` on
`tokio::fs`) plus an in-memory `Service` composing the two for tests
and single-instance deployments, with SHA-256 checksum support for
content-integrity verification.

Cloud storage adapters (`firefly-ecm-storage-aws`,
`firefly-ecm-storage-azure`) and e-signature provider adapters
(`firefly-ecm-esignature-docusign`, `firefly-ecm-esignature-adobe-sign`,
`firefly-ecm-esignature-logalty`) live in dedicated crates and ship as
port-asserting stubs.

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
