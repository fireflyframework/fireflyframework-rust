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

//! Configuration sources and the layered merge.
//!
//! A [`Source`] produces a flat `String → String` map of configuration
//! entries keyed by dot notation (`web.port`). [`Layered`] merges sources
//! left to right — **last write wins** — which gives the canonical
//! precedence chain: defaults → base YAML → profile YAML → environment →
//! CLI flags.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::ConfigError;

/// Anything producing a flat string→string map of configuration entries.
///
/// Keys use dot notation (`firefly.web.port`); the merge order is
/// determined by [`load`](crate::load) — later sources override earlier
/// ones. Implementations must be `Send + Sync` so a source list can be
/// shared with async tasks.
pub trait Source: Send + Sync {
    /// Self-reported name used in error messages (`yaml(<path>)`,
    /// `env(<PREFIX>)`, `flags`, …).
    fn name(&self) -> String;

    /// Produces this source's entries. Keys are lower-cased by the merge,
    /// so implementations may emit any case.
    fn load(&self) -> Result<HashMap<String, String>, ConfigError>;
}

/// Normalizes a key for relaxed (Spring Boot–style) matching: lower-cased
/// with kebab-case dashes folded to snake-case underscores, so a YAML key
/// `graceful-timeout` binds a `graceful_timeout` serde field (pyfly
/// `_relaxed` parity).
pub(crate) fn normalize_key(key: &str) -> String {
    key.to_lowercase().replace('-', "_")
}

/// Merges `sources` left to right (later wins), normalizing every key
/// (lower-case, `-` → `_`). Errors short-circuit and are wrapped with the
/// failing source's name.
pub(crate) fn merge(sources: &[Box<dyn Source>]) -> Result<HashMap<String, String>, ConfigError> {
    let mut out = HashMap::new();
    for source in sources {
        let entries = source.load().map_err(|err| ConfigError::Source {
            name: source.name(),
            source: Box::new(err),
        })?;
        for (key, value) in entries {
            out.insert(normalize_key(&key), value);
        }
    }
    Ok(out)
}

/// Combines sources right-to-precedence: later wins.
pub struct Layered {
    sources: Vec<Box<dyn Source>>,
}

impl Layered {
    /// Returns a [`Layered`] with the given sources.
    pub fn new(sources: Vec<Box<dyn Source>>) -> Self {
        Layered { sources }
    }

    /// Merges every source into a single flat map. Errors short-circuit.
    pub fn map(&self) -> Result<HashMap<String, String>, ConfigError> {
        merge(&self.sources)
    }

    /// The sources in merge order (earliest = lowest precedence).
    pub(crate) fn sources(&self) -> &[Box<dyn Source>] {
        &self.sources
    }
}

/// The simplest [`Source`] — wraps an in-memory map. Typically used for
/// hard-coded defaults at the bottom of the precedence chain.
#[derive(Debug, Clone, Default)]
pub struct StaticSource {
    /// Self-reported name (`"defaults"`, …).
    pub name: String,
    /// The hard-coded entries, keyed by dotted path.
    pub entries: HashMap<String, String>,
}

impl StaticSource {
    /// Returns a [`StaticSource`] with the given name and entries.
    pub fn new(name: impl Into<String>, entries: HashMap<String, String>) -> Self {
        StaticSource {
            name: name.into(),
            entries,
        }
    }
}

impl Source for StaticSource {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
        Ok(self.entries.clone())
    }
}

/// Reads environment variables with a given prefix, mapping
/// `<PREFIX>_FOO_BAR` → `foo.bar`.
#[derive(Debug, Clone)]
pub struct EnvSource {
    /// The variable prefix (e.g. `"FIREFLY"`), matched case-insensitively
    /// by upper-casing before comparison.
    pub prefix: String,
}

/// Returns an [`EnvSource`] with the given prefix (e.g. `"FIREFLY"`).
pub fn from_env(prefix: impl Into<String>) -> EnvSource {
    EnvSource {
        prefix: prefix.into(),
    }
}

impl Source for EnvSource {
    fn name(&self) -> String {
        format!("env({})", self.prefix)
    }

    fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
        let want = format!("{}_", self.prefix.to_uppercase());
        let mut out = HashMap::new();
        for (key, value) in std::env::vars_os() {
            let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
                continue; // skip non-UTF-8 entries, mirroring Go's string env
            };
            if let Some(rest) = key.strip_prefix(&want) {
                out.insert(rest.to_lowercase().replace('_', "."), value.to_string());
            }
        }
        Ok(out)
    }
}

/// Populated from CLI flag overrides — typically built by the
/// application's `main` via [`FlagSource::set`] before [`load`](crate::load)
/// runs. Cloning is cheap and clones share the same entry map, so the
/// application can keep a handle after boxing the source.
#[derive(Debug, Clone, Default)]
pub struct FlagSource {
    entries: Arc<Mutex<HashMap<String, String>>>,
}

impl FlagSource {
    /// Returns an empty [`FlagSource`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a key/value pair (last write wins). Keys are lower-cased.
    pub fn set(&self, key: &str, value: &str) {
        self.lock().insert(key.to_lowercase(), value.to_string());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Source for FlagSource {
    fn name(&self) -> String {
        "flags".to_string()
    }

    fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
        Ok(self.lock().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn merge_last_write_wins_and_lowercases_keys() {
        let sources: Vec<Box<dyn Source>> = vec![
            Box::new(StaticSource::new("a", entries(&[("WEB.PORT", "1")]))),
            Box::new(StaticSource::new("b", entries(&[("web.port", "2")]))),
        ];
        let flat = Layered::new(sources).map().unwrap();
        assert_eq!(flat.len(), 1);
        assert_eq!(flat["web.port"], "2");
    }

    // pyfly parity (audit #92): relaxed key normalization folds kebab-case
    // to snake_case at merge time, so `graceful-timeout:` in YAML lands on
    // the same flat key as a `graceful_timeout` serde field.
    #[test]
    fn merge_normalizes_kebab_keys_to_snake() {
        let sources: Vec<Box<dyn Source>> = vec![
            Box::new(StaticSource::new(
                "a",
                entries(&[("server.graceful-timeout", "30")]),
            )),
            Box::new(StaticSource::new(
                "b",
                entries(&[("server.graceful_timeout", "60")]),
            )),
        ];
        let flat = Layered::new(sources).map().unwrap();
        assert_eq!(flat.len(), 1, "kebab and snake forms must collide");
        assert_eq!(flat["server.graceful_timeout"], "60");
    }

    #[test]
    fn merge_wraps_source_errors_with_name() {
        struct Failing;
        impl Source for Failing {
            fn name(&self) -> String {
                "boom".to_string()
            }
            fn load(&self) -> Result<HashMap<String, String>, ConfigError> {
                Err(ConfigError::Message("nope".to_string()))
            }
        }
        let sources: Vec<Box<dyn Source>> = vec![Box::new(Failing)];
        let err = Layered::new(sources).map().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("config source \"boom\""), "got: {text}");
        assert!(text.contains("nope"), "got: {text}");
    }

    #[test]
    fn static_source_name_and_clone_semantics() {
        let source = StaticSource::new("default", entries(&[("k", "v")]));
        assert_eq!(Source::name(&source), "default");
        assert_eq!(source.load().unwrap()["k"], "v");
    }

    #[test]
    fn flag_source_lowercases_and_last_write_wins() {
        let flags = FlagSource::new();
        flags.set("Web.Port", "1");
        flags.set("web.port", "2");
        assert_eq!(flags.name(), "flags");
        let loaded = flags.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["web.port"], "2");
    }

    #[test]
    fn flag_source_clones_share_entries() {
        let flags = FlagSource::new();
        let other = flags.clone();
        other.set("web.port", "1234");
        assert_eq!(flags.load().unwrap()["web.port"], "1234");
    }

    #[test]
    fn env_source_name_keeps_prefix_as_given() {
        assert_eq!(from_env("Firefly").name(), "env(Firefly)");
    }
}
