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

//! The `firefly openapi` export command.
//!
//! Rust port of pyfly's `pyfly.cli.openapi` (`cli/openapi.py`). pyfly
//! **boots the application context** in-process, collects route metadata
//! from the registered controllers, and renders the live OpenAPI spec.
//!
//! A compiled Rust binary cannot boot an arbitrary application's router
//! (there is no DI container to introspect at runtime, and the routes
//! live in the consumer's own crate), so this port operates on the
//! **current project** instead: it derives document metadata
//! (`title` / `version` / `description`) from `firefly.yaml` and
//! `Cargo.toml`, then emits a **skeleton OpenAPI 3.1 document** built with
//! [`firefly_openapi::Builder`]. The skeleton has empty `paths`; the
//! intended workflow is that the developer wires real [`firefly_openapi::RouteDef`]s
//! into their app and serves them via `Builder::router()`, while this
//! command provides the document scaffold, the `info` block, and the
//! standard `ProblemDetail` component for tooling / CI checks.
//!
//! This is a documented divergence from pyfly: the **flags and wire shape**
//! (`--format json|yaml`, `-o/--output`, OpenAPI 3.1, `info`/`paths`/
//! `components`) match as closely as a compiled tool allows; what differs
//! is that the Rust command cannot enumerate live routes without running
//! the app.

use std::path::Path;

use firefly_openapi::{Builder, Info};

use crate::error::CliError;

/// Output format for `firefly openapi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenApiFormat {
    /// Pretty-printed JSON (the default, matching pyfly).
    Json,
    /// YAML.
    Yaml,
}

impl OpenApiFormat {
    /// Parse a `--format` value (`json` / `yaml`).
    pub fn parse(s: &str) -> Result<Self, CliError> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(OpenApiFormat::Json),
            "yaml" | "yml" => Ok(OpenApiFormat::Yaml),
            other => Err(CliError::InvalidName(format!(
                "unknown openapi format: {other} (expected json or yaml)"
            ))),
        }
    }
}

/// Document metadata for the generated skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecMeta {
    /// The document title (project name).
    pub title: String,
    /// The document version.
    pub version: String,
    /// Optional free-text description.
    pub description: String,
}

impl Default for SpecMeta {
    fn default() -> Self {
        Self {
            title: "Firefly".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
        }
    }
}

/// Derive [`SpecMeta`] from the current project, reading `firefly.yaml`
/// (`firefly.app.name` / `firefly.app.version` / `firefly.app.description`)
/// and falling back to `Cargo.toml`'s `[package]` name + version. Missing
/// inputs use the [`Default`] values, so the command always succeeds even
/// outside a project (mirroring pyfly's config-default reads).
pub fn meta_for_project(root: &Path) -> SpecMeta {
    let mut meta = SpecMeta::default();

    if let Some((name, version)) = cargo_name_version(&root.join("Cargo.toml")) {
        if !name.is_empty() {
            meta.title = name;
        }
        if !version.is_empty() {
            meta.version = version;
        }
    }

    if let Ok(text) = std::fs::read_to_string(root.join("firefly.yaml")) {
        if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            let app = value.get("firefly").and_then(|f| f.get("app"));
            if let Some(name) = app.and_then(|a| a.get("name")).and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    meta.title = name.to_string();
                }
            }
            if let Some(v) = app.and_then(|a| a.get("version")).and_then(|x| x.as_str()) {
                if !v.is_empty() {
                    meta.version = v.to_string();
                }
            }
            if let Some(d) = app
                .and_then(|a| a.get("description"))
                .and_then(|x| x.as_str())
            {
                meta.description = d.to_string();
            }
        }
    }

    meta
}

/// Parse `[package] name`/`version` from a `Cargo.toml` with a tiny line
/// scanner (no TOML dependency, matching `project.rs`).
fn cargo_name_version(cargo_toml: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(cargo_toml).ok()?;
    let mut in_package = false;
    let mut name = String::new();
    let mut version = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name") {
            if let Some(v) = scalar_value(rest) {
                name = v;
            }
        } else if let Some(rest) = line.strip_prefix("version") {
            if let Some(v) = scalar_value(rest) {
                version = v;
            }
        }
    }
    Some((name, version))
}

/// Extract a quoted scalar after a `key` prefix: ` = "value"`.
fn scalar_value(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let value = rest.trim().trim_matches('"').trim_matches('\'');
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Render the skeleton OpenAPI document for `meta` in `format`.
///
/// Built via [`firefly_openapi::Builder`], so the JSON shape is identical
/// to what a live app serves at `/openapi.json` (OpenAPI 3.1, an
/// always-present `ProblemDetail` component) — only with empty `paths`.
pub fn render_spec(meta: &SpecMeta, format: OpenApiFormat) -> Result<String, CliError> {
    let builder = Builder::new(Info {
        title: meta.title.clone(),
        version: meta.version.clone(),
        description: meta.description.clone(),
    });
    let doc = builder.build();
    match format {
        OpenApiFormat::Json => serde_json::to_string_pretty(&doc)
            .map_err(|e| CliError::Template(format!("openapi json serialize: {e}"))),
        OpenApiFormat::Yaml => serde_yaml::to_string(&doc)
            .map_err(|e| CliError::Template(format!("openapi yaml serialize: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn format_parse() {
        assert_eq!(OpenApiFormat::parse("json").unwrap(), OpenApiFormat::Json);
        assert_eq!(OpenApiFormat::parse("YAML").unwrap(), OpenApiFormat::Yaml);
        assert_eq!(OpenApiFormat::parse("yml").unwrap(), OpenApiFormat::Yaml);
        assert!(OpenApiFormat::parse("toml").is_err());
    }

    #[test]
    fn meta_defaults_when_no_project() {
        let tmp = TempDir::new().unwrap();
        let meta = meta_for_project(tmp.path());
        assert_eq!(meta, SpecMeta::default());
    }

    #[test]
    fn meta_from_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"my-svc\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        let meta = meta_for_project(tmp.path());
        assert_eq!(meta.title, "my-svc");
        assert_eq!(meta.version, "1.2.3");
    }

    #[test]
    fn meta_yaml_overrides_cargo() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"cargo-name\"\nversion = \"0.0.1\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("firefly.yaml"),
            "firefly:\n  app:\n    name: PrettyName\n    version: 9.9.9\n    description: A nice service\n",
        )
        .unwrap();
        let meta = meta_for_project(tmp.path());
        assert_eq!(meta.title, "PrettyName");
        assert_eq!(meta.version, "9.9.9");
        assert_eq!(meta.description, "A nice service");
    }

    #[test]
    fn render_json_is_openapi_31() {
        let meta = SpecMeta {
            title: "T".into(),
            version: "1".into(),
            description: String::new(),
        };
        let json = render_spec(&meta, OpenApiFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["openapi"], "3.1.0");
        assert_eq!(value["info"]["title"], "T");
        assert_eq!(value["info"]["version"], "1");
        assert!(value.get("paths").is_some());
        // The standard ProblemDetail component is always present.
        assert!(value["components"]["schemas"]
            .get("ProblemDetail")
            .is_some());
    }

    #[test]
    fn render_yaml_starts_with_openapi() {
        let meta = SpecMeta::default();
        let yaml = render_spec(&meta, OpenApiFormat::Yaml).unwrap();
        assert!(yaml.contains("openapi: 3.1.0"));
        // Round-trips back to the same document.
        let value: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(value["openapi"], "3.1.0");
    }
}
