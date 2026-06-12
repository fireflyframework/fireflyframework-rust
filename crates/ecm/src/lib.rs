//! firefly-ecm — the framework's Enterprise Content Management abstraction.
//!
//! Defines four orthogonal ports — [`Document`]/[`Folder`] models,
//! [`ContentStore`], [`DocumentService`], [`ESignatureProvider`] — and ships
//! a default [`LocalStore`] (filesystem-backed `ContentStore`) plus an
//! in-memory [`Service`] composing the two for tests and single-instance
//! deployments.
//!
//! Cloud storage adapters (`firefly-ecm-storage-aws`,
//! `firefly-ecm-storage-azure`) and e-signature provider adapters
//! (`firefly-ecm-esignature-docusign`, `firefly-ecm-esignature-adobe-sign`,
//! `firefly-ecm-esignature-logalty`) live in dedicated crates and ship as
//! port-asserting stubs.
//!
//! Faithful port of the Go module `fireflyframework-go/ecm`: the JSON wire
//! format of [`Document`], [`Folder`], [`SignatureRequest`], and
//! [`SignatureStatus`] matches the Go/Java/.NET/Python ports exactly.
//!
//! # Quick start
//!
//! ```
//! use firefly_ecm::{bytes_reader, Document, DocumentService, LocalStore, Service};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! # let dir = tempfile::tempdir().unwrap();
//! let svc = Service::new(LocalStore::new(dir.path()));
//!
//! let doc = svc
//!     .create(
//!         Document { name: "spec.pdf".into(), mime_type: "application/pdf".into(), ..Default::default() },
//!         bytes_reader(b"%PDF-1.7".to_vec()),
//!     )
//!     .await?;
//! println!("{} {}", doc.id, doc.size);
//!
//! let checksum = svc.checksum(&doc.id).await?;
//! assert!(svc.verify_checksum(&doc.id, &checksum).await?);
//!
//! svc.delete(&doc.id).await?;
//! # Ok(())
//! # }
//! ```

mod local;
mod ports;
mod service;

pub use local::LocalStore;
pub use ports::{
    bytes_reader, sha256_hex, ContentReader, ContentStore, Document, DocumentService,
    ESignatureProvider, EcmError, Folder, SignatureRequest, SignatureStatus,
};
pub use service::Service;

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
