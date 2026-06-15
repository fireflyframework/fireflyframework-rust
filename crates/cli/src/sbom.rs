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

//! `firefly sbom [--json]` — Software Bill of Materials.
//!
//! Rust port of pyfly's `sbom.py`. Where pyfly walks `importlib.metadata` to
//! list the installed Python dependencies, the Rust port reads the resolved
//! dependency graph from the project's `Cargo.lock` (the source of truth Cargo
//! itself uses for reproducible builds). Each entry carries the crate name, the
//! exact resolved version, and the source (`crates.io`, a git URL, or `local`
//! for workspace/path crates), mirroring pyfly's name/required/installed table.
//!
//! The lockfile is parsed with a tiny hand-rolled scanner (no extra TOML
//! dependency, matching the approach in [`crate::project`]) so the command has
//! zero runtime cost beyond reading one file. When no `Cargo.lock` is found
//! (the CLI is run outside a project), the command reports an empty SBOM rather
//! than failing — the framework metadata header is still emitted.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One dependency in the Software Bill of Materials.
///
/// Field-compatible in spirit with pyfly's `(name, required, installed)` row:
/// `version` is the *resolved* (locked) version, and `source` records where the
/// crate came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SbomEntry {
    /// Crate name.
    pub name: String,
    /// The exact resolved version from `Cargo.lock`.
    pub version: String,
    /// Origin: `"crates.io"`, a `"git+<url>"` string, or `"local"` for
    /// workspace members and path dependencies (no `source` key in the lock).
    pub source: String,
}

/// A full Software Bill of Materials for the project the CLI runs inside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sbom {
    /// The framework name (`"firefly"`).
    pub name: String,
    /// The framework version this CLI was built from.
    pub version: String,
    /// The framework's SPDX license identifier.
    pub license: String,
    /// The resolved dependency entries, sorted by crate name then version.
    pub dependencies: Vec<SbomEntry>,
}

impl Sbom {
    /// Build the SBOM for the project rooted at (or above) `start`.
    ///
    /// Walks up from `start` for a `Cargo.lock` and parses its `[[package]]`
    /// stanzas. When no lockfile is found the dependency list is empty (the
    /// framework header is always present), so the command never fails just
    /// because it is run outside a Cargo project.
    pub fn collect(start: &Path) -> Self {
        let dependencies = match find_lockfile(start) {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => parse_lockfile(&text),
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        };
        Sbom {
            name: "firefly".to_string(),
            version: crate::VERSION.to_string(),
            license: "Apache-2.0".to_string(),
            dependencies,
        }
    }

    /// Serialize the SBOM as pretty-printed JSON (the `--json` output).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Walk up from `start` looking for a `Cargo.lock`.
fn find_lockfile(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current: Option<&Path> = Some(start.as_path());
    while let Some(dir) = current {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

/// Parse the `[[package]]` stanzas of a `Cargo.lock` into sorted SBOM entries.
///
/// Public so tests (and embedders) can feed lockfile text directly without
/// touching the filesystem. The scanner tracks the current `name`/`version`/
/// `source` and flushes an entry at each new `[[package]]` boundary and at EOF.
pub fn parse_lockfile(text: &str) -> Vec<SbomEntry> {
    let mut entries: Vec<SbomEntry> = Vec::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut source: Option<String> = None;
    let mut in_package = false;

    let flush = |entries: &mut Vec<SbomEntry>,
                 name: &mut Option<String>,
                 version: &mut Option<String>,
                 source: &mut Option<String>| {
        if let (Some(n), Some(v)) = (name.take(), version.take()) {
            entries.push(SbomEntry {
                name: n,
                version: v,
                source: normalize_source(source.take()),
            });
        } else {
            // Reset partial state even if incomplete.
            *name = None;
            *version = None;
            *source = None;
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line == "[[package]]" {
            flush(&mut entries, &mut name, &mut version, &mut source);
            in_package = true;
            continue;
        }
        if line.starts_with('[') {
            // A non-package table (e.g. `[[patch.unused]]`, `[metadata]`).
            flush(&mut entries, &mut name, &mut version, &mut source);
            in_package = false;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(v) = scan_string_value(line, "name") {
            name = Some(v);
        } else if let Some(v) = scan_string_value(line, "version") {
            version = Some(v);
        } else if let Some(v) = scan_string_value(line, "source") {
            source = Some(v);
        }
    }
    flush(&mut entries, &mut name, &mut version, &mut source);

    entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    entries
}

/// Extract the value of a `key = "value"` line, when it matches `key`.
fn scan_string_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let value = rest.trim().trim_matches('"');
    Some(value.to_string())
}

/// Normalize a lockfile `source` value into a friendly origin label.
///
/// `None` (no `source` key) means a workspace/path crate (`"local"`); the
/// crates.io registry URL collapses to `"crates.io"`; git sources keep their
/// `git+<url>` form for traceability.
fn normalize_source(source: Option<String>) -> String {
    match source {
        None => "local".to_string(),
        Some(s) if s.starts_with("registry+") => "crates.io".to_string(),
        Some(s) => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "firefly-cli"
version = "26.6.10"
dependencies = [
 "clap",
]

[[package]]
name = "aes-gcm"
version = "0.10.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "831010a0f742e1209b3bcea8fab6a8e149051ba6099432c8cb2cc117dec3ead1"
dependencies = [
 "aead",
]

[[package]]
name = "some-git-dep"
version = "1.2.3"
source = "git+https://github.com/example/repo?branch=main#abc123"

[metadata]
"#;

    #[test]
    fn parses_packages_with_source_classification() {
        let entries = parse_lockfile(SAMPLE);
        assert_eq!(entries.len(), 3);
        // Sorted by name: aes-gcm, firefly-cli, some-git-dep.
        assert_eq!(entries[0].name, "aes-gcm");
        assert_eq!(entries[0].version, "0.10.3");
        assert_eq!(entries[0].source, "crates.io");

        let firefly = entries.iter().find(|e| e.name == "firefly-cli").unwrap();
        assert_eq!(firefly.version, "26.6.10");
        assert_eq!(firefly.source, "local"); // no source key -> workspace member

        let git = entries.iter().find(|e| e.name == "some-git-dep").unwrap();
        assert!(git.source.starts_with("git+"));
    }

    #[test]
    fn empty_lockfile_yields_no_entries() {
        assert!(parse_lockfile("").is_empty());
        assert!(parse_lockfile("[metadata]\nfoo = 1\n").is_empty());
    }

    #[test]
    fn collect_against_the_workspace_has_dependencies() {
        // The crate is built inside the firefly workspace, so its Cargo.lock is
        // reachable from CARGO_MANIFEST_DIR and lists many crates.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sbom = Sbom::collect(manifest);
        assert_eq!(sbom.name, "firefly");
        assert_eq!(sbom.license, "Apache-2.0");
        assert!(
            !sbom.dependencies.is_empty(),
            "expected the workspace Cargo.lock to yield dependencies"
        );
        // firefly-cli itself must appear (it is a workspace member).
        assert!(sbom.dependencies.iter().any(|e| e.name == "firefly-cli"));
    }

    #[test]
    fn json_output_is_non_empty_and_parseable() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let json = Sbom::collect(manifest).to_json();
        assert!(!json.is_empty());
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "firefly");
        assert!(value["dependencies"].as_array().unwrap().len() > 1);
    }

    #[test]
    fn collect_outside_a_project_is_empty_but_valid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sbom = Sbom::collect(tmp.path());
        assert_eq!(sbom.name, "firefly");
        assert!(sbom.dependencies.is_empty());
        // JSON still renders.
        assert!(!sbom.to_json().is_empty());
    }
}
