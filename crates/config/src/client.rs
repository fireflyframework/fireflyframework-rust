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

//! Spring-Cloud-Config **client** (pyfly `pyfly.config_server.client`
//! parity): fetches `/{application}/{profile}/{label}` from a remote config
//! server and flattens the document's `propertySources` into a flat
//! `key → value` map that slots into the [`Layered`](crate::Layered) chain
//! as a [`StaticSource`].
//!
//! Spring orders `propertySources` highest-priority **first**, so the
//! flattener applies them in reverse (lowest first) and lets later —
//! higher-priority — sources overwrite.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::error::ConfigError;
use crate::source::StaticSource;

/// Per-request timeout, matching pyfly's `httpx.AsyncClient(timeout=15.0)`.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Minimal HTTP client for a Spring-Cloud-Config-style server.
///
/// ```no_run
/// # async fn demo() -> Result<(), firefly_config::ConfigError> {
/// use firefly_config::{load, ConfigClient, Source};
/// # #[derive(serde::Deserialize)] struct AppCfg { }
///
/// let client = ConfigClient::new("http://config.internal:8888", "orders")
///     .with_profile("prod")
///     .with_label("main")
///     .with_basic_auth("user", "pass");
/// let remote = client.fetch_source().await?; // hard failure: fail-fast boot
/// let sources: Vec<Box<dyn Source>> = vec![Box::new(remote)];
/// let cfg: AppCfg = load(&sources)?;
/// # Ok(()) }
/// ```
///
/// For pyfly's non-fatal fallback (`_import_remote_config` logs a warning
/// and continues on local config), use
/// [`fetch_source_or_empty`](ConfigClient::fetch_source_or_empty).
#[derive(Debug, Clone)]
pub struct ConfigClient {
    url: String,
    application: String,
    profile: String,
    label: String,
    username: Option<String>,
    password: Option<String>,
    http: reqwest::Client,
}

impl ConfigClient {
    /// Returns a client for `url` (trailing slashes trimmed) and
    /// `application`, with `profile` defaulting to `"default"` and `label`
    /// to `"main"` — the pyfly constructor defaults.
    pub fn new(url: impl Into<String>, application: impl Into<String>) -> Self {
        let url: String = url.into();
        ConfigClient {
            url: url.trim_end_matches('/').to_string(),
            application: application.into(),
            profile: "default".to_string(),
            label: "main".to_string(),
            username: None,
            password: None,
            http: reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// Sets the profile segment of the document path (default `"default"`).
    #[must_use]
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        self.profile = profile.into();
        self
    }

    /// Sets the label segment (branch / tag) of the document path
    /// (default `"main"`).
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Enables HTTP basic authentication on every fetch.
    #[must_use]
    pub fn with_basic_auth(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    /// The document URL queried by [`fetch`](Self::fetch):
    /// `{url}/{application}/{profile}/{label}`.
    pub fn document_url(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.url, self.application, self.profile, self.label
        )
    }

    /// Fetches the document and flattens its `propertySources` into a flat
    /// `key → value` map (highest-priority source wins).
    ///
    /// Mirrors pyfly's contract exactly: a non-2xx response logs a warning
    /// and yields an **empty map** (soft miss), while transport or decode
    /// failures raise [`ConfigError::Remote`] so the caller can decide
    /// between fail-fast and fallback.
    pub async fn fetch(&self) -> Result<HashMap<String, String>, ConfigError> {
        let url = self.document_url();
        let mut request = self.http.get(&url);
        if let (Some(user), Some(pass)) = (&self.username, &self.password) {
            request = request.basic_auth(user, Some(pass));
        }
        let response = request.send().await.map_err(|err| ConfigError::Remote {
            url: url.clone(),
            message: err.to_string(),
        })?;
        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                application = %self.application,
                profile = %self.profile,
                label = %self.label,
                "config server returned non-success status"
            );
            return Ok(HashMap::new());
        }
        let document: RemoteDocument =
            response.json().await.map_err(|err| ConfigError::Remote {
                url: url.clone(),
                message: format!("invalid config document: {err}"),
            })?;

        // Spring orders propertySources HIGHEST priority first, so apply
        // them in reverse (lowest first) and let higher-priority sources
        // overwrite.
        let mut merged = HashMap::new();
        for source in document.property_sources.iter().rev() {
            for (key, value) in &source.source {
                merged.insert(key.clone(), stringify(value));
            }
        }
        Ok(merged)
    }

    /// [`fetch`](Self::fetch) bridged into the source chain: the flattened
    /// map wrapped as a [`StaticSource`] named
    /// `configserver({document_url})`. Errors propagate (fail-fast boot).
    pub async fn fetch_source(&self) -> Result<StaticSource, ConfigError> {
        Ok(StaticSource::new(self.source_name(), self.fetch().await?))
    }

    /// Non-fatal variant of [`fetch_source`](Self::fetch_source): on any
    /// failure it logs a warning and returns an **empty** source, so boot
    /// continues on local configuration — pyfly's
    /// `_import_remote_config` fallback behavior.
    pub async fn fetch_source_or_empty(&self) -> StaticSource {
        match self.fetch().await {
            Ok(map) => StaticSource::new(self.source_name(), map),
            Err(err) => {
                tracing::warn!(
                    url = %self.document_url(),
                    error = %err,
                    "remote config import failed; using local config only"
                );
                StaticSource::new(self.source_name(), HashMap::new())
            }
        }
    }

    fn source_name(&self) -> String {
        format!("configserver({})", self.document_url())
    }
}

/// The Spring Cloud Config wire document (only the fields the client needs).
#[derive(Debug, Deserialize)]
struct RemoteDocument {
    #[serde(default, rename = "propertySources")]
    property_sources: Vec<RemotePropertySource>,
}

#[derive(Debug, Deserialize)]
struct RemotePropertySource {
    #[serde(default)]
    source: serde_json::Map<String, serde_json::Value>,
}

/// Renders a JSON property value as the flat-map string the binder expects:
/// strings verbatim, scalars via their JSON rendering, `null` as `""`.
fn stringify(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_and_url_shape() {
        let client = ConfigClient::new("http://cfg:8888/", "orders");
        assert_eq!(client.document_url(), "http://cfg:8888/orders/default/main");
        let client = client.with_profile("prod").with_label("v2");
        assert_eq!(client.document_url(), "http://cfg:8888/orders/prod/v2");
    }

    #[test]
    fn stringify_renders_scalars_and_null() {
        assert_eq!(stringify(&serde_json::json!("x")), "x");
        assert_eq!(stringify(&serde_json::json!(8080)), "8080");
        assert_eq!(stringify(&serde_json::json!(true)), "true");
        assert_eq!(stringify(&serde_json::json!(null)), "");
        assert_eq!(stringify(&serde_json::json!([1, 2])), "[1,2]");
    }

    #[test]
    fn document_parses_spring_wire_format() {
        let json = r#"{
            "name": "orders", "profiles": ["prod"], "label": "main",
            "propertySources": [
                {"name": "high", "source": {"web.port": 9999}},
                {"name": "low", "source": {"web.port": 1111, "app.name": "orders"}}
            ]
        }"#;
        let doc: RemoteDocument = serde_json::from_str(json).unwrap();
        assert_eq!(doc.property_sources.len(), 2);
        let mut merged = HashMap::new();
        for source in doc.property_sources.iter().rev() {
            for (key, value) in &source.source {
                merged.insert(key.clone(), stringify(value));
            }
        }
        assert_eq!(merged["web.port"], "9999", "highest priority must win");
        assert_eq!(merged["app.name"], "orders");
    }
}
