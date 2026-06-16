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

//! firefly-ecm — the framework's Enterprise Content Management abstraction.
//!
//! Defines four orthogonal ports — [`Document`]/[`Folder`] models,
//! [`ContentStore`], [`DocumentService`], [`ESignatureProvider`] — and ships
//! a default [`LocalStore`] (filesystem-backed `ContentStore`) plus an
//! in-memory [`Service`] composing the two for tests and single-instance
//! deployments.
//!
//! Cloud storage adapters (`firefly-ecm-storage-aws`,
//! `firefly-ecm-storage-azure`) live in dedicated crates as port-asserting
//! stubs; e-signature provider adapters (`firefly-ecm-esignature-docusign`,
//! `firefly-ecm-esignature-adobe-sign`, `firefly-ecm-esignature-logalty`)
//! ship real REST integrations.
//!
//! # pyfly parity
//!
//! On top of the Go-parity core, this crate ports pyfly's ECM surface:
//! the [`DocumentVersion`] model and version-aware blob keys
//! ([`version_key`], [`Service::add_version`]), the [`MetadataStore`] and
//! [`FolderRepository`] ports with their [`InMemoryMetadataStore`] /
//! [`InMemoryFolderRepository`] adapters, document listing
//! ([`Service::list`]), folder management ([`Service::create_folder`]), a
//! public [`NoOpESignature`] provider (signs immediately — ideal for tests),
//! the [`ESignatureEnvelope`] / [`SignerState`] metadata returned by
//! [`ESignatureProvider::get`] (status + lifecycle timestamps + per-signer
//! breakdown — pyfly's `ESignatureEnvelope` dataclass and
//! `ESignatureAdapter.get`), and a [`from_config`] factory that selects
//! storage / e-signature providers from an [`EcmConfig`] — the DI-free
//! analog of pyfly's `EcmAutoConfiguration`.
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

mod factory;
mod in_memory;
mod local;
mod noop;
mod ports;
mod service;

pub use factory::{from_config, EcmConfig, EsignatureConfig, StorageConfig};
pub use in_memory::{InMemoryFolderRepository, InMemoryMetadataStore};
pub use local::LocalStore;
pub use noop::NoOpESignature;
pub use ports::{
    bytes_reader, sha256_hex, version_key, ContentReader, ContentStore, Document, DocumentService,
    DocumentVersion, ESignatureEnvelope, ESignatureProvider, EcmError, Folder, FolderRepository,
    MetadataStore, SignatureRequest, SignatureStatus, SignerState,
};
pub use service::Service;

/// Framework version stamp.
pub const VERSION: &str = "26.6.16";
