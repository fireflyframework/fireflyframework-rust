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

//! [`ConfigServer`] — composes [`ConfigBackend`] bundles into the
//! Spring-Cloud-Config overlay set.
//!
//! The Rust port of pyfly's `pyfly.config_server.server.ConfigServer`.
//! It is framework-agnostic: it produces (and round-trips) plain
//! [`Environment`] / [`ConfigSource`] values that
//! [`router`](crate::router) — or any HTTP layer — can serve.

use crate::backend::{BackendError, ConfigBackend, ConfigSource};
use crate::{Environment, PropertySource};

/// Composes a [`ConfigBackend`] into Spring-Cloud-Config payloads.
///
/// [`fetch`](ConfigServer::fetch) emits the full overlay set, highest
/// priority first: the requested `app+profile`, then the app's `default`
/// bundle, then the shared `application` config for the profile and its
/// default. A client merges these with the first winning. The result is
/// `None` only when **every** overlay is absent.
pub struct ConfigServer<B: ConfigBackend> {
    backend: B,
}

impl<B: ConfigBackend> ConfigServer<B> {
    /// Wraps `backend`.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Returns the wrapped backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Builds the overlay [`Environment`] for `(application, profile,
    /// label)`, or `None` when no overlay exists.
    ///
    /// The overlay candidates, highest precedence first, are:
    /// `(application, profile)`, `(application, "default")`,
    /// `("application", profile)`, `("application", "default")` — with
    /// duplicates collapsed (so `profile == "default"` does not query the
    /// same bundle twice).
    pub async fn fetch(
        &self,
        application: &str,
        profile: &str,
        label: &str,
    ) -> Result<Option<Environment>, BackendError> {
        let candidates = [
            (application, profile),
            (application, "default"),
            ("application", profile),
            ("application", "default"),
        ];
        let mut seen: Vec<(String, String)> = Vec::with_capacity(candidates.len());
        let mut sources: Vec<ConfigSource> = Vec::new();
        for (app_name, prof) in candidates {
            let key = (app_name.to_string(), prof.to_string());
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            if let Some(source) = self.backend.fetch(app_name, prof, label).await? {
                sources.push(source);
            }
        }

        if sources.is_empty() {
            return Ok(None);
        }

        let property_sources = sources
            .into_iter()
            .map(|s| PropertySource {
                name: format!("{}-{}", s.application, s.profile),
                // `Properties` (a `serde_json::Map`) may iterate in
                // insertion order when `serde_json/preserve_order` is on;
                // collecting into the field's `BTreeMap` restores the
                // sorted-key wire contract.
                source: s.properties.into_iter().collect(),
            })
            .collect();

        Ok(Some(Environment {
            name: application.to_string(),
            profiles: vec![profile.to_string()],
            label: label.to_string(),
            property_sources,
            ..Environment::default()
        }))
    }

    /// Persists `properties` under `(application, profile, label)` via the
    /// backend's [`save`](ConfigBackend::save) path.
    ///
    /// # Errors
    ///
    /// Propagates [`BackendError::Unsupported`] for read-only backends.
    pub async fn save(
        &self,
        application: &str,
        profile: &str,
        label: &str,
        properties: crate::backend::Properties,
    ) -> Result<(), BackendError> {
        self.backend
            .save(ConfigSource::with_label(
                application,
                profile,
                label,
                properties,
            ))
            .await
    }

    /// Lists the `(application, profile, label)` of every known bundle.
    pub async fn list(&self) -> Result<Vec<(String, String, String)>, BackendError> {
        Ok(self
            .backend
            .list()
            .await?
            .into_iter()
            .map(|s| (s.application, s.profile, s.label))
            .collect())
    }
}
