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

//! Multi-instance server mode — the Rust rendering of pyfly's
//! `InstanceRegistry` / `InstanceInfo` / `StaticDiscovery`.
//!
//! In server mode the dashboard tracks a set of downstream application
//! instances: an [`InstanceRegistry`] (a concurrent map keyed by name) plus
//! its [`discover_static`](InstanceRegistry::discover_static) seeder (pyfly's
//! `StaticDiscovery`), which loads
//! [`InstanceConfig`](crate::InstanceConfig) entries at startup. The
//! `/admin/api/instances` route set lets instances register / deregister at
//! runtime — the server half of the client/server handshake.

use std::collections::BTreeMap;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::config::InstanceConfig;

/// One registered application instance — pyfly's `InstanceInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    /// Unique instance name (the registry key).
    pub name: String,
    /// Base URL of the instance, with any trailing slash stripped.
    pub url: String,
    /// Health status (`UNKNOWN` until a poll updates it).
    pub status: String,
    /// When the status was last refreshed, if ever.
    pub last_checked: Option<DateTime<Utc>>,
    /// Free-form metadata supplied at registration.
    pub metadata: BTreeMap<String, String>,
}

impl InstanceInfo {
    /// Builds a freshly-registered instance (`status = "UNKNOWN"`, never
    /// checked), normalising the URL like pyfly (`url.rstrip("/")`).
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into().trim_end_matches('/').to_string(),
            status: "UNKNOWN".to_string(),
            last_checked: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Serialises to pyfly's wire shape: `{name, url, status, last_checked,
    /// metadata}`.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "url": self.url,
            "status": self.status,
            "last_checked": self.last_checked.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            "metadata": self.metadata,
        })
    }
}

/// A concurrent registry of known application instances — pyfly's
/// `InstanceRegistry`. Backed by an `RwLock<BTreeMap>` so the
/// `/admin/api/instances` routes and the (future) health poller can share it
/// behind an `Arc`.
#[derive(Default)]
pub struct InstanceRegistry {
    instances: RwLock<BTreeMap<String, InstanceInfo>>,
}

impl InstanceRegistry {
    /// Returns an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or overwrites) an instance and returns the stored info.
    pub fn register(
        &self,
        name: impl Into<String>,
        url: impl Into<String>,
        metadata: BTreeMap<String, String>,
    ) -> InstanceInfo {
        let mut info = InstanceInfo::new(name, url);
        info.metadata = metadata;
        self.instances
            .write()
            .expect("instance registry lock poisoned")
            .insert(info.name.clone(), info.clone());
        info
    }

    /// Removes an instance by name. Returns `true` when it existed.
    pub fn deregister(&self, name: &str) -> bool {
        self.instances
            .write()
            .expect("instance registry lock poisoned")
            .remove(name)
            .is_some()
    }

    /// Every registered instance.
    pub fn instances(&self) -> Vec<InstanceInfo> {
        self.instances
            .read()
            .expect("instance registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Looks up an instance by name.
    pub fn get(&self, name: &str) -> Option<InstanceInfo> {
        self.instances
            .read()
            .expect("instance registry lock poisoned")
            .get(name)
            .cloned()
    }

    /// Updates an instance's status and stamps `last_checked` (pyfly's
    /// `update_status`). A no-op when no such instance exists.
    pub fn update_status(&self, name: &str, status: impl Into<String>) {
        if let Some(info) = self
            .instances
            .write()
            .expect("instance registry lock poisoned")
            .get_mut(name)
        {
            info.status = status.into();
            info.last_checked = Some(Utc::now());
        }
    }

    /// Number of registered instances.
    pub fn len(&self) -> usize {
        self.instances
            .read()
            .expect("instance registry lock poisoned")
            .len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `{"instances": […]}` body for `GET /admin/api/instances` (pyfly's
    /// `to_dict`).
    pub fn to_json(&self) -> Value {
        let instances: Vec<Value> = self.instances().iter().map(InstanceInfo::to_json).collect();
        serde_json::json!({ "instances": instances })
    }

    /// Seeds the registry from a static list of configured instances —
    /// pyfly's `StaticDiscovery`. Entries missing a `name` or `url` are
    /// skipped.
    pub fn discover_static(&self, configs: &[InstanceConfig]) {
        for entry in configs {
            if !entry.name.is_empty() && !entry.url.is_empty() {
                self.register(
                    entry.name.clone(),
                    entry.url.clone(),
                    entry.metadata.clone(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // pyfly: test_register_instance
    #[test]
    fn register_starts_unknown() {
        let registry = InstanceRegistry::new();
        registry.register("test-app", "http://localhost:8080", BTreeMap::new());
        let instances = registry.instances();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].name, "test-app");
        assert_eq!(instances[0].status, "UNKNOWN");
    }

    // pyfly: test_deregister_instance
    #[test]
    fn deregister_removes() {
        let registry = InstanceRegistry::new();
        registry.register("test-app", "http://localhost:8080", BTreeMap::new());
        assert!(registry.deregister("test-app"));
        assert!(!registry.deregister("test-app"));
        assert!(registry.is_empty());
    }

    // pyfly: test_update_status
    #[test]
    fn update_status_stamps_last_checked() {
        let registry = InstanceRegistry::new();
        registry.register("test-app", "http://localhost:8080", BTreeMap::new());
        registry.update_status("test-app", "UP");
        let info = registry.get("test-app").expect("registered");
        assert_eq!(info.status, "UP");
        assert!(info.last_checked.is_some());
    }

    // pyfly: test_get_instance_not_found
    #[test]
    fn get_missing_is_none() {
        let registry = InstanceRegistry::new();
        assert!(registry.get("nope").is_none());
    }

    // pyfly: test_to_dict
    #[test]
    fn to_json_lists_instances() {
        let registry = InstanceRegistry::new();
        registry.register("app1", "http://localhost:8080", BTreeMap::new());
        let data = registry.to_json();
        assert_eq!(data["instances"].as_array().unwrap().len(), 1);
        assert_eq!(data["instances"][0]["name"], "app1");
    }

    #[test]
    fn url_trailing_slash_stripped() {
        let registry = InstanceRegistry::new();
        let info = registry.register("a", "http://localhost:8080/", BTreeMap::new());
        assert_eq!(info.url, "http://localhost:8080");
    }

    // pyfly: test_discover_registers_instances
    #[test]
    fn static_discovery_registers() {
        let registry = InstanceRegistry::new();
        registry.discover_static(&[
            InstanceConfig {
                name: "app-1".into(),
                url: "http://localhost:8080".into(),
                metadata: BTreeMap::new(),
            },
            InstanceConfig {
                name: "app-2".into(),
                url: "http://localhost:8081".into(),
                metadata: BTreeMap::new(),
            },
        ]);
        assert_eq!(registry.len(), 2);
        assert!(registry.get("app-1").is_some());
        assert!(registry.get("app-2").is_some());
    }

    // pyfly: test_discover_skips_incomplete_entries
    #[test]
    fn static_discovery_skips_incomplete() {
        let registry = InstanceRegistry::new();
        registry.discover_static(&[
            InstanceConfig {
                name: "valid".into(),
                url: "http://localhost:8080".into(),
                metadata: BTreeMap::new(),
            },
            InstanceConfig {
                name: String::new(),
                url: "http://localhost:8081".into(),
                metadata: BTreeMap::new(),
            },
            InstanceConfig {
                name: "no-url".into(),
                url: String::new(),
                metadata: BTreeMap::new(),
            },
        ]);
        assert_eq!(registry.len(), 1);
        assert!(registry.get("valid").is_some());
    }
}
