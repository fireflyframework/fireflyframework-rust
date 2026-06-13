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

//! `firefly license` — framework + dependency license report.
//!
//! Rust port of pyfly's `license.py`, extended with the dependency report the
//! gap analysis asked for. The command prints:
//!
//! 1. the framework's own license header (Apache-2.0, with the copyright line),
//! 2. the full `LICENSE` text when one is found by walking up the project tree
//!    (falling back to the canonical Apache-2.0 pointer when absent), and
//! 3. a third-party dependency report: every resolved crate from `Cargo.lock`
//!    with its version and origin (the SBOM inventory), so a reader can audit
//!    what the project pulls in. Cargo lockfiles do not record per-crate SPDX
//!    identifiers, so the report is the dependency *inventory* rather than a
//!    per-crate license string — a deliberate, documented divergence from a
//!    `cargo-license`-style scan that would require a heavier dependency.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::sbom::Sbom;

/// The framework's SPDX license identifier.
pub const LICENSE_ID: &str = "Apache-2.0";

/// The canonical pointer printed when no `LICENSE` file can be located.
const APACHE_POINTER: &str = "Licensed under the Apache License, Version 2.0.\n\
     You may obtain a copy of the License at:\n\n    \
     http://www.apache.org/licenses/LICENSE-2.0\n";

/// A rendered license report for the project the CLI runs inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseReport {
    /// The framework SPDX id (`Apache-2.0`).
    pub license: String,
    /// The full license text (from a found `LICENSE` file, or the pointer).
    pub text: String,
    /// `true` when the `text` came from a real `LICENSE` file on disk.
    pub from_file: bool,
    /// The third-party dependency inventory, sorted by crate name.
    pub dependencies: Vec<DependencyLicense>,
}

/// One dependency's license-report row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyLicense {
    /// Crate name.
    pub name: String,
    /// Resolved version.
    pub version: String,
    /// Origin (`crates.io`, a git URL, or `local`).
    pub source: String,
}

impl LicenseReport {
    /// Build the license report for the project rooted at (or above) `start`.
    pub fn collect(start: &Path) -> Self {
        let (text, from_file) = match load_license_file(start) {
            Some(text) => (text, true),
            None => (APACHE_POINTER.to_string(), false),
        };
        let dependencies = Sbom::collect(start)
            .dependencies
            .into_iter()
            .map(|e| DependencyLicense {
                name: e.name,
                version: e.version,
                source: e.source,
            })
            .collect();
        LicenseReport {
            license: LICENSE_ID.to_string(),
            text,
            from_file,
            dependencies,
        }
    }

    /// Render the full report as the text the `firefly license` command prints.
    ///
    /// The output is deterministic and pipeable: a framework header, the
    /// license body, then the dependency inventory with a trailing count.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Firefly Framework for Rust — License\n");
        let _ = writeln!(out, "Apache License, Version 2.0");
        let _ = writeln!(out, "Copyright 2026 Firefly Software Foundation.\n");
        let _ = writeln!(out, "{}", self.text.trim_end());

        let _ = writeln!(
            out,
            "\nThird-party dependencies ({}):",
            self.dependencies.len()
        );
        for dep in &self.dependencies {
            let _ = writeln!(out, "  {} {} [{}]", dep.name, dep.version, dep.source);
        }
        if self.dependencies.is_empty() {
            let _ = writeln!(
                out,
                "  (none resolved — run inside a project with a Cargo.lock for the dependency report)"
            );
        }
        let _ = writeln!(
            out,
            "\nNote: dependency licenses are governed by each crate's own SPDX \
             metadata on crates.io; this report lists the resolved inventory."
        );
        out
    }
}

/// Walk up from `start` for a `LICENSE` (or `LICENSE.txt`/`LICENSE.md`) file and
/// return its text, or `None` when none is found.
fn load_license_file(start: &Path) -> Option<String> {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current: Option<&Path> = Some(start.as_path());
    while let Some(dir) = current {
        for name in ["LICENSE", "LICENSE.txt", "LICENSE.md"] {
            let candidate: PathBuf = dir.join(name);
            if candidate.is_file() {
                if let Ok(text) = std::fs::read_to_string(&candidate) {
                    if !text.trim().is_empty() {
                        return Some(text);
                    }
                }
            }
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_against_workspace_finds_license_and_deps() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let report = LicenseReport::collect(manifest);
        assert_eq!(report.license, "Apache-2.0");
        // The workspace ships a LICENSE file at its root.
        assert!(report.from_file);
        assert!(report.text.contains("Apache License"));
        assert!(!report.dependencies.is_empty());
    }

    #[test]
    fn render_produces_non_empty_output() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = LicenseReport::collect(manifest).render();
        assert!(!text.is_empty());
        assert!(text.contains("Apache License, Version 2.0"));
        assert!(text.contains("Third-party dependencies"));
    }

    #[test]
    fn falls_back_to_pointer_without_license_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = LicenseReport::collect(tmp.path());
        assert!(!report.from_file);
        assert!(report.text.contains("apache.org/licenses/LICENSE-2.0"));
        // Render still works and notes the empty inventory.
        let text = report.render();
        assert!(text.contains("none resolved"));
    }

    #[test]
    fn finds_a_local_license_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("LICENSE"), "Apache License\nVersion 2.0\n").unwrap();
        let report = LicenseReport::collect(tmp.path());
        assert!(report.from_file);
        assert!(report.text.contains("Apache License"));
    }
}
