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

//! Detect the current firefly-rust project's shape for code generators.
//!
//! Rust-adapted port of pyfly's `_project.py`. Whereas the Python detector keys
//! off `pyproject.toml` + a `src/<package>/` package directory, the Rust port
//! keys off `Cargo.toml` + a flat `src/` tree (Rust's idiomatic layout) plus the
//! `firefly.yaml` written by `firefly new`.

use std::path::{Path, PathBuf};

use crate::error::CliError;

/// Resolved layout of the project the generator runs against.
///
/// Field-compatible (in spirit) with pyfly's `ProjectInfo`. `package` is the
/// Cargo crate name; `src_dir` is the crate's `src/`; `tests_dir` is the
/// top-level `tests/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    /// Project root (the directory containing `Cargo.toml`).
    pub root: PathBuf,
    /// Cargo package name (from `[package] name = "..."`).
    pub package: String,
    /// Detected archetype (`core`, `web-api`, `web`, `hexagonal`, `library`, `cli`).
    pub archetype: String,
    /// The crate's `src/` directory.
    pub src_dir: PathBuf,
    /// The top-level `tests/` directory.
    pub tests_dir: PathBuf,
}

/// Feature flags derived from `firefly.yaml`, mirroring pyfly's `feature_flags`.
///
/// These drive `has_*` template conditionals so generated artifacts match the
/// project's configured stack (e.g. a data-aware entity vs a plain struct).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeatureFlags {
    /// `firefly.data.relational.enabled` is true.
    pub has_data: bool,
    /// `firefly.data.document.enabled` is true.
    pub has_mongodb: bool,
    /// `firefly.web` is enabled (REST/web archetype).
    pub has_web: bool,
}

/// Walk up from `start` looking for the directory containing `Cargo.toml`.
fn find_root(start: &Path) -> Result<PathBuf, CliError> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current: Option<&Path> = Some(start.as_path());
    while let Some(dir) = current {
        if dir.join("Cargo.toml").is_file() {
            return Ok(dir.to_path_buf());
        }
        current = dir.parent();
    }
    Err(CliError::ProjectNotFound(
        "No Cargo.toml found. Run 'firefly generate' inside a firefly-rust project.".to_string(),
    ))
}

/// Read and parse `firefly.yaml` from the project root, returning `None` on any
/// failure (missing file, malformed YAML) — exactly like pyfly's tolerant reader.
fn read_yaml(root: &Path) -> Option<serde_yaml::Value> {
    let path = root.join("firefly.yaml");
    let text = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str::<serde_yaml::Value>(&text).ok()
}

/// Extract the `[package] name = "..."` value from a `Cargo.toml`.
///
/// A deliberately tiny line scanner rather than a full TOML parse: it reads the
/// `name` key inside the first `[package]` table. This avoids pulling a TOML
/// dependency that is not in the workspace catalog while covering the layouts
/// `firefly new` and ordinary Cargo projects produce.
fn parse_cargo_package(cargo_toml: &str) -> Option<String> {
    let mut in_package = false;
    for raw_line in cargo_toml.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// Resolve the project package name from `firefly.yaml` (`firefly.app.module`)
/// then fall back to `Cargo.toml`'s `[package] name`.
fn detect_package(root: &Path, data: Option<&serde_yaml::Value>) -> Result<String, CliError> {
    if let Some(module) = data
        .and_then(|d| d.get("firefly"))
        .and_then(|f| f.get("app"))
        .and_then(|a| a.get("module"))
        .and_then(|m| m.as_str())
    {
        if !module.is_empty() {
            // A dotted/`::` module path collapses to its head segment.
            let head = module.split(['.', ':']).next().unwrap_or(module);
            return Ok(head.to_string());
        }
    }
    let cargo = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|_| CliError::ProjectNotFound("Could not read Cargo.toml.".to_string()))?;
    parse_cargo_package(&cargo).ok_or_else(|| {
        CliError::ProjectNotFound("Could not determine the Cargo package name.".to_string())
    })
}

/// Detect the archetype from `firefly.yaml` (`firefly.app.archetype`), then by
/// inspecting which subdirectories exist under `src/`.
fn detect_archetype(root: &Path, data: Option<&serde_yaml::Value>) -> String {
    if let Some(archetype) = data
        .and_then(|d| d.get("firefly"))
        .and_then(|f| f.get("app"))
        .and_then(|a| a.get("archetype"))
        .and_then(|x| x.as_str())
    {
        if !archetype.is_empty() {
            return archetype.to_string();
        }
    }
    let src = root.join("src");
    if src.join("domain").is_dir() {
        return "hexagonal".to_string();
    }
    if src.join("templates").is_dir() {
        return "web".to_string();
    }
    if src.join("controllers").is_dir() {
        return "web-api".to_string();
    }
    if src.join("commands").is_dir() {
        return "cli".to_string();
    }
    "core".to_string()
}

/// Resolve the current project's package, archetype, and directories.
///
/// `start` defaults to the current working directory when `None`.
///
/// # Errors
/// Returns [`CliError::ProjectNotFound`] when no `Cargo.toml` is found by
/// walking up from `start`, or when the package name cannot be determined.
pub fn detect_project(start: Option<&Path>) -> Result<ProjectInfo, CliError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let start = start.unwrap_or(cwd.as_path());
    let root = find_root(start)?;
    let data = read_yaml(&root);
    let package = detect_package(&root, data.as_ref())?;
    let archetype = detect_archetype(&root, data.as_ref());
    Ok(ProjectInfo {
        src_dir: root.join("src"),
        tests_dir: root.join("tests"),
        root,
        package,
        archetype,
    })
}

/// Derive `has_*` flags from `firefly.yaml` so generators match the stack.
///
/// Mirrors pyfly's `feature_flags`: reads `firefly.data.relational.enabled`,
/// `firefly.data.document.enabled`, and the presence of a `firefly.web` section.
pub fn feature_flags(info: &ProjectInfo) -> FeatureFlags {
    let data = read_yaml(&info.root);
    let firefly = data.as_ref().and_then(|d| d.get("firefly"));

    let bool_at = |section: &str, key: &str| -> bool {
        firefly
            .and_then(|f| f.get("data"))
            .and_then(|d| d.get(section))
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

    let has_web = firefly.and_then(|f| f.get("web")).is_some()
        || matches!(info.archetype.as_str(), "web" | "web-api" | "hexagonal");

    FeatureFlags {
        has_data: bool_at("relational", "enabled"),
        has_mongodb: bool_at("document", "enabled"),
        has_web,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn scaffold(root: &Path, package: &str, archetype: &str, data: bool) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        let mut yaml =
            format!("firefly:\n  app:\n    name: {package}\n    archetype: {archetype}\n");
        if data {
            yaml.push_str("  data:\n    relational:\n      enabled: true\n");
        }
        fs::write(root.join("firefly.yaml"), yaml).unwrap();
    }

    fn scaffold_no_archetype(root: &Path, package: &str, subdir: Option<&str>) {
        fs::create_dir_all(root.join("src")).unwrap();
        if let Some(sub) = subdir {
            fs::create_dir_all(root.join("src").join(sub)).unwrap();
        }
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{package}\"\n"),
        )
        .unwrap();
        fs::write(
            root.join("firefly.yaml"),
            format!("firefly:\n  app:\n    name: {package}\n"),
        )
        .unwrap();
    }

    #[test]
    fn detects_package_and_archetype() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path(), "shop", "web-api", false);
        let info = detect_project(Some(tmp.path())).unwrap();
        assert_eq!(info.package, "shop");
        assert_eq!(info.archetype, "web-api");
        assert_eq!(info.src_dir, info.root.join("src"));
        assert_eq!(info.tests_dir, info.root.join("tests"));
    }

    #[test]
    fn walks_up_to_find_root() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path(), "shop", "core", false);
        let nested = tmp.path().join("src");
        let info = detect_project(Some(&nested)).unwrap();
        assert_eq!(
            info.root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn raises_when_no_project() {
        let tmp = TempDir::new().unwrap();
        let err = detect_project(Some(tmp.path()));
        assert!(matches!(err, Err(CliError::ProjectNotFound(_))));
    }

    #[test]
    fn feature_flags_reads_yaml() {
        let tmp = TempDir::new().unwrap();
        scaffold(tmp.path(), "shop", "web-api", true);
        let info = detect_project(Some(tmp.path())).unwrap();
        let flags = feature_flags(&info);
        assert!(flags.has_data);
        assert!(!flags.has_mongodb);
    }

    #[test]
    fn infers_hexagonal_from_domain() {
        let tmp = TempDir::new().unwrap();
        scaffold_no_archetype(tmp.path(), "shop", Some("domain"));
        assert_eq!(
            detect_project(Some(tmp.path())).unwrap().archetype,
            "hexagonal"
        );
    }

    #[test]
    fn infers_web_from_templates() {
        let tmp = TempDir::new().unwrap();
        scaffold_no_archetype(tmp.path(), "shop", Some("templates"));
        assert_eq!(detect_project(Some(tmp.path())).unwrap().archetype, "web");
    }

    #[test]
    fn infers_web_api_from_controllers() {
        let tmp = TempDir::new().unwrap();
        scaffold_no_archetype(tmp.path(), "shop", Some("controllers"));
        assert_eq!(
            detect_project(Some(tmp.path())).unwrap().archetype,
            "web-api"
        );
    }

    #[test]
    fn infers_cli_from_commands() {
        let tmp = TempDir::new().unwrap();
        scaffold_no_archetype(tmp.path(), "shop", Some("commands"));
        assert_eq!(detect_project(Some(tmp.path())).unwrap().archetype, "cli");
    }

    #[test]
    fn defaults_to_core() {
        let tmp = TempDir::new().unwrap();
        scaffold_no_archetype(tmp.path(), "shop", None);
        assert_eq!(detect_project(Some(tmp.path())).unwrap().archetype, "core");
    }

    #[test]
    fn parses_cargo_package_name() {
        assert_eq!(
            parse_cargo_package("[package]\nname = \"my-svc\"\nversion = \"1\"\n").as_deref(),
            Some("my-svc")
        );
        // name outside [package] is ignored
        assert_eq!(parse_cargo_package("[dependencies]\nname = \"x\"\n"), None);
    }

    #[test]
    fn package_from_yaml_module_overrides_cargo() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"cargo_name\"\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("firefly.yaml"),
            "firefly:\n  app:\n    module: yaml_name::main\n",
        )
        .unwrap();
        let info = detect_project(Some(tmp.path())).unwrap();
        assert_eq!(info.package, "yaml_name");
    }
}
