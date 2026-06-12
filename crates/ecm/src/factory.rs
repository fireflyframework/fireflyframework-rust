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

//! Config-driven provider selection — the DI-free analog of pyfly's
//! `EcmAutoConfiguration`.
//!
//! pyfly selects storage and e-signature adapters from YAML keys
//! (`pyfly.ecm.storage.provider` / `pyfly.ecm.esignature.provider`) inside an
//! `@auto_configuration` class. Rust has no DI container, so the equivalent is
//! an explicit factory: bind an [`EcmConfig`] (a `serde`-deserializable struct,
//! e.g. via `firefly-config`) and call [`from_config`], which maps the provider
//! strings to boxed trait objects.
//!
//! ```
//! use firefly_ecm::{from_config, EcmConfig};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), firefly_ecm::EcmError> {
//! let dir = tempfile::tempdir().unwrap();
//! let cfg = EcmConfig {
//!     storage: firefly_ecm::StorageConfig {
//!         provider: "local".into(),
//!         local_base_dir: Some(dir.path().display().to_string()),
//!     },
//!     esignature: firefly_ecm::EsignatureConfig { provider: "noop".into() },
//! };
//! let (store, signer) = from_config(&cfg)?;
//! assert_eq!(store.name(), "local-fs");
//! assert_eq!(signer.name(), "noop");
//! # Ok(())
//! # }
//! ```

use serde::Deserialize;

use crate::local::LocalStore;
use crate::noop::NoOpESignature;
use crate::ports::{ContentStore, ESignatureProvider, EcmError};

/// Storage-provider selection, mirroring pyfly's `pyfly.ecm.storage.*` keys.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct StorageConfig {
    /// `local` (default) | `s3`/`aws` | `azure`/`azure-blob`. Cloud providers
    /// require the dedicated `firefly-ecm-storage-*` crate; [`from_config`]
    /// returns an explanatory [`EcmError::Provider`] for them.
    #[serde(default = "default_storage_provider")]
    pub provider: String,
    /// Filesystem root for the `local` provider; a fresh temp directory is
    /// used when unset (matching pyfly's `tempfile.mkdtemp` default).
    #[serde(default)]
    pub local_base_dir: Option<String>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            provider: default_storage_provider(),
            local_base_dir: None,
        }
    }
}

fn default_storage_provider() -> String {
    "local".to_string()
}

/// E-signature-provider selection, mirroring pyfly's
/// `pyfly.ecm.esignature.provider` key.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EsignatureConfig {
    /// `noop` (default) | `docusign` | `adobe`/`adobe-sign` | `logalty`.
    /// Vendor providers require the dedicated `firefly-ecm-esignature-*`
    /// crate; [`from_config`] returns an explanatory [`EcmError::Provider`]
    /// for them so the caller constructs the REST client explicitly.
    #[serde(default = "default_esignature_provider")]
    pub provider: String,
}

impl Default for EsignatureConfig {
    fn default() -> Self {
        Self {
            provider: default_esignature_provider(),
        }
    }
}

fn default_esignature_provider() -> String {
    "noop".to_string()
}

/// Top-level ECM configuration, the analog of pyfly's `pyfly.ecm.*` tree.
/// `serde`-deserializable so it binds straight from `firefly-config`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct EcmConfig {
    /// Storage-provider selection.
    pub storage: StorageConfig,
    /// E-signature-provider selection.
    pub esignature: EsignatureConfig,
}

/// Selects a [`ContentStore`] and an [`ESignatureProvider`] from `cfg`,
/// returning them as boxed trait objects — the DI-free analog of pyfly's
/// `EcmAutoConfiguration.document_storage` / `.esignature_adapter` beans.
///
/// Self-contained providers (`local` storage, `noop` e-signature) are built
/// directly. Cloud-storage and vendor e-signature providers live in dedicated
/// adapter crates (`firefly-ecm-storage-aws`, `firefly-ecm-esignature-docusign`,
/// …); selecting one returns an [`EcmError::Provider`] explaining that the
/// caller must construct that adapter from its own crate and pass it in — the
/// adapters all implement these same ports, so they drop straight into a
/// [`crate::Service`].
#[allow(clippy::type_complexity)]
pub fn from_config(
    cfg: &EcmConfig,
) -> Result<(Box<dyn ContentStore>, Box<dyn ESignatureProvider>), EcmError> {
    let store = storage_from_config(&cfg.storage)?;
    let signer = esignature_from_config(&cfg.esignature)?;
    Ok((store, signer))
}

/// Builds just the [`ContentStore`] from a [`StorageConfig`].
pub(crate) fn storage_from_config(cfg: &StorageConfig) -> Result<Box<dyn ContentStore>, EcmError> {
    match cfg.provider.to_ascii_lowercase().as_str() {
        "local" | "" | "filesystem" => {
            let base = match &cfg.local_base_dir {
                Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
                _ => std::env::temp_dir().join(format!(
                    "firefly-ecm-{}",
                    uuid::Uuid::new_v4().simple()
                )),
            };
            Ok(Box::new(LocalStore::new(base)))
        }
        "s3" | "aws" => Err(EcmError::provider(
            "firefly/ecm: storage provider 's3' requires firefly-ecm-storage-aws — construct its Store and pass it to Service::new",
        )),
        "azure" | "azure-blob" | "azure_blob" => Err(EcmError::provider(
            "firefly/ecm: storage provider 'azure' requires firefly-ecm-storage-azure — construct its Store and pass it to Service::new",
        )),
        other => Err(EcmError::provider(format!(
            "firefly/ecm: unknown storage provider '{other}'"
        ))),
    }
}

/// Builds just the [`ESignatureProvider`] from an [`EsignatureConfig`].
pub(crate) fn esignature_from_config(
    cfg: &EsignatureConfig,
) -> Result<Box<dyn ESignatureProvider>, EcmError> {
    match cfg.provider.to_ascii_lowercase().as_str() {
        "noop" | "" => Ok(Box::new(NoOpESignature::new())),
        "docusign" => Err(EcmError::provider(
            "firefly/ecm: e-signature provider 'docusign' requires firefly-ecm-esignature-docusign — construct its Provider and pass it in",
        )),
        "adobe" | "adobe-sign" | "adobe_sign" => Err(EcmError::provider(
            "firefly/ecm: e-signature provider 'adobe' requires firefly-ecm-esignature-adobe-sign — construct its Provider and pass it in",
        )),
        "logalty" => Err(EcmError::provider(
            "firefly/ecm: e-signature provider 'logalty' requires firefly-ecm-esignature-logalty — construct its Provider and pass it in",
        )),
        other => Err(EcmError::provider(format!(
            "firefly/ecm: unknown e-signature provider '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_select_local_and_noop() {
        let cfg = EcmConfig::default();
        assert_eq!(cfg.storage.provider, "local");
        assert_eq!(cfg.esignature.provider, "noop");
        let (store, signer) = from_config(&cfg).unwrap();
        assert_eq!(store.name(), "local-fs");
        assert_eq!(signer.name(), "noop");
    }

    #[test]
    fn local_honors_base_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = StorageConfig {
            provider: "local".into(),
            local_base_dir: Some(dir.path().display().to_string()),
        };
        let store = storage_from_config(&cfg).unwrap();
        assert_eq!(store.name(), "local-fs");
    }

    #[test]
    fn cloud_storage_providers_point_to_their_crates() {
        for p in ["s3", "AWS", "azure", "azure-blob"] {
            let cfg = StorageConfig {
                provider: p.into(),
                local_base_dir: None,
            };
            let err = storage_from_config(&cfg).err().unwrap();
            assert!(matches!(err, EcmError::Provider(_)), "{p}: {err}");
            assert!(
                err.to_string().contains("firefly-ecm-storage"),
                "{p}: {err}"
            );
        }
    }

    #[test]
    fn vendor_esignature_providers_point_to_their_crates() {
        for p in ["docusign", "adobe", "adobe-sign", "logalty"] {
            let cfg = EsignatureConfig { provider: p.into() };
            let err = esignature_from_config(&cfg).err().unwrap();
            assert!(
                err.to_string().contains("firefly-ecm-esignature"),
                "{p}: {err}"
            );
        }
    }

    #[test]
    fn unknown_providers_error() {
        let err = storage_from_config(&StorageConfig {
            provider: "gcs".into(),
            local_base_dir: None,
        })
        .err()
        .unwrap();
        assert_eq!(
            err.to_string(),
            "firefly/ecm: unknown storage provider 'gcs'"
        );

        let err = esignature_from_config(&EsignatureConfig {
            provider: "hellosign".into(),
        })
        .err()
        .unwrap();
        assert_eq!(
            err.to_string(),
            "firefly/ecm: unknown e-signature provider 'hellosign'"
        );
    }

    #[test]
    fn binds_from_serde_json() {
        // Demonstrates the serde wiring firefly-config relies on.
        let cfg: EcmConfig = serde_json::from_str(
            r#"{"storage":{"provider":"s3"},"esignature":{"provider":"docusign"}}"#,
        )
        .unwrap();
        assert_eq!(cfg.storage.provider, "s3");
        assert_eq!(cfg.esignature.provider, "docusign");

        // Empty object falls back to defaults.
        let cfg: EcmConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.storage.provider, "local");
        assert_eq!(cfg.esignature.provider, "noop");
    }
}
