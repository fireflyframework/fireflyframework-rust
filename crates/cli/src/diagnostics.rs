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

//! `firefly info` and `firefly doctor` — environment diagnostics.
//!
//! Rust-adapted port of pyfly's `info.py` and `doctor.py`. Where pyfly probes
//! the Python interpreter and importable extras, the Rust port probes the
//! toolchain (`rustc`, `cargo`, `git`) and reports the build environment.

use std::process::Command;

/// One row of a diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The tool or property being reported.
    pub name: String,
    /// `true` when present/passing.
    pub ok: bool,
    /// A short detail (version string, description, or "not found").
    pub detail: String,
}

/// Facts about the firefly-rust project the CLI is being run inside, when one
/// is detected by walking up for a `Cargo.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFacts {
    /// The Cargo package name.
    pub package: String,
    /// The detected archetype (`core`, `web-api`, …).
    pub archetype: String,
    /// The project root (the directory containing `Cargo.toml`).
    pub root: String,
    /// Whether a `firefly.yaml` is present at the root.
    pub has_firefly_yaml: bool,
    /// Whether a `migrations/` directory is present.
    pub has_migrations: bool,
}

/// The result of a full `doctor` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// Required tools (a missing one fails the run).
    pub required: Vec<Check>,
    /// Optional tools (a missing one is merely noted).
    pub optional: Vec<Check>,
    /// The detected project, when the CLI runs inside one (`None` otherwise).
    pub project: Option<ProjectFacts>,
    /// `true` when every required tool was found.
    pub all_ok: bool,
}

/// The framework version this CLI was built from.
pub const FRAMEWORK_VERSION: &str = "26.7.0";

/// Run a `<tool> --version` and return its first trimmed output line, or `None`
/// when the tool is missing or errors.
fn tool_version(tool: &str) -> Option<String> {
    let output = Command::new(tool).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().next().map(|l| l.trim().to_string())
}

/// Required tools for a firefly-rust workspace, with descriptions.
const REQUIRED_TOOLS: &[(&str, &str)] = &[
    ("rustc", "Rust compiler"),
    ("cargo", "Rust package manager"),
];

/// Optional tools that enhance the workflow.
const OPTIONAL_TOOLS: &[(&str, &str)] = &[
    ("git", "Version control"),
    ("clippy-driver", "Linter (cargo clippy)"),
    ("rustfmt", "Formatter (cargo fmt)"),
    ("docker", "Container builds"),
];

/// Run the doctor checks and return a structured report.
///
/// This is pure data (no printing) so it can be asserted in tests; the binary
/// renders it. Mirrors pyfly's `doctor_command` check set, retargeted to Rust.
pub fn run_doctor() -> DoctorReport {
    let required: Vec<Check> = REQUIRED_TOOLS
        .iter()
        .map(|(tool, desc)| match tool_version(tool) {
            Some(v) => Check {
                name: (*tool).to_string(),
                ok: true,
                detail: v,
            },
            None => Check {
                name: (*tool).to_string(),
                ok: false,
                detail: format!("{desc} (not found)"),
            },
        })
        .collect();
    let optional: Vec<Check> = OPTIONAL_TOOLS
        .iter()
        .map(|(tool, desc)| match tool_version(tool) {
            Some(v) => Check {
                name: (*tool).to_string(),
                ok: true,
                detail: v,
            },
            None => Check {
                name: (*tool).to_string(),
                ok: false,
                detail: format!("{desc} (not found)"),
            },
        })
        .collect();
    let all_ok = required.iter().all(|c| c.ok);
    DoctorReport {
        required,
        optional,
        project: detect_project_facts(),
        all_ok,
    }
}

/// Detect the project the CLI is being run inside, returning its facts, or
/// `None` when there is no `Cargo.toml` up the tree.
fn detect_project_facts() -> Option<ProjectFacts> {
    let info = crate::project::detect_project(None).ok()?;
    Some(ProjectFacts {
        package: info.package,
        archetype: info.archetype,
        has_firefly_yaml: info.root.join("firefly.yaml").is_file(),
        has_migrations: info.root.join("migrations").is_dir(),
        root: info.root.display().to_string(),
    })
}

/// A single environment/info row for `firefly info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoRow {
    /// Property key.
    pub key: String,
    /// Property value.
    pub value: String,
}

/// Gather framework + environment info rows.
///
/// Mirrors pyfly's `info_command` environment table, with Rust-relevant keys.
pub fn run_info() -> Vec<InfoRow> {
    let row = |k: &str, v: String| InfoRow {
        key: k.to_string(),
        value: v,
    };
    let mut rows = vec![
        row(
            "Framework",
            format!("Firefly for Rust v{FRAMEWORK_VERSION}"),
        ),
        row("OS", std::env::consts::OS.to_string()),
        row("Architecture", std::env::consts::ARCH.to_string()),
        row(
            "rustc",
            tool_version("rustc").unwrap_or_else(|| "not found".to_string()),
        ),
        row(
            "cargo",
            tool_version("cargo").unwrap_or_else(|| "not found".to_string()),
        ),
    ];
    // When run inside a firefly-rust project, surface its identity too — the
    // Rust spelling of pyfly's `info` project section.
    if let Some(facts) = detect_project_facts() {
        rows.push(row("Project", facts.package));
        rows.push(row("Archetype", facts.archetype));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_reports_rustc_and_cargo() {
        // Running under cargo test guarantees the toolchain is present.
        let report = run_doctor();
        assert!(report.required.iter().any(|c| c.name == "rustc"));
        assert!(report.required.iter().any(|c| c.name == "cargo"));
        // The test runner implies cargo+rustc exist, so the run should pass.
        assert!(report.all_ok);
    }

    #[test]
    fn info_includes_framework_version() {
        let rows = run_info();
        let fw = rows.iter().find(|r| r.key == "Framework").unwrap();
        assert!(fw.value.contains(FRAMEWORK_VERSION));
        assert!(rows.iter().any(|r| r.key == "OS"));
        assert!(rows.iter().any(|r| r.key == "Architecture"));
    }

    #[test]
    fn doctor_reports_project_when_inside_one() {
        use std::fs;
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::create_dir_all(tmp.path().join("migrations")).unwrap();
        fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"acme\"\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("firefly.yaml"),
            "firefly:\n  app:\n    name: acme\n    archetype: web-api\n",
        )
        .unwrap();

        // Run the detection against the temp project explicitly (the public
        // `run_doctor` reads the CWD, which a test must not mutate globally).
        let info = crate::project::detect_project(Some(tmp.path())).unwrap();
        let facts = ProjectFacts {
            package: info.package.clone(),
            archetype: info.archetype.clone(),
            has_firefly_yaml: info.root.join("firefly.yaml").is_file(),
            has_migrations: info.root.join("migrations").is_dir(),
            root: info.root.display().to_string(),
        };
        assert_eq!(facts.package, "acme");
        assert_eq!(facts.archetype, "web-api");
        assert!(facts.has_firefly_yaml);
        assert!(facts.has_migrations);
    }

    #[test]
    fn doctor_report_has_project_field() {
        // The `project` field is always present in the report (Some when run
        // inside a package, None otherwise). Asserting it is reachable guards
        // the field against accidental removal without coupling the test to the
        // test runner's working directory.
        let report = run_doctor();
        let _ = &report.project;
    }
}
