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

//! Environment / configuration snapshot for the admin dashboard's
//! `/admin/api/env` + `/admin/api/config` panels — the Rust rendering of
//! Spring Boot Actuator's `/env` + `/configprops` (pyfly's
//! `EnvironmentProvider` / `ConfigPropsProvider`).
//!
//! The snapshot is built once at startup by the bootstrap (from
//! `firefly-config`'s `Layered::property_sources()` plus the active profiles)
//! and handed to [`AdminDeps`](crate::AdminDeps), so the panels render real,
//! masked, origin-attributed configuration instead of an empty stub. The field
//! shapes mirror `firefly_config`'s `PropertySourceView` / `PropertyView`
//! one-for-one, so the bootstrap conversion is a trivial field copy.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One masked property value plus the source it came from — Spring's `/env`
/// `{ "value": …, "origin": … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyEntry {
    /// The (already masked) property value.
    pub value: String,
    /// Origin attribution — the name of the source the value came from.
    pub origin: String,
}

/// One ordered property source (highest precedence first) — Spring's `/env`
/// `propertySources[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertySource {
    /// Source name (`systemEnvironment`, `yaml(<path>)`, `flags`, …).
    pub name: String,
    /// Masked properties keyed by dotted path (sorted for stable output).
    pub properties: BTreeMap<String, PropertyEntry>,
}

/// A point-in-time view of the resolved configuration: active profiles plus
/// the ordered, origin-attributed property sources. Built once at startup and
/// stored in [`AdminDeps::environment`](crate::AdminDeps::environment).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    /// Active Spring-style profiles (`demo`, `local`, …).
    pub active_profiles: Vec<String>,
    /// Ordered property sources, highest precedence first.
    pub property_sources: Vec<PropertySource>,
}

impl EnvironmentSnapshot {
    /// Builds a snapshot from its active profiles and ordered sources.
    pub fn new(active_profiles: Vec<String>, property_sources: Vec<PropertySource>) -> Self {
        Self {
            active_profiles,
            property_sources,
        }
    }

    /// Total property count across every source (each source's keys summed).
    pub fn property_count(&self) -> usize {
        self.property_sources
            .iter()
            .map(|s| s.properties.len())
            .sum()
    }

    /// The effective value per key, taking the highest-precedence source
    /// (sources are stored highest-precedence first), sorted by key.
    pub fn effective(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for source in &self.property_sources {
            for (key, entry) in &source.properties {
                out.entry(key.clone()).or_insert_with(|| entry.value.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str, pairs: &[(&str, &str, &str)]) -> PropertySource {
        PropertySource {
            name: name.to_string(),
            properties: pairs
                .iter()
                .map(|(k, v, o)| {
                    (
                        (*k).to_string(),
                        PropertyEntry {
                            value: (*v).to_string(),
                            origin: (*o).to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn property_count_sums_sources() {
        let snap = EnvironmentSnapshot::new(
            vec!["local".into()],
            vec![
                source("a", &[("x", "1", "a"), ("y", "2", "a")]),
                source("b", &[("z", "3", "b")]),
            ],
        );
        assert_eq!(snap.property_count(), 3);
    }

    #[test]
    fn effective_takes_highest_precedence() {
        let snap = EnvironmentSnapshot::new(
            Vec::new(),
            vec![
                source("overrides", &[("app.name", "svc", "overrides")]),
                source("defaults", &[("app.name", "default", "defaults")]),
            ],
        );
        // overrides is listed first (highest precedence) and wins.
        assert_eq!(snap.effective()["app.name"], "svc");
    }
}
