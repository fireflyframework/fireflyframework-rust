//! Backend SPI + filesystem / in-memory / git adapters for the config server.
//!
//! This is the Rust port of pyfly's `pyfly.config_server.backend` and
//! `pyfly.config_server.adapters.git` modules. Where the Go-parity
//! [`Store`](crate::Store) trait answers Spring-Cloud-Config
//! `Environment` lookups directly, the pyfly-parity [`ConfigBackend`]
//! works one tier lower: it reads, writes, and lists individual
//! [`ConfigSource`] bundles keyed by `(application, profile, label)`.
//! [`crate::ConfigServer`] composes these bundles into the
//! Spring-Cloud-Config overlay set.
//!
//! Three adapters ship out of the box:
//!
//! * [`MemoryBackend`] — a `HashMap`-backed store, ideal for tests.
//! * [`FsStore`] — reads `<root>/<app>-<profile>.{yaml,yml,json}` (with
//!   an optional `<label>` sub-directory), and supports **tiered search
//!   locations** so a domain layer can override core which overrides
//!   common.
//! * [`GitStore`] — clones (or reuses) a Git working tree, then
//!   delegates to an [`FsStore`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A flat `property → value` map.
///
/// Matches the [`PropertySource::source`](crate::PropertySource) shape so
/// a [`ConfigSource`] composes directly into the
/// Spring-Cloud-Config wire format. A `BTreeMap` keeps keys in sorted
/// order — byte-for-byte identical to the Go encoder and to the existing
/// [`Environment`](crate::Environment) serialization.
pub type Properties = serde_json::Map<String, serde_json::Value>;

/// Errors surfaced by a [`ConfigBackend`] operation.
///
/// pyfly raises plain `ImportError` / OS errors; this enum is the typed
/// Rust counterpart. [`ConfigServer`](crate::ConfigServer) maps these to
/// the appropriate HTTP status when it is mounted on a router.
#[derive(Debug, Error)]
pub enum BackendError {
    /// A filesystem read/write failed.
    #[error("config-server io error: {0}")]
    Io(String),
    /// A config file could not be parsed as YAML or JSON.
    #[error("config-server parse error: {0}")]
    Parse(String),
    /// A `git` subprocess failed; the captured stderr is included.
    #[error("config-server git error: {0}")]
    Git(String),
    /// The operation is not supported by this backend.
    #[error("config-server: operation not supported: {0}")]
    Unsupported(String),
}

/// One config bundle keyed by application + profile + (optional) label.
///
/// The Rust port of pyfly's `ConfigSource` dataclass. `label` defaults
/// to `"main"` via [`ConfigSource::new`]; `properties` is a sorted map
/// so its serialization is deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSource {
    /// Application name (e.g. `"orders"`).
    pub application: String,
    /// Profile name (e.g. `"prod"`).
    pub profile: String,
    /// Source label (branch / tag); defaults to `"main"`.
    pub label: String,
    /// The flat property map for this bundle.
    pub properties: Properties,
}

impl ConfigSource {
    /// Builds a [`ConfigSource`] with the default `"main"` label.
    pub fn new(
        application: impl Into<String>,
        profile: impl Into<String>,
        properties: Properties,
    ) -> Self {
        Self {
            application: application.into(),
            profile: profile.into(),
            label: "main".to_string(),
            properties,
        }
    }

    /// Builds a [`ConfigSource`] with an explicit label.
    pub fn with_label(
        application: impl Into<String>,
        profile: impl Into<String>,
        label: impl Into<String>,
        properties: Properties,
    ) -> Self {
        Self {
            application: application.into(),
            profile: profile.into(),
            label: label.into(),
            properties,
        }
    }
}

/// The pyfly-parity backend SPI: fetch / save / list [`ConfigSource`]s.
///
/// This is the Rust port of pyfly's `ConfigBackend` `Protocol`. Unlike
/// the Go-parity [`Store`](crate::Store) trait — which answers fully
/// composed `Environment` lookups — a `ConfigBackend` works with raw,
/// per-`(app, profile, label)` bundles, leaving overlay composition to
/// [`ConfigServer`](crate::ConfigServer).
///
/// `save` defaults to [`BackendError::Unsupported`] so a read-only
/// backend need not implement a write path — the "optional save/write
/// path, default unsupported" contract.
#[async_trait]
pub trait ConfigBackend: Send + Sync {
    /// Returns the bundle for `(application, profile, label)`, or `None`
    /// when no file/entry matches (a *miss*, distinct from an error).
    async fn fetch(
        &self,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<ConfigSource>, BackendError>;

    /// Persists `source`. The default implementation rejects writes with
    /// [`BackendError::Unsupported`]; override it to enable saving.
    async fn save(&self, source: ConfigSource) -> Result<(), BackendError> {
        let _ = source;
        Err(BackendError::Unsupported("save".to_string()))
    }

    /// Enumerates every bundle the backend knows about.
    async fn list(&self) -> Result<Vec<ConfigSource>, BackendError>;
}

/// A `HashMap`-backed [`ConfigBackend`] — perfect for tests.
///
/// The Rust port of pyfly's `InMemoryConfigBackend`. A `Mutex` provides
/// the interior mutability the async trait needs while keeping the type
/// `Send + Sync`.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    store: Mutex<BTreeMap<(String, String, String), ConfigSource>>,
}

impl MemoryBackend {
    /// Returns an empty backend.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConfigBackend for MemoryBackend {
    async fn fetch(
        &self,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<ConfigSource>, BackendError> {
        let key = (
            application.to_string(),
            profile.to_string(),
            label.to_string(),
        );
        Ok(self
            .store
            .lock()
            .expect("MemoryBackend lock poisoned")
            .get(&key)
            .cloned())
    }

    async fn save(&self, source: ConfigSource) -> Result<(), BackendError> {
        let key = (
            source.application.clone(),
            source.profile.clone(),
            source.label.clone(),
        );
        self.store
            .lock()
            .expect("MemoryBackend lock poisoned")
            .insert(key, source);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ConfigSource>, BackendError> {
        Ok(self
            .store
            .lock()
            .expect("MemoryBackend lock poisoned")
            .values()
            .cloned()
            .collect())
    }
}

/// Loads config from `<root>/<application>-<profile>.{yaml,yml,json}`.
///
/// The Rust port of pyfly's `FilesystemConfigBackend`. The label maps to
/// a sub-directory: `<root>/<label>/<application>-<profile>.yaml`, with a
/// fall-back to the root for label-less layouts.
///
/// # Tiered search locations
///
/// Pass `search_locations` (a list of directory paths, **highest
/// precedence first**) to merge config from multiple directories. The
/// convention is:
///
/// ```text
/// search_locations = [domain_dir, core_dir, common_dir]
/// ```
///
/// so the domain layer overrides core which overrides common. Keys that
/// exist only in a lower-precedence location are inherited (fill-in
/// semantics). [`save`](FsStore::save) and [`list`](FsStore::list)
/// operate on the **primary** (first / highest-precedence) location; the
/// single-root behaviour is unchanged when `search_locations` is empty.
#[derive(Debug, Clone)]
pub struct FsStore {
    root: PathBuf,
    locations: Vec<PathBuf>,
}

impl FsStore {
    /// Opens (and creates) a single-root store at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Io`] if the root directory cannot be
    /// created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, BackendError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| BackendError::Io(e.to_string()))?;
        Ok(Self {
            root,
            locations: Vec::new(),
        })
    }

    /// Opens a tiered store: `root` is the primary location and
    /// `search_locations` lists the search tier **highest precedence
    /// first**. Each location directory is created if absent.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Io`] if any directory cannot be created.
    pub fn with_search_locations(
        root: impl Into<PathBuf>,
        search_locations: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, BackendError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| BackendError::Io(e.to_string()))?;
        let locations: Vec<PathBuf> = search_locations.into_iter().collect();
        for loc in &locations {
            std::fs::create_dir_all(loc).map_err(|e| BackendError::Io(e.to_string()))?;
        }
        Ok(Self { root, locations })
    }

    /// Returns the candidate file paths under an arbitrary `root`, in
    /// search order (label sub-directory first, then the root itself,
    /// each in `.yaml` → `.yml` → `.json` order).
    fn path_candidates_for(
        root: &Path,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Vec<PathBuf> {
        let stem = format!("{application}-{profile}");
        let base = if label.is_empty() {
            root.to_path_buf()
        } else {
            root.join(label)
        };
        let mut out = Vec::with_capacity(6);
        for ext in ["yaml", "yml", "json"] {
            out.push(base.join(format!("{stem}.{ext}")));
        }
        for ext in ["yaml", "yml", "json"] {
            out.push(root.join(format!("{stem}.{ext}")));
        }
        out
    }

    /// Attempts to read a single file match from `root`.
    fn fetch_from_root(
        root: &Path,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<ConfigSource>, BackendError> {
        for candidate in Self::path_candidates_for(root, application, profile, label) {
            if !candidate.is_file() {
                continue;
            }
            let text =
                std::fs::read_to_string(&candidate).map_err(|e| BackendError::Io(e.to_string()))?;
            let fmt = candidate
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            let properties = parse_text(&text, fmt)?;
            return Ok(Some(ConfigSource::with_label(
                application,
                profile,
                label,
                properties,
            )));
        }
        Ok(None)
    }
}

#[async_trait]
impl ConfigBackend for FsStore {
    async fn fetch(
        &self,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<ConfigSource>, BackendError> {
        if !self.locations.is_empty() {
            // Tiered mode: iterate locations from lowest to highest
            // precedence and accumulate, so higher-precedence locations
            // win on key collisions.
            let mut merged = Properties::new();
            let mut found = false;
            for loc in self.locations.iter().rev() {
                if let Some(source) = Self::fetch_from_root(loc, application, profile, label)? {
                    for (k, v) in source.properties {
                        merged.insert(k, v);
                    }
                    found = true;
                }
            }
            if !found {
                return Ok(None);
            }
            return Ok(Some(ConfigSource::with_label(
                application,
                profile,
                label,
                merged,
            )));
        }
        Self::fetch_from_root(&self.root, application, profile, label)
    }

    async fn save(&self, source: ConfigSource) -> Result<(), BackendError> {
        // Always write to the primary (highest-precedence) location.
        let candidates = Self::path_candidates_for(
            &self.root,
            &source.application,
            &source.profile,
            &source.label,
        );
        let existing: Vec<PathBuf> = candidates.iter().filter(|c| c.is_file()).cloned().collect();

        // Write back to the SAME file fetch() would read (highest-
        // priority existing candidate), preserving its format — otherwise
        // a save that wrote a fresh .json would be silently shadowed by a
        // pre-existing higher-priority .yaml. If none exists yet, create a
        // .json.
        let path = if let Some(first) = existing.first() {
            first.clone()
        } else {
            let target = if source.label.is_empty() {
                self.root.clone()
            } else {
                self.root.join(&source.label)
            };
            std::fs::create_dir_all(&target).map_err(|e| BackendError::Io(e.to_string()))?;
            target.join(format!("{}-{}.json", source.application, source.profile))
        };

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("json");
        let text = if ext == "yaml" || ext == "yml" {
            serde_yaml::to_string(&source.properties)
                .map_err(|e| BackendError::Parse(e.to_string()))?
        } else {
            serde_json::to_string_pretty(&source.properties)
                .map_err(|e| BackendError::Parse(e.to_string()))?
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| BackendError::Io(e.to_string()))?;
        }
        std::fs::write(&path, text).map_err(|e| BackendError::Io(e.to_string()))?;

        // Guarantee exactly one file backs this (app, profile, label) so
        // future fetches/saves can't diverge across stale duplicate
        // formats.
        for other in &existing {
            if other != &path {
                let _ = std::fs::remove_file(other);
            }
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ConfigSource>, BackendError> {
        // Always list from the primary location.
        let mut results = Vec::new();
        for path in walk_files(&self.root) {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if !matches!(ext, "yaml" | "yml" | "json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((application, profile)) = stem.split_once('-') else {
                continue;
            };
            let parent = path.parent();
            let label = match parent {
                Some(p) if p != self.root => p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("main")
                    .to_string(),
                _ => "main".to_string(),
            };
            if let Some(source) = self.fetch(application, profile, &label).await? {
                results.push(source);
            }
        }
        Ok(results)
    }
}

/// Recursively collects every file under `root`.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Parses a config file's text as JSON or YAML into a sorted property map.
///
/// `fmt` is the file extension (`"json"`, `"yaml"`, or `"yml"`); anything
/// other than `"json"` is treated as YAML, mirroring pyfly's `_parse_text`.
/// An empty YAML document parses to an empty map (pyfly's `yaml.safe_load`
/// returns `None` → `{}`).
fn parse_text(text: &str, fmt: &str) -> Result<Properties, BackendError> {
    if fmt == "json" {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| BackendError::Parse(e.to_string()))?;
        return value_to_properties(value);
    }
    // YAML (default). serde_yaml maps an empty document to `Null`.
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| BackendError::Parse(e.to_string()))?;
    if value.is_null() {
        return Ok(Properties::new());
    }
    let json: serde_json::Value =
        serde_json::to_value(value).map_err(|e| BackendError::Parse(e.to_string()))?;
    value_to_properties(json)
}

/// Coerces a parsed top-level JSON value into a property map, rejecting
/// non-object documents (a scalar or array is not a valid config bundle).
fn value_to_properties(value: serde_json::Value) -> Result<Properties, BackendError> {
    match value {
        serde_json::Value::Object(map) => Ok(map),
        serde_json::Value::Null => Ok(Properties::new()),
        other => Err(BackendError::Parse(format!(
            "expected a top-level object, got {other}"
        ))),
    }
}

mod git;
pub use git::GitStore;
